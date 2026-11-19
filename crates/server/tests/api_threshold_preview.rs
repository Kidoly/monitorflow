//! Threshold previews (ADR-0153) against a real PostgreSQL and an
//! in-process fake VictoriaMetrics: the queries sent with the server's
//! filters, the replay, site scoping, and nothing written.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{OriginalUri, State};
use axum::http::{Request, StatusCode};
use axum::Router;
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users;
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use monitorflow_server::vm_query::VmQuery;
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::net::TcpListener;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";
const MIN: i64 = 60_000;

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

/// The parameters of every query received.
type Queries = Arc<Mutex<Vec<Vec<(String, String)>>>>;

/// Answers every range query with load samples of `web-1`: 1 until the
/// last 30 minutes, then 6; records the queries.
#[derive(Clone, Default)]
struct FakeVm(Queries);

async fn answer(State(fake): State<FakeVm>, OriginalUri(uri): OriginalUri) -> String {
    let url = reqwest::Url::parse(&format!("http://fake{uri}")).unwrap();
    let params: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    let get = |k: &str| {
        params
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
            .unwrap()
    };
    let (start, end): (i64, i64) = (get("start").parse().unwrap(), get("end").parse().unwrap());
    fake.0.lock().unwrap().push(params);
    let values: Vec<Value> = (start..=end)
        .step_by(60)
        .map(|t| {
            json!([
                t,
                if t * 1000 > end * 1000 - 30 * MIN {
                    "6"
                } else {
                    "1"
                }
            ])
        })
        .collect();
    json!({"status": "success", "data": {"resultType": "matrix", "result": [
        {"metric": {"__name__": "monitorflow_cpu_load1", "peer": "web-1", "site": "paris"},
         "values": values}
    ]}})
    .to_string()
}

async fn app(pool: &PgPool) -> (Router, FakeVm) {
    let fake = FakeVm::default();
    let vm = Router::new().fallback(answer).with_state(fake.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, vm).await.unwrap() });
    let client = VmQuery::with_timeout(&url, Duration::from_secs(5)).unwrap();
    let router = api::router(
        ApiState::new(pool.clone(), Registry::default(), ApiSettings::default())
            .with_metrics(client),
    );
    (router, fake)
}

async fn send(router: &Router, request: Request<Body>) -> (StatusCode, Value, Option<String>) {
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let cookie = response
        .headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 4 << 20)
        .await
        .unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body, cookie)
}

async fn session(pool: &PgPool, router: &Router, site: Uuid) -> String {
    let id = users::create_user(pool, "viewer", &Secret::new(PASSWORD), Role::Viewer, "test")
        .await
        .unwrap();
    sqlx::query("UPDATE user_roles SET site_id = $2 WHERE user_id = $1")
        .bind(id)
        .bind(site)
        .execute(pool)
        .await
        .unwrap();
    let request = Request::post("/api/auth/login")
        .header("content-type", "application/json")
        .header(CSRF_HEADER, "1")
        .body(Body::from(
            json!({"username": "viewer", "password": PASSWORD}).to_string(),
        ))
        .unwrap();
    let (status, _, cookie) = send(router, request).await;
    assert_eq!(status, StatusCode::OK);
    cookie.unwrap()
}

async fn post(router: &Router, cookie: &str, uri: &str, body: Value) -> (StatusCode, Value) {
    let request = Request::post(uri)
        .header("cookie", cookie)
        .header(CSRF_HEADER, "1")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let (status, body, _) = send(router, request).await;
    (status, body)
}

async fn site(pool: &PgPool, name: &str) -> (Uuid, Uuid) {
    let site: Uuid = sqlx::query_scalar("INSERT INTO sites (name) VALUES ($1) RETURNING id")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap();
    let server = sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family)
         VALUES ($1, $2, $1, 'debian') RETURNING id",
    )
    .bind(if name == "paris" { "web-1" } else { "db-1" })
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap();
    (site, server)
}

db_tests! {
    #[sqlx::test(migrations = "./migrations")]
    async fn a_preview_replays_history_and_writes_nothing(pool: PgPool) {
        let (paris, web) = site(&pool, "paris").await;
        let (_, db) = site(&pool, "lyon").await;
        let (router, fake) = app(&pool).await;
        let cookie = session(&pool, &router, paris).await;
        let rule = json!({"metric": "monitorflow_cpu_load1", "instance": "", "kind": "window",
                          "function": "avg", "window_s": 900, "comparison": ">", "warning": 4,
                          "conditions": [{"metric": "monitorflow_cpu_load1", "comparison": ">",
                                          "value": 0.5}]});
        let (status, body) = post(&router, &cookie,
            &format!("/api/servers/{web}/thresholds/preview"),
            json!({"rule": rule, "days": 1})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["days"], 1);
        let series = &body["series"][0];
        assert_eq!(series["instance"], "");
        assert!(series["points"].as_array().unwrap().len() <= 300);
        let incidents = series["incidents"].as_array().unwrap();
        assert_eq!(incidents.len(), 1, "{incidents:?}");
        assert_eq!(incidents[0]["severity"], "warning");
        assert_eq!(incidents[0]["resolved_at"], Value::Null);
        // One query for the series, one for the condition, both scoped.
        let seen = fake.0.lock().unwrap().clone();
        assert_eq!(seen.len(), 2);
        for params in &seen {
            let query = &params.iter().find(|(k, _)| k == "query").unwrap().1;
            assert!(query.starts_with("last_over_time(monitorflow_cpu_load1{peer=\"web-1\",site=\"paris\""), "{query}");
            assert!(params.contains(&("extra_label".into(), "peer=web-1".into())));
        }
        // The window starts 15 minutes before the day shown.
        let start: i64 = seen[0].iter().find(|(k, _)| k == "start").unwrap().1.parse().unwrap();
        assert!(body["from"].as_i64().unwrap() / 1000 - start >= 899);

        // The site preview names its servers; another site is not found.
        let (status, body) = post(&router, &cookie,
            &format!("/api/sites/{paris}/thresholds/preview"), json!({"rule": rule})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["servers"][0]["hostname"], "web-1");
        assert_eq!(body["servers"][0]["incidents"], 1);
        assert_eq!(body["servers"][0]["series"][0]["points"], json!([]));
        assert_eq!(body["more_servers"], false);
        let (status, _) = post(&router, &cookie,
            &format!("/api/servers/{db}/thresholds/preview"), json!({"rule": rule})).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, body) = post(&router, &cookie,
            &format!("/api/servers/{web}/thresholds/preview"),
            json!({"rule": {"metric": "monitorflow_cpu_load1", "kind": "window", "comparison": ">",
                            "warning": 4}})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().contains("needs a function"));

        // A viewer may preview, and nothing was written or audited.
        let written: i64 = sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM thresholds) + (SELECT count(*) FROM incidents)
                  + (SELECT count(*) FROM audit_log WHERE action LIKE 'threshold.%')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(written, 0);
    }
}
