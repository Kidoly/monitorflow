//! The system logs route (ADR-0091) against a real PostgreSQL and an
//! in-process fake VictoriaLogs that records every request: the query and
//! stream filter sent, site scoping, the answer's parsing and backend
//! errors. Ignored without `DATABASE_URL`.

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

/// Marks every test in the block as ignored when `DATABASE_URL` is unset.
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

/// VictoriaLogs answers of the lines and units queries.
const LINES: &str = r#"{"_msg":"older","_time":"2026-10-19T10:10:00Z","priority":"4","identifier":"kernel","peer":"agent-paris","site":"paris"}
{"_msg":"disk full","_time":"2026-10-19T10:20:00.5Z","priority":"3","unit":"nginx.service","pid":"42","peer":"agent-paris","site":"paris"}
{"_msg":"oldest","_time":"2026-10-19T10:00:00Z","priority":"4"}
"#;
const UNITS: &str = r#"{"unit":"ssh.service","lines":"1"}
{"unit":"nginx.service","lines":"3"}
"#;

/// The query pairs of one request.
type Params = Vec<(String, String)>;

#[derive(Clone, Default)]
struct FakeVl {
    url: String,
    /// The query pairs of every request.
    seen: Arc<Mutex<Vec<Params>>>,
    status: Arc<Mutex<u16>>,
}

impl FakeVl {
    async fn start() -> Self {
        async fn answer(
            State(vl): State<FakeVl>,
            OriginalUri(uri): OriginalUri,
        ) -> (StatusCode, String) {
            let url = reqwest::Url::parse(&format!("http://fake{uri}")).unwrap();
            assert_eq!(url.path(), "/select/logsql/query");
            let params: Params = url.query_pairs().into_owned().collect();
            let units = params
                .iter()
                .any(|(k, v)| k == "query" && v.contains("stats by (unit)"));
            vl.seen.lock().unwrap().push(params);
            let status = StatusCode::from_u16(*vl.status.lock().unwrap()).unwrap();
            (status, if units { UNITS } else { LINES }.to_owned())
        }
        let mut vl = Self {
            status: Arc::new(Mutex::new(200)),
            ..Self::default()
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        vl.url = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().fallback(answer).with_state(vl.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        vl
    }

    fn seen(&self) -> Vec<Params> {
        self.seen.lock().unwrap().clone()
    }
}

fn param<'a>(params: &'a [(String, String)], name: &str) -> &'a str {
    params
        .iter()
        .find(|(k, _)| k == name)
        .map_or("", |(_, v)| v.as_str())
}

fn app(pool: &PgPool, vl: Option<&FakeVl>) -> Router {
    let mut state = ApiState::new(pool.clone(), Registry::default(), ApiSettings::default());
    if let Some(vl) = vl {
        state =
            state.with_logs(VmQuery::with_timeout(&vl.url, Duration::from_millis(500)).unwrap());
    }
    api::router(state)
}

async fn send(router: Router, request: Request<Body>) -> (StatusCode, Value, Option<String>) {
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    let cookie = response
        .headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body, cookie)
}

/// A viewer of `site`, logged in.
async fn session(pool: &PgPool, username: &str, site: Uuid) -> String {
    let id = users::create_user(pool, username, &Secret::new(PASSWORD), Role::Viewer, "test")
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
    let (status, _, cookie) = send(app(pool, None), request).await;
    assert_eq!(status, StatusCode::OK);
    cookie.unwrap()
}

async fn get(pool: &PgPool, vl: Option<&FakeVl>, cookie: &str, uri: &str) -> (StatusCode, Value) {
    let request = Request::get(uri)
        .header("cookie", cookie)
        .body(Body::empty())
        .unwrap();
    let (status, body, _) = send(app(pool, vl), request).await;
    (status, body)
}

/// A server in each of the sites `paris` and `lyon`.
async fn fleet(pool: &PgPool) -> [(Uuid, Uuid); 2] {
    let mut out = [(Uuid::nil(), Uuid::nil()); 2];
    for (i, (site, peer)) in [("paris", "agent-paris"), ("lyon", "agent-lyon")]
        .into_iter()
        .enumerate()
    {
        let site: Uuid = sqlx::query_scalar("INSERT INTO sites (name) VALUES ($1) RETURNING id")
            .bind(site)
            .fetch_one(pool)
            .await
            .unwrap();
        let server: Uuid = sqlx::query_scalar(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family)
             VALUES ($1, $2, $1, 'debian') RETURNING id",
        )
        .bind(peer)
        .bind(site)
        .fetch_one(pool)
        .await
        .unwrap();
        out[i] = (site, server);
    }
    out
}

db_tests! {
    #[sqlx::test]
    async fn logs_are_queried_on_the_servers_stream_and_read_newest_first(pool: PgPool) {
        let vl = FakeVl::start().await;
        let [(paris, paris_server), (_, lyon_server)] = fleet(&pool).await;
        let viewer = session(&pool, "paris-viewer", paris).await;

        let uri = format!(
            "/api/servers/{paris_server}/logs?start=2026-10-19T10:00:00Z&end=2026-10-19T11:00:00Z\
             &level=error&unit=nginx.service&q=Disk%20(full)&limit=2"
        );
        let (status, body) = get(&pool, Some(&vl), &viewer, &uri).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body,
            json!({
                "lines": [
                    { "time": "2026-10-19T10:20:00.500Z", "message": "disk full", "priority": 3,
                      "unit": "nginx.service", "identifier": null, "pid": 42 },
                    { "time": "2026-10-19T10:10:00Z", "message": "older", "priority": 4,
                      "unit": null, "identifier": "kernel", "pid": null },
                ],
                "more": true,
                "units": ["nginx.service", "ssh.service"],
            })
        );

        let seen = vl.seen();
        assert_eq!(seen.len(), 2);
        let stream = r#"{peer="agent-paris",site="paris"}"#;
        for params in &seen {
            assert_eq!(param(params, "extra_stream_filters"), stream);
            assert_eq!(param(params, "start"), "2026-10-19T10:00:00.000Z");
            assert_eq!(param(params, "end"), "2026-10-19T11:00:00.000Z");
            assert!(param(params, "query").starts_with(&format!("{stream} source:=\"journal\"")));
        }
        let lines = seen.iter().map(|p| param(p, "query")).find(|q| !q.contains("stats")).unwrap();
        assert_eq!(
            lines,
            format!(
                r#"{stream} source:="journal" priority:<=3 unit:="nginx.service" _msg:~"(?i)Disk \\(full\\)" | sort by (_time desc) | limit 3"#
            )
        );

        // Lyon's server does not exist for a Paris viewer; nothing is sent.
        let (status, _) = get(&pool, Some(&vl), &viewer, &format!("/api/servers/{lyon_server}/logs")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = get(&pool, Some(&vl), &viewer, &format!("/api/servers/{}/logs", Uuid::new_v4())).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(vl.seen().len(), 2);
    }

    #[sqlx::test]
    async fn bad_requests_and_backend_failures_say_so(pool: PgPool) {
        let vl = FakeVl::start().await;
        let [(paris, server), _] = fleet(&pool).await;
        let viewer = session(&pool, "paris-viewer", paris).await;
        for query in ["limit=0", "limit=1001", "level=debug", "start=2026-10-19T11:00:00Z&end=2026-10-19T10:00:00Z", "start=2026-08-01T00:00:00Z&end=2026-10-01T00:00:00Z", "query=*"] {
            let (status, body) = get(&pool, Some(&vl), &viewer, &format!("/api/servers/{server}/logs?{query}")).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{query}: {body}");
        }
        assert!(vl.seen().is_empty());

        *vl.status.lock().unwrap() = 503;
        let (status, body) = get(&pool, Some(&vl), &viewer, &format!("/api/servers/{server}/logs")).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::BAD_GATEWAY, Some("logs backend unavailable")));
        let (status, _) = get(&pool, None, &viewer, &format!("/api/servers/{server}/logs")).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
    }
}
