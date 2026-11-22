//! The availability API (ADR-0155): exact numbers from incidents, each
//! customer's report showing only its own, and the CSV file. Ignored
//! without `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{DateTime, TimeDelta, Utc};
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";

macro_rules! db_tests {
    ($($item:item)*) => {
        $(
            #[cfg_attr(
                not(db_tests),
                ignore = "DATABASE_URL is not set; run `docker compose -f deploy/docker-compose.dev.yml up -d postgres` and export DATABASE_URL from .env.example"
            )]
            $item
        )*
    };
}

async fn call(
    pool: &PgPool,
    method: &str,
    uri: &str,
    cookie: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value, Option<String>) {
    let state = ApiState::new(pool.clone(), Registry::default(), ApiSettings::default());
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(CSRF_HEADER, "1")
        .header("content-type", "application/json");
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    let request = request
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let response = api::router(state).oneshot(request).await.unwrap();
    let status = response.status();
    let cookie = response
        .headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        cookie,
    )
}

async fn login(pool: &PgPool, name: &str, role: Role, scope: GrantScope) -> String {
    users::create_user_with(pool, name, &Secret::new(PASSWORD), role, scope, "test")
        .await
        .unwrap();
    let body = json!({ "username": name, "password": PASSWORD });
    let (status, _, cookie) = call(pool, "POST", "/api/auth/login", None, Some(body)).await;
    assert_eq!(status, StatusCode::OK);
    cookie.unwrap()
}

/// Minute `m` of October 2026.
fn t(m: i64) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-01T00:00:00Z")
        .unwrap()
        .to_utc()
        + TimeDelta::minutes(m)
}

/// A customer with a site and a server `<name>-web` that was offline from
/// minute `down` for `minutes`, and a service of it.
async fn customer(pool: &PgPool, name: &str, down: i64, minutes: i64) -> (Uuid, Uuid) {
    let customer: Uuid =
        sqlx::query_scalar("INSERT INTO customers (name) VALUES ($1) RETURNING id")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap();
    let site: Uuid =
        sqlx::query_scalar("INSERT INTO sites (name, customer_id) VALUES ($1, $2) RETURNING id")
            .bind(format!("{name}-site"))
            .bind(customer)
            .fetch_one(pool)
            .await
            .unwrap();
    let server: Uuid = sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family, created_at)
         VALUES ($1, $2, $1, 'debian', '2026-01-01T00:00:00Z') RETURNING id",
    )
    .bind(format!("{name}-web"))
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap();
    let incident: Uuid = sqlx::query_scalar(
        "INSERT INTO incidents (server_id, metric, instance, severity, state, opened_at,
                                resolved_at, last_value, last_value_at, peak_value, threshold)
         VALUES ($1, 'offline', '', 'critical', 'resolved', $2, $3, 0, $2, 0, '{}') RETURNING id",
    )
    .bind(server)
    .bind(t(down))
    .bind(t(down + minutes))
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO incident_events (incident_id, occurred_at, kind, severity)
         VALUES ($1, $2, 'opened', 'critical'), ($1, $3, 'resolved', NULL)",
    )
    .bind(incident)
    .bind(t(down))
    .bind(t(down + minutes))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "WITH b AS (INSERT INTO business_services (customer_id, name, target)
                    VALUES ($1, $2, 99.9) RETURNING id)
         INSERT INTO business_service_members (service_id, server_id) SELECT id, $3 FROM b",
    )
    .bind(customer)
    .bind(format!("{name} website"))
    .bind(server)
    .execute(pool)
    .await
    .unwrap();
    (customer, site)
}

const OCTOBER: &str = "from=2026-10-01T00:00:00Z&to=2026-11-01T00:00:00Z";

db_tests! {
    #[sqlx::test]
    async fn each_customer_sees_its_own_report(pool: PgPool) {
        let (acme, acme_site) = customer(&pool, "acme", 600, 30).await;
        let (globex, _) = customer(&pool, "globex", 1000, 90).await;
        let admin = login(&pool, "root", Role::Administrator, GrantScope::Global).await;
        let uri = format!("/api/availability?{OCTOBER}&customer_id={acme}");
        let (status, report, _) = call(&pool, "GET", &uri, Some(&admin), None).await;
        assert_eq!(status, StatusCode::OK, "{report}");
        assert_eq!(report["customer_name"], "acme");
        assert_eq!(report["machines"].as_array().unwrap().len(), 1);
        let web = &report["machines"][0];
        assert_eq!(web["name"], "acme-web");
        assert_eq!(web["numbers"]["down_seconds"], 1800.0);
        assert_eq!(web["numbers"]["counted_seconds"], 31.0 * 86_400.0);
        // 30 minutes of 31 days: 99.9328 %, so 99.9 % is met.
        let service = &report["services"][0];
        assert_eq!(service["name"], "acme website");
        assert_eq!(service["met"], true);
        let budget = 0.001 * 31.0 * 86_400.0 - 1800.0;
        assert!((service["budget_seconds"].as_f64().unwrap() - budget).abs() < 1e-6);
        assert!(!report.to_string().contains("globex"), "{report}");

        // An operator of acme cannot read globex's, and sees only acme's
        // without naming it.
        let olga = login(&pool, "olga", Role::Operator, GrantScope::Customer(acme)).await;
        let uri = format!("/api/availability?{OCTOBER}&customer_id={globex}");
        let (status, _, _) = call(&pool, "GET", &uri, Some(&olga), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (_, report, _) = call(&pool, "GET", &format!("/api/availability?{OCTOBER}"), Some(&olga), None).await;
        assert!(!report.to_string().contains("globex"), "{report}");
        assert_eq!(report["services"].as_array().unwrap().len(), 1);
        // A viewer of one site gets its servers, without services.
        let sam = login(&pool, "sam", Role::Viewer, GrantScope::Site(acme_site)).await;
        let uri = format!("/api/availability?{OCTOBER}&customer_id={acme}");
        let (status, report, _) = call(&pool, "GET", &uri, Some(&sam), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(report["machines"].as_array().unwrap().len(), 1);
        assert_eq!(report["services"], json!([]));
    }

    #[sqlx::test]
    async fn uptime_over_windows_and_days_for_one_customer(pool: PgPool) {
        let (acme, _) = customer(&pool, "acme", 600, 30).await;
        customer(&pool, "globex", 1000, 90).await;
        let admin = login(&pool, "root", Role::Administrator, GrantScope::Global).await;
        let uri = format!(
            "/api/availability/uptime?customer_id={acme}&at=2026-10-30T22:00:00Z&time_zone=Europe/Paris"
        );
        let (status, body, _) = call(&pool, "GET", &uri, Some(&admin), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["time_zone"], "Europe/Paris");
        assert!(!body.to_string().contains("globex"), "{body}");
        let rows = body["rows"].as_array().unwrap();
        let web = rows.iter().find(|r| r["kind"] == "server").unwrap();
        assert_eq!(web["name"], "acme-web");
        assert_eq!(web["outages"], 1);
        let days = web["days"].as_array().unwrap();
        assert_eq!(days.len(), 90);
        // The last day is 30 October in Paris (23:00 there), so far.
        assert_eq!(days[89]["date"], "2026-10-30");
        let down: f64 = days.iter().map(|d| d["down_seconds"].as_f64().unwrap()).sum();
        assert_eq!(down, 1800.0);
        // 30 minutes of the last 30 days.
        let month = web["windows"]["month"].as_f64().unwrap();
        assert!((month - (1.0 - 1800.0 / (30.0 * 86_400.0)) * 100.0).abs() < 1e-9, "{month}");
        assert!(rows.iter().any(|r| r["kind"] == "service"));

        for bad in ["time_zone=Paris", "at=yesterday", "days=30"] {
            let uri = format!("/api/availability/uptime?{bad}");
            let (status, _, _) = call(&pool, "GET", &uri, Some(&admin), None).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
        }
    }

    #[sqlx::test]
    async fn the_report_as_csv(pool: PgPool) {
        let (acme, _) = customer(&pool, "acme", 600, 30).await;
        let admin = login(&pool, "root", Role::Administrator, GrantScope::Global).await;
        let state = ApiState::new(pool.clone(), Registry::default(), ApiSettings::default());
        let uri = format!(
            "/api/availability/report?{OCTOBER}&customer_id={acme}&format=csv&language=fr&time_zone=Europe/Paris"
        );
        let request = Request::builder()
            .uri(&uri)
            .header(CSRF_HEADER, "1")
            .header("cookie", &admin)
            .body(Body::empty())
            .unwrap();
        let response = api::router(state).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()["content-disposition"],
            "attachment; filename=\"availability-acme-2026-10-01-2026-11-01.csv\""
        );
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with("kind,name,server,site,check"), "{text}");
        assert!(lines[1].starts_with("service,acme website,"), "{text}");
        assert!(lines[2].starts_with("server,acme-web,acme-web,acme-site,"), "{text}");
        // 10:00 UTC is 12:00 in Paris in October; the cause in French.
        assert!(
            text.contains("outage,acme-web,,,,,,,,,1800,,,,2026-10-01T12:00:00+02:00,2026-10-01T12:30:00+02:00,acme-web\u{a0}: hors ligne"),
            "{text}"
        );
        // Unknown formats and zones.
        for bad in ["format=xls", "format=csv&time_zone=Paris", "format=csv&language=de"] {
            let (status, _, _) = call(&pool, "GET", &format!("/api/availability/report?{bad}"), Some(&admin), None).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
        }
    }

    #[sqlx::test]
    async fn the_report_as_pdf(pool: PgPool) {
        let (acme, _) = customer(&pool, "acme", 600, 30).await;
        customer(&pool, "globex", 1000, 90).await;
        let admin = login(&pool, "root", Role::Administrator, GrantScope::Global).await;
        let state = ApiState::new(pool.clone(), Registry::default(), ApiSettings::default());
        let uri = format!("/api/availability/report?{OCTOBER}&customer_id={acme}&format=pdf");
        let request = Request::builder()
            .uri(&uri)
            .header(CSRF_HEADER, "1")
            .header("cookie", &admin)
            .body(Body::empty())
            .unwrap();
        let response = api::router(state).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "application/pdf");
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 22).await.unwrap();
        assert!(bytes.starts_with(b"%PDF-"));
        let has = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
        assert!(has(b"(For acme)"));
        assert!(has(b"(acme website)"));
        assert!(!has(b"globex"));
    }
}
