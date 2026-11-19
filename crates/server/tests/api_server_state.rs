//! One status per server (ADR-0130): the `state` of `GET /api/servers`
//! and the counts of `GET /api/servers/summary`, against PostgreSQL.
//! Ignored without `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{TimeDelta, Utc};
use monitorflow_common::redact::Secret;
use monitorflow_common::tls::PeerRole;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::{Counters, PeerInfo, Registry};
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
    state: &ApiState,
    uri: &str,
    cookie: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(if body.is_some() { "POST" } else { "GET" })
        .uri(uri)
        .header(CSRF_HEADER, "1")
        .header("content-type", "application/json");
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    let request = request
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let response = api::router(state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let cookie = response
        .headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let mut body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if let Some(cookie) = cookie {
        body["cookie"] = cookie.into();
    }
    (status, body)
}

async fn login(state: &ApiState, name: &str, role: Role, scope: GrantScope) -> String {
    users::create_user_with(
        &state.pool,
        name,
        &Secret::new(PASSWORD),
        role,
        scope,
        "test",
    )
    .await
    .unwrap();
    let body = json!({ "username": name, "password": PASSWORD });
    let (status, body) = call(state, "/api/auth/login", None, Some(body)).await;
    assert_eq!(status, StatusCode::OK);
    body["cookie"].as_str().unwrap().to_owned()
}

fn live(id: &str) -> PeerInfo {
    let now = Utc::now();
    PeerInfo {
        id: id.into(),
        role: PeerRole::Agent,
        via_relay: None,
        relay_path: Vec::new(),
        connection: 0,
        cert_serial: None,
        agent_version: String::new(),
        hostname: id.into(),
        connected_at: now,
        last_seen: now,
        last_sequence: 0,
        counters: Counters::default(),
        sensor_revision_sent: None,
        sensor_revision_applied: None,
    }
}

async fn site(pool: &PgPool, name: &str) -> Uuid {
    let customer: Uuid =
        sqlx::query_scalar("INSERT INTO customers (name) VALUES ($1) RETURNING id")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap();
    sqlx::query_scalar("INSERT INTO sites (name, customer_id) VALUES ($1, $2) RETURNING id")
        .bind(name)
        .bind(customer)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn server(pool: &PgPool, site: Uuid, name: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family)
         VALUES ($1, $2, $1, 'debian') RETURNING id",
    )
    .bind(name)
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn open(pool: &PgPool, server: Uuid, metric: &str, severity: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO incidents (server_id, metric, instance, severity, opened_at, last_value,
                                last_value_at, peak_value, threshold)
         VALUES ($1, $2, '', $3, now(), 91, now(), 91, '{}') RETURNING id",
    )
    .bind(server)
    .bind(metric)
    .bind(severity)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn acknowledge(pool: &PgPool, incident: Uuid) {
    sqlx::query(
        "INSERT INTO incident_acknowledgements (incident_id, username, via, note)
         VALUES ($1, 'alice', 'page', 'swapping the disk')",
    )
    .bind(incident)
    .execute(pool)
    .await
    .unwrap();
}

db_tests! {
    #[sqlx::test]
    async fn every_server_has_one_state_and_the_summary_counts_them(pool: PgPool) {
        let registry = Registry::default();
        let state = ApiState::new(pool.clone(), registry.clone(), ApiSettings::default());
        let (a, b) = (site(&pool, "a").await, site(&pool, "b").await);
        let critical = server(&pool, a, "critical-1").await;
        let warning = server(&pool, a, "warning-1").await;
        let acknowledged = server(&pool, a, "acknowledged-1").await;
        let silenced = server(&pool, a, "silenced-1").await;
        server(&pool, a, "ok-1").await;
        server(&pool, b, "offline-1").await;
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let _live: Vec<_> = ["critical-1", "warning-1", "acknowledged-1", "silenced-1", "ok-1"]
            .into_iter()
            .map(|id| registry.register(live(id), tx.clone()).unwrap())
            .collect();

        // An acknowledged critical does not hide a warning nobody took.
        let taken = open(&pool, critical, "cpu", "critical").await;
        acknowledge(&pool, taken).await;
        open(&pool, critical, "disk", "critical").await;
        let taken = open(&pool, warning, "cpu", "critical").await;
        acknowledge(&pool, taken).await;
        open(&pool, warning, "disk", "warning").await;
        let taken = open(&pool, acknowledged, "cpu", "critical").await;
        acknowledge(&pool, taken).await;
        open(&pool, silenced, "cpu", "critical").await;
        sqlx::query(
            "INSERT INTO silences (server_id, starts_at, ends_at, reason)
             VALUES ($1, $2, $3, 'migration')",
        )
        .bind(silenced)
        .bind(Utc::now() - TimeDelta::minutes(1))
        .bind(Utc::now() + TimeDelta::hours(1))
        .execute(&pool)
        .await
        .unwrap();

        let admin = login(&state, "root", Role::Administrator, GrantScope::Global).await;
        let (status, list) = call(&state, "/api/servers", Some(&admin), None).await;
        assert_eq!(status, StatusCode::OK);
        let states: Vec<(&str, &str)> = list["servers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| (s["hostname"].as_str().unwrap(), s["state"].as_str().unwrap()))
            .collect();
        assert_eq!(
            states,
            [
                ("acknowledged-1", "acknowledged"),
                ("critical-1", "critical"),
                ("ok-1", "ok"),
                ("silenced-1", "silenced"),
                ("warning-1", "warning"),
                ("offline-1", "offline"),
            ]
        );
        // Who silenced or acknowledged, and why, is on the row (ADR-0130).
        let row = |name: &str| {
            list["servers"].as_array().unwrap().iter().find(|s| s["hostname"] == name).unwrap().clone()
        };
        let silence = &row("silenced-1")["silence"];
        assert_eq!((silence["reason"].as_str(), silence["by"].is_null()), (Some("migration"), true));
        assert_eq!(silence["until"], row("silenced-1")["silenced_until"]);
        let taken = &row("acknowledged-1")["acknowledgement"];
        assert_eq!((taken["by"].as_str(), taken["note"].as_str()), (Some("alice"), Some("swapping the disk")));
        assert!(taken["at"].is_string());
        assert!(row("ok-1")["silence"].is_null() && row("ok-1")["acknowledgement"].is_null());

        let (_, detail) = call(&state, &format!("/api/servers/{critical}"), Some(&admin), None).await;
        assert_eq!(detail["state"], "critical");
        assert_eq!(detail["open_incidents"], 2);

        let (status, all) = call(&state, "/api/servers/summary", Some(&admin), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            all,
            json!({"critical": 1, "warning": 1, "acknowledged": 1, "silenced": 1,
                   "offline": 1, "ok": 1, "waiting": 0, "unreachable": 0, "total": 6})
        );
        let (_, of_b) = call(&state, &format!("/api/servers/summary?site_id={b}"), Some(&admin), None).await;
        assert_eq!((of_b["offline"].clone(), of_b["total"].clone()), (json!(1), json!(1)));
        let customer: Uuid = sqlx::query_scalar("SELECT customer_id FROM sites WHERE id = $1")
            .bind(a)
            .fetch_one(&pool)
            .await
            .unwrap();
        let (_, of_a) =
            call(&state, &format!("/api/servers/summary?customer_id={customer}"), Some(&admin), None).await;
        assert_eq!(of_a["total"], 5);

        // A viewer of site b counts only b, whatever it asks for.
        let viewer = login(&state, "bob", Role::Viewer, GrantScope::Site(b)).await;
        let (_, theirs) = call(&state, "/api/servers/summary", Some(&viewer), None).await;
        assert_eq!(theirs["total"], 1);
        let (_, other) = call(&state, &format!("/api/servers/summary?site_id={a}"), Some(&viewer), None).await;
        assert_eq!(other["total"], 0);

        let (status, _) = call(&state, "/api/servers/summary?site_id=nope", Some(&admin), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = call(&state, "/api/servers/summary", None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}
