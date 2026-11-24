//! Traffic flows through the API and PostgreSQL (ADR-0184): who
//! receives a device's flows, what probes are sent, reports written,
//! summed into hours and read back. Ignored without `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use chrono::{Duration, DurationRound, Utc};
use monitorflow_common::proto::v1::{FlowConversation, FlowReport, FlowTop};
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users;
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::flows::store;
use monitorflow_server::grpc::Registry;
use monitorflow_server::probes::ProbeStore;
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

fn app(pool: &PgPool) -> Router {
    api::router(ApiState::new(
        pool.clone(),
        Registry::default(),
        ApiSettings::default(),
    ))
}

async fn send(pool: &PgPool, request: Request<Body>) -> (StatusCode, Value, Option<String>) {
    let response = app(pool).oneshot(request).await.unwrap();
    let status = response.status();
    let cookie = response
        .headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body, cookie)
}

async fn session(pool: &PgPool, username: &str, role: Role, site: Option<Uuid>) -> String {
    let id = users::create_user(pool, username, &Secret::new(PASSWORD), role, "test")
        .await
        .unwrap();
    sqlx::query("UPDATE user_roles SET site_id = $2 WHERE user_id = $1")
        .bind(id)
        .bind(site)
        .execute(pool)
        .await
        .unwrap();
    let body = json!({ "username": username, "password": PASSWORD }).to_string();
    let request = Request::post("/api/auth/login")
        .header("content-type", "application/json")
        .header(CSRF_HEADER, "1")
        .body(Body::from(body))
        .unwrap();
    let (status, _, cookie) = send(pool, request).await;
    assert_eq!(status, StatusCode::OK);
    cookie.unwrap()
}

async fn call(
    pool: &PgPool,
    cookie: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("cookie", cookie)
        .header(CSRF_HEADER, "1")
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let (status, body, _) = send(pool, request).await;
    (status, body)
}

async fn site(pool: &PgPool, name: &str) -> Uuid {
    sqlx::query_scalar("INSERT INTO sites (name) VALUES ($1) RETURNING id")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn relay(pool: &PgPool, site: Uuid, peer: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO relays (peer_id, site_id, name) VALUES ($1, $2, $1) RETURNING id",
    )
    .bind(peer)
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap()
}

fn top(dimension: &str, key: &str, bytes: u64) -> FlowTop {
    FlowTop {
        dimension: dimension.into(),
        key: key.into(),
        bytes,
        packets: 1,
    }
}

fn report(device: &str, start_ms: i64) -> FlowReport {
    FlowReport {
        device: device.into(),
        start_unix_ms: start_ms,
        seconds: 60,
        bytes: 10_000,
        packets: 10,
        tops: vec![
            top("src", "192.0.2.15", 8_000),
            top("src", "192.0.2.16", 1_000),
            top("dst", "198.51.100.1", 9_000),
            top("app", "tcp/443", 9_000),
            top("protocol", "tcp", 10_000),
            top("in", "3", 10_000),
            top("out", "0", 10_000),
            top("nonsense", "x", 1),
        ],
        conversations: vec![FlowConversation {
            src: "192.0.2.15".into(),
            dst: "198.51.100.1".into(),
            app: "tcp/443".into(),
            input: 3,
            output: 0,
            bytes: 8_000,
            packets: 8,
        }],
    }
}

db_tests! {
    #[sqlx::test]
    async fn exporters_are_set_sent_to_their_probe_and_read(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let lyon = site(&pool, "lyon").await;
        let operator = session(&pool, "op", Role::Operator, Some(paris)).await;
        let viewer = session(&pool, "viewer", Role::Viewer, Some(paris)).await;
        let stranger = session(&pool, "lyon-op", Role::Operator, Some(lyon)).await;
        let relay_id = relay(&pool, paris, "relay-paris").await;
        let (status, router) = call(&pool, &operator, "POST", "/api/devices",
            Some(json!({"site_id": paris, "name": "Router", "address": "192.0.2.1", "ping": false}))).await;
        assert_eq!(status, StatusCode::CREATED, "{router}");
        let id = router["id"].as_str().unwrap().to_owned();
        let path = format!("/api/devices/{id}/flow-exporter");

        assert_eq!(call(&pool, &viewer, "GET", &path, None).await.0, StatusCode::NOT_FOUND);
        assert_eq!(call(&pool, &viewer, "PUT", &path, Some(json!({}))).await.0, StatusCode::FORBIDDEN);
        assert_eq!(call(&pool, &stranger, "GET", &path, None).await.0, StatusCode::NOT_FOUND);
        let (status, error) = call(&pool, &operator, "PUT", &path, Some(json!({"source_address": "a b"}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");

        // The site's relay by default.
        let (status, view) = call(&pool, &operator, "PUT", &path, Some(json!({}))).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["probe"], json!(relay_id));
        assert_eq!(view["relay_name"], "relay-paris");
        assert_eq!(view["address"], "192.0.2.1");
        assert_eq!(view["last_flow_at"], Value::Null);
        let sets = ProbeStore::new(pool.clone()).load().await.unwrap();
        let set = &sets["relay-paris"];
        assert_eq!(set.exporters.len(), 1);
        assert_eq!(set.exporters[0].addresses, ["192.0.2.1"]);
        assert_eq!(set.sites[&set.exporters[0].device], "paris");
        assert!(set.to_proto().flow_exporters.len() == 1);

        // The central server, from a loopback.
        let (_, view) = call(&pool, &operator, "PUT", &path,
            Some(json!({"probe": "central", "source_address": "192.0.2.254"}))).await;
        assert_eq!((view["probe"].as_str(), view["address"].as_str()), (Some("central"), Some("192.0.2.254")));
        let sets = ProbeStore::new(pool.clone()).load().await.unwrap();
        assert!(sets.get("relay-paris").is_none_or(|s| s.exporters.is_empty()));
        assert_eq!(sets["central"].exporters[0].addresses, ["192.0.2.254"]);
        let actions: Vec<String> = sqlx::query_scalar(
            "SELECT action FROM audit_log WHERE action LIKE 'device.flow%' ORDER BY id")
            .fetch_all(&pool).await.unwrap();
        assert_eq!(actions, ["device.flow_exporter_update", "device.flow_exporter_update"]);

        // Reports are written, out-of-time ones and other devices dropped.
        let peer = router["peer_id"].as_str().unwrap().to_owned();
        let now = Utc::now();
        let minute = now.duration_trunc(Duration::minutes(1)).unwrap() - Duration::minutes(2);
        let ms = minute.timestamp_millis();
        store::insert(&pool, &[report(&peer, ms), report(&peer, ms), report("unknown", ms),
                               report(&peer, ms - 3 * 3_600_000)], now).await.unwrap();
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM flow_tops").fetch_one(&pool).await.unwrap();
        assert_eq!(rows, 8, "total and 7 keys, the unknown dimension left out");
        let bytes: i64 = sqlx::query_scalar("SELECT bytes FROM flow_tops WHERE dimension = 'total'")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(bytes, 20_000, "the same window twice adds up");

        let flows = format!("/api/devices/{id}/flows?by=app");
        let (status, view) = call(&pool, &viewer, "GET", &flows, None).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["bytes"], 20_000);
        let src = view["dimensions"].as_array().unwrap().iter().find(|d| d["dimension"] == "src").unwrap();
        assert_eq!(src["keys"][0]["key"], "192.0.2.15");
        assert_eq!(src["other_bytes"], 2_000);
        let app = view["dimensions"].as_array().unwrap().iter().find(|d| d["dimension"] == "app").unwrap();
        assert_eq!(app["keys"][0]["label"], "HTTPS");
        assert_eq!(view["chart"]["keys"][0]["key"], "tcp/443");
        let points = view["chart"]["points"].as_array().unwrap();
        assert!((60..=61).contains(&points.len()), "{}", points.len());
        assert!(points.iter().any(|p| p["values"][0].as_f64() == Some(18_000.0 * 8.0 / 60.0)), "{points:?}");
        let (_, filtered) = call(&pool, &viewer, "GET",
            &format!("/api/devices/{id}/flows?address=192.0.2.15&by=dst"), None).await;
        assert_eq!(filtered["address"], "192.0.2.15");
        assert_eq!(filtered["bytes"], 16_000);
        assert_eq!(filtered["chart"]["keys"][0]["key"], "198.51.100.1");
        let (status, _) = call(&pool, &viewer, "GET", &format!("/api/devices/{id}/flows?by=port"), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(call(&pool, &stranger, "GET", &flows, None).await.0, StatusCode::NOT_FOUND);
        let (_, view) = call(&pool, &viewer, "GET", &path, None).await;
        assert!(view["last_flow_at"].is_string());

        // Stopped: no probe gets it, what was stored stays.
        assert_eq!(call(&pool, &operator, "DELETE", &path, None).await.0, StatusCode::NO_CONTENT);
        assert_eq!(call(&pool, &operator, "DELETE", &path, None).await.0, StatusCode::NOT_FOUND);
        let sets = ProbeStore::new(pool.clone()).load().await.unwrap();
        assert!(sets.values().all(|s| s.exporters.is_empty()));
        assert_eq!(call(&pool, &viewer, "GET", &flows, None).await.1["bytes"], 20_000);
    }

    #[sqlx::test]
    async fn old_minutes_become_hours_then_expire(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family, kind, address, package_updates)
             VALUES ('router', $1, 'router', 'device', 'device', '192.0.2.1', false) RETURNING id")
            .bind(paris).fetch_one(&pool).await.unwrap();
        sqlx::query("INSERT INTO flow_exporters (server_id, probe) VALUES ($1, 'central')")
            .bind(id).execute(&pool).await.unwrap();
        // Three minutes of one hour, 50 hours ago; written as if then.
        let now = Utc::now();
        let hour = (now - Duration::hours(50)).duration_trunc(Duration::hours(1)).unwrap();
        for m in 0..3 {
            let at = hour + Duration::minutes(m);
            store::insert(&pool, &[report("router", at.timestamp_millis())], at + Duration::seconds(70)).await.unwrap();
        }
        // And a recent minute, which stays.
        let recent = (now - Duration::minutes(5)).duration_trunc(Duration::minutes(1)).unwrap();
        store::insert(&pool, &[report("router", recent.timestamp_millis())], now).await.unwrap();
        let done = store::maintain(&pool, now).await.unwrap();
        assert_eq!(done.minutes_summed, 3 * 8 + 3, "tops and conversations of the 3 old minutes");
        let (seconds, bytes): (i32, i64) = sqlx::query_as(
            "SELECT seconds, bytes FROM flow_tops WHERE dimension = 'total' AND at = $1")
            .bind(hour).fetch_one(&pool).await.unwrap();
        assert_eq!((seconds, bytes), (3600, 30_000));
        let conversation: i64 = sqlx::query_scalar(
            "SELECT bytes FROM flow_conversations WHERE seconds = 3600").fetch_one(&pool).await.unwrap();
        assert_eq!(conversation, 24_000);
        let minutes: i64 = sqlx::query_scalar("SELECT count(*) FROM flow_tops WHERE seconds = 60")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(minutes, 8, "the recent minute");
        // Again: nothing more to sum.
        assert!(!store::maintain(&pool, now).await.unwrap().any());
        // A month later the hour is gone.
        let later = store::maintain(&pool, now + Duration::days(31)).await.unwrap();
        assert!(later.dropped >= 8, "{later:?}");
        let left: i64 = sqlx::query_scalar("SELECT count(*) FROM flow_tops WHERE seconds = 3600")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(left, 0);
    }
}
