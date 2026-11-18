//! The API of check silences and maintenance windows (ADR-0151): who may
//! do what, what is checked, what a window says about its next windows,
//! and the audit log. Ignored without `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{TimeDelta, Utc};
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
const WINDOWS: &str = "/api/maintenance-windows";

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

struct Place {
    site: Uuid,
    other_site: Uuid,
    web: Uuid,
    stranger: Uuid,
    relay: Uuid,
}

async fn place(pool: &PgPool) -> Place {
    let customer: Uuid =
        sqlx::query_scalar("INSERT INTO customers (name) VALUES ('acme') RETURNING id")
            .fetch_one(pool)
            .await
            .unwrap();
    let mut sites = Vec::new();
    for name in ["paris", "lyon"] {
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO sites (name, customer_id) VALUES ($1, $2) RETURNING id",
        )
        .bind(name)
        .bind(customer)
        .fetch_one(pool)
        .await
        .unwrap();
        sites.push(id);
    }
    let mut servers = Vec::new();
    for (name, site) in [("web-1", sites[0]), ("lyon-1", sites[1])] {
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family)
             VALUES ($1, $2, $1, 'debian') RETURNING id",
        )
        .bind(name)
        .bind(site)
        .fetch_one(pool)
        .await
        .unwrap();
        servers.push(id);
    }
    sqlx::query(
        "INSERT INTO sensor_configs (server_id, kind, name, instance, interval_ms, params)
         VALUES ($1, 'systemd', 'service_nginx', '', 60000, '{\"unit\": \"nginx.service\"}')",
    )
    .bind(servers[0])
    .execute(pool)
    .await
    .unwrap();
    let relay = sqlx::query_scalar(
        "INSERT INTO relays (peer_id, site_id, name) VALUES ('gw', $1, 'gw') RETURNING id",
    )
    .bind(sites[0])
    .fetch_one(pool)
    .await
    .unwrap();
    Place {
        site: sites[0],
        other_site: sites[1],
        web: servers[0],
        stranger: servers[1],
        relay,
    }
}

fn weekly(site: Uuid) -> Value {
    json!({
        "site_id": site,
        "repeat": "weekly",
        "weekdays": ["sunday"],
        "start_time": "02:00",
        "duration_minutes": 120,
        "time_zone": "Europe/Paris",
        "reason": "Patch night",
    })
}

async fn audited(pool: &PgPool, action: &str) -> Vec<Value> {
    sqlx::query_scalar("SELECT details FROM audit_log WHERE action = $1 ORDER BY id")
        .bind(action)
        .fetch_all(pool)
        .await
        .unwrap()
}

db_tests! {
    #[sqlx::test]
    async fn operators_make_change_and_remove_windows(pool: PgPool) {
        let p = place(&pool).await;
        let operator = login(
            &pool,
            "olga",
            Role::Operator,
            GrantScope::Site(p.site),
        )
        .await;
        let viewer = login(&pool, "vic", Role::Viewer, GrantScope::Global).await;

        let (status, _, _) = call(&pool, "POST", WINDOWS, Some(&viewer), Some(weekly(p.site))).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        // Another site: not seen.
        let (status, _, _) =
            call(&pool, "POST", WINDOWS, Some(&operator), Some(weekly(p.other_site))).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, made, _) =
            call(&pool, "POST", WINDOWS, Some(&operator), Some(weekly(p.site))).await;
        assert_eq!(status, StatusCode::CREATED, "{made}");
        assert_eq!(made["weekdays"], json!(["sunday"]));
        assert_eq!(made["months"].as_array().unwrap().len(), 12);
        assert_eq!(made["servers"], json!([]));
        assert_eq!(made["can_change"], true);
        let next = made["next"].as_array().unwrap();
        assert_eq!(next.len(), 3);
        let first: chrono::DateTime<Utc> =
            serde_json::from_value(next[0]["starts_at"].clone()).unwrap();
        let second: chrono::DateTime<Utc> =
            serde_json::from_value(next[1]["starts_at"].clone()).unwrap();
        assert_eq!(second - first, TimeDelta::days(7));
        let id = made["id"].as_str().unwrap().to_owned();
        assert_eq!(audited(&pool, "maintenance_window.create").await.len(), 1);

        // Everyone who reads the site sees it, by site or by server.
        for query in [
            String::new(),
            format!("?site_id={}", p.site),
            format!("?server_id={}", p.web),
        ] {
            let (status, list, _) =
                call(&pool, "GET", &format!("{WINDOWS}{query}"), Some(&viewer), None).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(list["windows"].as_array().unwrap().len(), 1, "{query}");
            assert_eq!(list["windows"][0]["can_change"], false);
        }
        let (_, list, _) = call(
            &pool,
            "GET",
            &format!("{WINDOWS}?server_id={}", p.stranger),
            Some(&viewer),
            None,
        )
        .await;
        assert!(list["windows"].as_array().unwrap().is_empty());

        // Narrowed to a server and a check, on the second Tuesday.
        let mut change = weekly(p.site);
        change["server_ids"] = json!([p.web]);
        change["checks"] = json!([{"name": "service_nginx"}]);
        change["repeat"] = json!("monthly");
        change["weekdays"] = json!(["tuesday"]);
        change["month_week"] = json!(2);
        let uri = format!("{WINDOWS}/{id}");
        let (status, changed, _) = call(&pool, "PUT", &uri, Some(&operator), Some(change)).await;
        assert_eq!(status, StatusCode::OK, "{changed}");
        assert_eq!(changed["servers"][0]["hostname"], "web-1");
        assert_eq!(changed["checks"], json!([{"name": "service_nginx", "instance": ""}]));
        assert_eq!(changed["month_week"], 2);
        assert_eq!(audited(&pool, "maintenance_window.update").await.len(), 1);

        let (status, _, _) = call(&pool, "DELETE", &uri, Some(&viewer), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _, _) = call(&pool, "DELETE", &uri, Some(&operator), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _, _) = call(&pool, "DELETE", &uri, Some(&operator), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(audited(&pool, "maintenance_window.remove").await.len(), 1);
        let (_, list, _) = call(&pool, "GET", WINDOWS, Some(&viewer), None).await;
        assert!(list["windows"].as_array().unwrap().is_empty());
    }

    #[sqlx::test]
    async fn a_running_window_shows_as_a_silence_and_on_its_server(pool: PgPool) {
        let p = place(&pool).await;
        let admin = login(&pool, "root", Role::Administrator, GrantScope::Global).await;
        let start = Utc::now() - TimeDelta::minutes(30);
        let mut body = weekly(p.site);
        body["repeat"] = json!("daily");
        body["time_zone"] = json!("UTC");
        body["start_time"] = json!(start.format("%H:%M").to_string());
        body["starts_on"] = json!(start.date_naive());
        let (status, made, _) = call(&pool, "POST", WINDOWS, Some(&admin), Some(body)).await;
        assert_eq!(status, StatusCode::CREATED, "{made}");
        assert!(!made["active_until"].is_null());

        // Its running occurrence is listed, its coming ones are not.
        let (_, list, _) = call(&pool, "GET", "/api/silences", Some(&admin), None).await;
        let silences = list["silences"].as_array().unwrap();
        assert_eq!(silences.len(), 1, "{list}");
        assert_eq!(silences[0]["window_id"], made["id"]);
        assert_eq!(silences[0]["scope"], "site");
        let (_, server, _) =
            call(&pool, "GET", &format!("/api/servers/{}", p.web), Some(&admin), None).await;
        assert_eq!(server["silence"]["window"], true);

        // Ending it skips this one window; the window stays.
        let uri = format!("/api/silences/{}/end", silences[0]["id"].as_str().unwrap());
        let (status, _, _) = call(&pool, "POST", &uri, Some(&admin), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, list, _) = call(&pool, "GET", WINDOWS, Some(&admin), None).await;
        assert!(list["windows"][0]["active_until"].is_null());
        assert_eq!(audited(&pool, "silence.end").await[0]["window_id"], made["id"]);
    }

    #[sqlx::test]
    async fn rules_and_places_that_are_refused(pool: PgPool) {
        let p = place(&pool).await;
        let admin = login(&pool, "root", Role::Administrator, GrantScope::Global).await;
        let with = |key: &str, value: Value| {
            let mut body = weekly(p.site);
            body[key] = value;
            body
        };
        for bad in [
            with("repeat", json!("yearly")),
            with("weekdays", json!([])),
            with("time_zone", json!("Mars/Olympus")),
            with("start_time", json!("25:00")),
            with("duration_minutes", json!(10081)),
            with("server_ids", json!([p.stranger])),
            with("checks", json!([{"name": "service_nope"}])),
            with("checks", json!([])),
            with("reason", json!("  ")),
            with("months", json!([0])),
        ] {
            let (status, answer, _) = call(&pool, "POST", WINDOWS, Some(&admin), Some(bad.clone())).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad} {answer}");
        }
    }

    #[sqlx::test]
    async fn a_silence_of_checks_is_checked_and_listed(pool: PgPool) {
        let p = place(&pool).await;
        let admin = login(&pool, "root", Role::Administrator, GrantScope::Global).await;
        let silence = |scope: &str, target: Uuid, checks: Value| {
            json!({
                "scope": scope, "target_id": target, "checks": checks,
                "ends_at": Utc::now() + TimeDelta::hours(1), "reason": "rebuild",
            })
        };
        for (scope, target, checks) in [
            ("relay", p.relay, json!([{"name": "service_nginx"}])),
            ("server", p.web, json!([{"name": "service_nope"}])),
            ("server", p.web, json!([{"name": "Bad Name"}])),
            ("server", p.stranger, json!([{"name": "service_nginx"}])),
        ] {
            let body = silence(scope, target, checks);
            let (status, _, _) = call(&pool, "POST", "/api/silences", Some(&admin), Some(body.clone())).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        }
        // A built-in check, and one of the server, the same one twice.
        let body = silence(
            "server",
            p.web,
            json!([{"name": "service_nginx"}, {"name": "disk", "instance": "/var"}, {"name": "service_nginx"}]),
        );
        let (status, made, _) = call(&pool, "POST", "/api/silences", Some(&admin), Some(body.clone())).await;
        assert_eq!(status, StatusCode::CREATED, "{made}");
        assert_eq!(made["checks"].as_array().unwrap().len(), 2);
        assert!(made["window_id"].is_null());
        // The same again is the same silence.
        let (status, again, _) = call(&pool, "POST", "/api/silences", Some(&admin), Some(body)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(again["id"], made["id"]);
        // A site's checks are its servers'.
        let body = silence("site", p.site, json!([{"name": "service_nginx"}]));
        let (status, _, _) = call(&pool, "POST", "/api/silences", Some(&admin), Some(body)).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(audited(&pool, "silence.create").await[0]["checks"].as_array().unwrap().len(), 2);
        // The server is not silenced.
        let (_, server, _) =
            call(&pool, "GET", &format!("/api/servers/{}", p.web), Some(&admin), None).await;
        assert!(server["silenced_until"].is_null());
        assert_ne!(server["state"], "silenced");
    }
}
