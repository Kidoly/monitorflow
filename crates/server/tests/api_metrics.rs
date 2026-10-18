//! Metric query proxy against a real PostgreSQL and an in-process fake
//! VictoriaMetrics that records every request: the exact query and filters
//! sent, site scoping, limits and backend errors. Ignored without
//! `DATABASE_URL`; the last test also needs `VICTORIAMETRICS_URL`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{OriginalUri, State};
use axum::http::{HeaderMap, Request, StatusCode};
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

/// One request the fake received.
#[derive(Debug, Clone)]
struct Seen {
    path: String,
    params: Vec<(String, String)>,
    headers: HeaderMap,
}

impl Seen {
    fn param(&self, name: &str) -> Vec<&str> {
        self.params
            .iter()
            .filter(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
            .collect()
    }
}

#[derive(Default)]
struct Inner {
    seen: Mutex<Vec<Seen>>,
    /// Answers for the next requests; an empty success once empty.
    script: Mutex<VecDeque<(StatusCode, Value)>>,
    delay: Mutex<Duration>,
}

#[derive(Clone)]
struct FakeVm {
    url: String,
    inner: Arc<Inner>,
}

impl FakeVm {
    async fn start() -> Self {
        let inner = Arc::new(Inner::default());
        let app = Router::new().fallback(answer).with_state(inner.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { url, inner }
    }

    fn reply(&self, status: u16, body: Value) {
        let status = StatusCode::from_u16(status).unwrap();
        self.inner.script.lock().unwrap().push_back((status, body));
    }

    fn success(&self, data: Value) {
        self.reply(200, json!({ "status": "success", "data": data }));
    }

    fn seen(&self) -> Vec<Seen> {
        self.inner.seen.lock().unwrap().clone()
    }
}

async fn answer(
    State(inner): State<Arc<Inner>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> (StatusCode, String) {
    let url = reqwest::Url::parse(&format!("http://fake{uri}")).unwrap();
    inner.seen.lock().unwrap().push(Seen {
        path: url.path().to_owned(),
        params: url.query_pairs().into_owned().collect(),
        headers,
    });
    let delay = *inner.delay.lock().unwrap();
    tokio::time::sleep(delay).await;
    let (status, body) = inner.script.lock().unwrap().pop_front().unwrap_or((
        StatusCode::OK,
        json!({ "status": "success", "data": { "resultType": "matrix", "result": [] } }),
    ));
    (status, body.to_string())
}

fn app(pool: &PgPool, vm: &FakeVm) -> Router {
    let client = VmQuery::with_timeout(&vm.url, Duration::from_millis(500)).unwrap();
    api::router(
        ApiState::new(pool.clone(), Registry::default(), ApiSettings::default())
            .with_metrics(client),
    )
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
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body, cookie)
}

/// A user with one role, on `site` or globally, logged in.
async fn session(pool: &PgPool, vm: &FakeVm, username: &str, site: Option<Uuid>) -> String {
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
    let (status, _, cookie) = send(app(pool, vm), request).await;
    assert_eq!(status, StatusCode::OK);
    cookie.unwrap()
}

async fn get(pool: &PgPool, vm: &FakeVm, cookie: &str, uri: &str) -> (StatusCode, Value) {
    let request = Request::get(uri)
        .header("cookie", cookie)
        .header("authorization", "Bearer not-for-vm")
        .body(Body::empty())
        .unwrap();
    let (status, body, _) = send(app(pool, vm), request).await;
    (status, body)
}

/// Sites `paris` and `o"lyon`, one server each.
async fn fleet(pool: &PgPool) -> [(Uuid, Uuid); 2] {
    let mut out = [(Uuid::nil(), Uuid::nil()); 2];
    for (i, (site, peer)) in [("paris", "agent-paris"), ("o\"lyon", "agent-lyon")]
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
    async fn range_queries_are_built_and_scoped_server_side(pool: PgPool) {
        let vm = FakeVm::start().await;
        let [(paris, paris_server), (_, lyon_server)] = fleet(&pool).await;
        let viewer = session(&pool, &vm, "paris-viewer", Some(paris)).await;
        vm.success(json!({ "resultType": "matrix", "result": [{
            "metric": { "__name__": "monitorflow_cpu_usage_ratio", "peer": "agent-paris",
                        "site": "paris", "instance": "total" },
            "values": [[1760000000, "0.25"], [1760000060, "0.5"]],
        }]}));

        let uri = format!(
            "/api/metrics/query_range?server_id={paris_server}&metric=monitorflow_cpu_usage_ratio\
             &instance=total&start=2026-10-19T10:00:00Z&end=2026-10-19T11:00:00Z&step=60&agg=avg"
        );
        let (status, body) = get(&pool, &vm, &viewer, &uri).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let query = r#"avg(monitorflow_cpu_usage_ratio{peer="agent-paris",site="paris",instance="total"})"#;
        assert_eq!(body["query"], query);
        assert_eq!(body["series"][0]["labels"]["instance"], "total");
        assert_eq!(body["series"][0]["points"], json!([[1760000000.0, 0.25], [1760000060.0, 0.5]]));

        let seen = vm.seen();
        assert_eq!(seen.len(), 1);
        let sent = &seen[0];
        assert_eq!(sent.path, "/api/v1/query_range");
        let mut params = sent.params.clone();
        params.sort();
        let expected: Vec<(String, String)> = [
            ("end", "1792407600"),
            ("extra_label", "peer=agent-paris"),
            ("extra_label", "site=paris"),
            ("query", query),
            ("start", "1792404000"),
            ("step", "60"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
        assert_eq!(params, expected);
        // Nothing of the browser's request reaches VictoriaMetrics.
        assert!(sent.headers.get("cookie").is_none());
        assert!(sent.headers.get("authorization").is_none());

        // Lyon's server does not exist for a Paris viewer; nothing is sent.
        for uri in [
            format!("/api/metrics/query_range?server_id={lyon_server}&metric=monitorflow_cpu_usage_ratio"),
            format!("/api/metrics/query?server_id={lyon_server}&metric=monitorflow_cpu_usage_ratio"),
            format!("/api/servers/{lyon_server}/metrics"),
            format!("/api/metrics/query?server_id={}&metric=monitorflow_cpu_usage_ratio", Uuid::new_v4()),
        ] {
            let (status, body) = get(&pool, &vm, &viewer, &uri).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}: {body}");
        }
        assert_eq!(vm.seen().len(), 1);
    }

    #[sqlx::test]
    async fn instant_queries_escape_site_names(pool: PgPool) {
        let vm = FakeVm::start().await;
        let [_, (lyon, lyon_server)] = fleet(&pool).await;
        let viewer = session(&pool, &vm, "lyon-viewer", Some(lyon)).await;
        vm.success(json!({ "resultType": "vector", "result": [
            { "metric": { "instance": "/" }, "value": [1760000000, "0.75"] }
        ]}));
        let uri = format!(
            "/api/metrics/query?server_id={lyon_server}&metric=monitorflow_disk_used_ratio\
             &instance=%2F&time=2026-10-19T10:00:00Z"
        );
        let (status, body) = get(&pool, &vm, &viewer, &uri).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["series"][0]["points"], json!([[1760000000.0, 0.75]]));
        let sent = &vm.seen()[0];
        assert_eq!(sent.path, "/api/v1/query");
        assert_eq!(
            sent.param("query"),
            [r#"monitorflow_disk_used_ratio{peer="agent-lyon",site="o\"lyon",instance="/"}"#]
        );
        assert_eq!(sent.param("time"), ["1792404000"]);
        assert_eq!(sent.param("extra_label"), ["peer=agent-lyon", "site=o\"lyon"]);
    }

    #[sqlx::test]
    async fn invalid_requests_never_reach_the_backend(pool: PgPool) {
        let vm = FakeVm::start().await;
        let [(_, paris_server), _] = fleet(&pool).await;
        let viewer = session(&pool, &vm, "viewer", None).await;
        let base = format!("/api/metrics/query_range?server_id={paris_server}");
        let cases = [
            (format!("{base}&metric=up"), "metric must be"),
            (format!("{base}&metric=monitorflow_cpu%7Bpeer%3D%22x%22%7D"), "metric must be"),
            (format!("{base}&metric=monitorflow_cpu&query=up"), "unknown field `query`"),
            (format!("{base}&metric=monitorflow_cpu&agg=quantile"), "unknown variant"),
            (format!("{base}&metric=monitorflow_cpu&instance=a%0Ab"), "instance must be"),
            (format!("{base}&metric=monitorflow_cpu&start=2026-01-01T00:00:00Z&end=2026-03-01T00:00:00Z"), "at most 31 days"),
            (format!("{base}&metric=monitorflow_cpu&start=2026-01-01T00:00:00Z&end=2026-01-02T00:00:00Z&step=1"), "points"),
            (format!("{base}&metric=monitorflow_cpu&step=0"), "step must be"),
            ("/api/metrics/query_range?server_id=nope&metric=monitorflow_cpu".to_owned(), "server_id"),
        ];
        for (uri, expected) in cases {
            let (status, body) = get(&pool, &vm, &viewer, &uri).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}: {body}");
            let message = body["error"].as_str().unwrap();
            assert!(message.contains(expected), "{uri}: {message}");
        }
        assert!(vm.seen().is_empty());
    }

    #[sqlx::test]
    async fn backend_errors_and_limits_map_to_clean_errors(pool: PgPool) {
        let vm = FakeVm::start().await;
        let [(_, server), _] = fleet(&pool).await;
        let viewer = session(&pool, &vm, "viewer", None).await;
        let uri = format!("/api/metrics/query?server_id={server}&metric=monitorflow_cpu_usage_ratio");

        vm.reply(503, json!({ "status": "error", "error": "storage at http://secret@vm down" }));
        let (status, body) = get(&pool, &vm, &viewer, &uri).await;
        assert_eq!((status, body), (StatusCode::BAD_GATEWAY, json!({ "error": "metrics backend unavailable" })));

        vm.reply(422, json!({ "status": "error", "error": "cannot select more than 300000 series" }));
        let (status, body) = get(&pool, &vm, &viewer, &uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().starts_with("metrics backend rejected the query"));

        vm.reply(200, json!({ "status": "success", "data": "garbage" }));
        let (status, _) = get(&pool, &vm, &viewer, &uri).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);

        let many: Vec<_> = (0..501)
            .map(|i| json!({ "metric": { "instance": i.to_string() }, "value": [1, "1"] }))
            .collect();
        vm.success(json!({ "resultType": "vector", "result": many }));
        let (status, body) = get(&pool, &vm, &viewer, &uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().contains("more than 500 series"), "{body}");

        *vm.inner.delay.lock().unwrap() = Duration::from_secs(2);
        let (status, body) = get(&pool, &vm, &viewer, &uri).await;
        assert_eq!((status, body), (StatusCode::GATEWAY_TIMEOUT, json!({ "error": "metrics backend timed out" })));

        // No backend configured.
        let router = api::router(ApiState::new(pool.clone(), Registry::default(), ApiSettings::default()));
        let request = Request::get(&uri).header("cookie", &viewer).body(Body::empty()).unwrap();
        let (status, _, _) = send(router, request).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
    }

    #[sqlx::test]
    async fn discovery_lists_metrics_and_instances_of_one_server(pool: PgPool) {
        let vm = FakeVm::start().await;
        let [(paris, server), _] = fleet(&pool).await;
        let viewer = session(&pool, &vm, "paris-viewer", Some(paris)).await;
        let series = |name: &str, peer: &str, instance: Option<&str>| {
            let mut labels = json!({ "__name__": name, "peer": peer, "site": "paris" });
            if let Some(i) = instance {
                labels["instance"] = json!(i);
            }
            labels
        };
        vm.success(json!([
            series("monitorflow_cpu_usage_ratio", "agent-paris", Some("total")),
            series("monitorflow_cpu_usage_ratio", "agent-paris", Some("0")),
            series("monitorflow_cpu_load1", "agent-paris", None),
            // A backend that ignored the filters must not leak this one.
            series("monitorflow_cpu_load1", "agent-other", Some("leak")),
        ]));
        let (status, body) = get(&pool, &vm, &viewer, &format!("/api/servers/{server}/metrics")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({ "truncated": false, "metrics": [
            { "name": "monitorflow_cpu_load1", "instances": [] },
            { "name": "monitorflow_cpu_usage_ratio", "instances": ["0", "total"] },
        ]}));
        let sent = &vm.seen()[0];
        assert_eq!(sent.path, "/api/v1/series");
        assert_eq!(
            sent.param("match[]"),
            [r#"{__name__=~"monitorflow_.+",peer="agent-paris",site="paris"}"#]
        );
        assert_eq!(sent.param("limit"), ["10000"]);
        assert_eq!(sent.param("extra_label"), ["peer=agent-paris", "site=paris"]);
        let start: i64 = sent.param("start")[0].parse().unwrap();
        let end: i64 = sent.param("end")[0].parse().unwrap();
        assert_eq!(end - start, 86_400);
    }

    /// Writes a series of two servers to the VictoriaMetrics at
    /// `VICTORIAMETRICS_URL` and reads one back through the API.
    #[sqlx::test]
    #[cfg_attr(
        not(vm_tests),
        ignore = "VICTORIAMETRICS_URL is not set; run `docker compose -f deploy/docker-compose.dev.yml up -d victoriametrics` and export it from .env.example"
    )]
    async fn real_victoriametrics_answers_scoped_queries(pool: PgPool) {
        use monitorflow_server::ingest::{RemoteWrite, Series, Sink};

        let base = std::env::var("VICTORIAMETRICS_URL").unwrap();
        let site_name = format!("api-test-{}", Uuid::new_v4().simple());
        let site: Uuid = sqlx::query_scalar("INSERT INTO sites (name) VALUES ($1) RETURNING id")
            .bind(&site_name)
            .fetch_one(&pool)
            .await
            .unwrap();
        let server: Uuid = sqlx::query_scalar(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family)
             VALUES ('agent-real', $1, 'h', 'debian') RETURNING id",
        )
        .bind(site)
        .fetch_one(&pool)
        .await
        .unwrap();
        // Older than VictoriaMetrics' latency offset (30 s), which hides
        // newer points from instant queries.
        let now = chrono::Utc::now().timestamp_millis() - 60_000;
        let batch: Vec<Series> = ["agent-real", "agent-other"]
            .iter()
            .map(|peer| Series {
                labels: [
                    ("__name__", "monitorflow_api_test_ratio"),
                    ("instance", "x"),
                    ("peer", peer),
                    ("site", site_name.as_str()),
                ]
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
                value: 0.5,
                timestamp_ms: now,
            })
            .collect();
        RemoteWrite::new(&base).unwrap().write(&batch).await.unwrap();

        let client = reqwest::Client::new();
        let router = || {
            api::router(
                ApiState::new(pool.clone(), Registry::default(), ApiSettings::default())
                    .with_metrics(VmQuery::new(&base).unwrap()),
            )
        };
        let vm = FakeVm::start().await;
        let viewer = session(&pool, &vm, "viewer", Some(site)).await;
        let uri = format!("/api/metrics/query?server_id={server}&metric=monitorflow_api_test_ratio");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            client.get(format!("{base}/internal/force_flush")).send().await.unwrap();
            let request = Request::get(&uri).header("cookie", &viewer).body(Body::empty()).unwrap();
            let (status, body, _) = send(router(), request).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let series = body["series"].as_array().unwrap();
            if !series.is_empty() {
                assert_eq!(series.len(), 1, "{body}");
                assert_eq!(series[0]["labels"]["peer"], "agent-real");
                assert_eq!(series[0]["points"][0][1], 0.5);
                return;
            }
            assert!(tokio::time::Instant::now() < deadline, "series not found");
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}
