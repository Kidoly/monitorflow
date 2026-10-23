//! Incident context gathering against a real PostgreSQL and an in-process
//! fake of VictoriaMetrics and VictoriaLogs that records every request:
//! what is read, the filters sent, bounds, redaction, and the refusal of
//! another customer's data. Ignored without `DATABASE_URL`; the last test
//! also needs `VICTORIALOGS_URL`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{OriginalUri, State};
use axum::http::StatusCode;
use axum::Router;
use chrono::{DateTime, Utc};
use monitorflow_server::remediation::context::{
    gather, Backends, ContextError, Limits, Section, Trigger,
};
use monitorflow_server::remediation::redact::Kind;
use monitorflow_server::vm_query::VmQuery;
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::net::TcpListener;
use uuid::Uuid;

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

const SECRET: &str = "hunter2-Sup3r";

/// Query parameters in order.
type Params = Vec<(String, String)>;

#[derive(Default)]
struct Inner {
    /// `(path, query parameters)` of every request.
    seen: Mutex<Vec<(String, Params)>>,
    /// The answer per path; an empty success without one.
    answers: Mutex<HashMap<String, (StatusCode, String)>>,
    delay: Mutex<Duration>,
}

/// Fake VictoriaMetrics and VictoriaLogs on one listener.
#[derive(Clone)]
struct Fake {
    url: String,
    inner: Arc<Inner>,
}

impl Fake {
    async fn start() -> Self {
        let inner = Arc::new(Inner::default());
        let app = Router::new().fallback(answer).with_state(inner.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { url, inner }
    }

    fn answer(&self, path: &str, status: u16, body: String) {
        let status = StatusCode::from_u16(status).unwrap();
        self.inner
            .answers
            .lock()
            .unwrap()
            .insert(path.to_owned(), (status, body));
    }

    fn series(&self, result: Value) {
        let body = json!({"status": "success", "data": {"resultType": "matrix", "result": result}});
        self.answer("/api/v1/query_range", 200, body.to_string());
    }

    fn logs(&self, lines: &[Value]) {
        let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
        self.answer("/select/logsql/query", 200, body);
    }

    fn params(&self, path: &str) -> Params {
        let seen = self.inner.seen.lock().unwrap();
        let (_, params) = seen.iter().find(|(p, _)| p == path).unwrap();
        params.clone()
    }

    fn backends(&self, pool: &PgPool) -> Backends {
        let client = VmQuery::with_timeout(&self.url, Duration::from_millis(500)).unwrap();
        Backends {
            pool: pool.clone(),
            metrics: Some(client.clone()),
            logs: Some(client),
        }
    }
}

async fn answer(
    State(inner): State<Arc<Inner>>,
    OriginalUri(uri): OriginalUri,
) -> (StatusCode, String) {
    let url = reqwest::Url::parse(&format!("http://fake{uri}")).unwrap();
    let path = url.path().to_owned();
    inner
        .seen
        .lock()
        .unwrap()
        .push((path.clone(), url.query_pairs().into_owned().collect()));
    let delay = *inner.delay.lock().unwrap();
    tokio::time::sleep(delay).await;
    inner
        .answers
        .lock()
        .unwrap()
        .get(&path)
        .cloned()
        .unwrap_or_else(|| {
            let body = json!({"status": "success", "data": {"resultType": "matrix", "result": []}});
            (StatusCode::OK, body.to_string())
        })
}

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-23T12:00:00Z")
        .unwrap()
        .to_utc()
}

/// A customer with one site and one server; returns `(customer, server)`.
async fn tenant(pool: &PgPool, name: &str) -> (Uuid, Uuid) {
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
        "INSERT INTO servers (peer_id, site_id, hostname, os_family, os_version, packages_reported_at)
         VALUES ($1, $2, $3, 'debian', '12', $4) RETURNING id",
    )
    .bind(format!("{name}-web"))
    .bind(site)
    .bind(format!("web.{name}.example"))
    .bind(now())
    .fetch_one(pool)
    .await
    .unwrap();
    (customer, server)
}

async fn incident(pool: &PgPool, server: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO incidents (server_id, metric, instance, severity, opened_at, last_value,
                                last_value_at, peak_value, threshold)
         VALUES ($1, 'monitorflow_disk_used_ratio', '/var', 'critical', $2, 0.97, $2, 0.99,
                 '{\"comparison\": \">\", \"critical\": 0.95}')
         RETURNING id",
    )
    .bind(server)
    .bind(now() - chrono::Duration::minutes(10))
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn command(pool: &PgPool, server: Uuid, minutes_ago: i64, stdout: &str) -> Uuid {
    let issued = now() - chrono::Duration::minutes(minutes_ago);
    sqlx::query_scalar(
        "INSERT INTO commands (server_id, kind, payload, state, issued_at, expires_at,
                               finished_at, exit_code, stdout, stderr)
         VALUES ($1, 'apply_updates', '', 'failed', $2, $2 + interval '1 hour', $2, 100, $3,
                 'E: dpkg was interrupted')
         RETURNING id",
    )
    .bind(server)
    .bind(issued)
    .bind(stdout)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// A running campaign of `customer` whose only target is `server`.
async fn campaign(pool: &PgPool, customer: Uuid, server: Uuid, name: &str) -> Uuid {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO campaigns (customer_id, name, state, started_at, packages, security_only)
         VALUES ($1, $2, 'running', $3, '{openssl}', true) RETURNING id",
    )
    .bind(customer)
    .bind(name)
    .bind(now() - chrono::Duration::hours(2))
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO campaign_waves (campaign_id, position) VALUES ($1, 0)")
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO campaign_targets (campaign_id, server_id, wave) VALUES ($1, $2, 0)")
        .bind(id)
        .bind(server)
        .execute(pool)
        .await
        .unwrap();
    id
}

fn log_line(peer: &str, site: &str, minutes_ago: i64, msg: &str) -> Value {
    let time = now() - chrono::Duration::minutes(minutes_ago);
    json!({
        "_time": time.to_rfc3339(), "_msg": msg, "_stream": "{}", "_stream_id": "0",
        "peer": peer, "site": site, "event": "peer.disconnect",
    })
}

db_tests! {
    #[sqlx::test]
    async fn gathers_one_customers_bounded_redacted_context(pool: PgPool) {
        let (acme, server) = tenant(&pool, "acme").await;
        let (globex, other) = tenant(&pool, "globex").await;
        let id = incident(&pool, server).await;
        for (i, name) in ["libc6", "openssl", "zlib1g"].iter().enumerate() {
            sqlx::query(
                "INSERT INTO server_packages (server_id, name, arch, version, upgradable_version)
                 VALUES ($1, $2, 'amd64', '1.0', $3)",
            )
            .bind(server)
            .bind(name)
            .bind(format!("1.{}", i + 1))
            .execute(&pool)
            .await
            .unwrap();
        }
        let long = format!("{}\npassword={SECRET}\nE: broken", "x".repeat(5000));
        for minutes in 1..=4 {
            command(&pool, server, minutes * 60, &long).await;
        }
        command(&pool, server, 60 * 24 * 30, "too old").await;
        command(&pool, other, 5, "globex output").await;
        campaign(&pool, acme, server, "acme october").await;
        campaign(&pool, globex, other, "globex october").await;

        let fake = Fake::start().await;
        let at = |minutes: i64| json!([(now() - chrono::Duration::minutes(minutes)).timestamp(), "0.97"]);
        fake.series(json!([{
            "metric": {"__name__": "monitorflow_disk_used_ratio", "instance": "/var",
                       "peer": "acme-web", "site": "acme-site"},
            "values": [at(15), at(10), at(5), at(0)],
        }]));
        fake.logs(&[
            log_line("acme-web", "acme-site", 3, &format!("stream closed token={SECRET}")),
            log_line("acme-web", "acme-site", 2, "stream opened"),
            log_line("acme-web", "acme-site", 1, "stream closed"),
        ]);

        let trigger = Trigger::of_incident(&pool, id).await.unwrap().unwrap();
        assert_eq!(trigger.server_id, server);
        let limits = Limits {
            commands: 3,
            output_bytes: 64,
            packages: 2,
            log_lines: 2,
            history_points: 3,
            ..Limits::default()
        };
        let context = gather(&fake.backends(&pool), &trigger, &limits, now()).await.unwrap();
        assert_eq!(context.owner.customer_id, acme);
        let text = context.render().unwrap();

        // One customer only.
        assert!(!text.contains("globex"), "{text}");
        assert!(text.contains("customer: acme\n"));
        assert!(text.contains("hostname: web.acme.example\n"));
        // Bounds and their markers.
        assert!(text.contains("packages_upgradable: 3\n  libc6 amd64 1.0 -> 1.1\n  openssl amd64 1.0 -> 1.2\n  [TRUNCATED: 1 packages]\n"), "{text}");
        let Section::Ready(changes) = &context.changes else { panic!() };
        assert_eq!((changes.commands.len(), changes.commands_omitted), (3, 1));
        assert!(text.contains("[TRUNCATED: 1 older commands]"));
        assert!(text.contains("    | [TRUNCATED: "));
        assert!(text.contains("    | E: broken\n"));
        assert!(text.contains("  name: acme october\n"));
        let Section::Ready(logs) = &context.logs else { panic!() };
        assert!(logs.truncated && logs.lines.len() == 2);
        assert!(text.contains("2026-10-23T11:59:00Z stream closed event=peer.disconnect\n"));
        let Section::Ready(history) = &context.history else { panic!() };
        assert_eq!((history.series[0].points.len(), history.series[0].points_omitted), (3, 1));
        // Redaction.
        assert!(!text.contains(SECRET));
        assert_eq!(context.redactions.get(Kind::Password), 3);
        assert!(text.contains("password: 3\n"));

        // Both backends were asked for this server only.
        let vm = fake.params("/api/v1/query_range");
        let get = |params: &[(String, String)], name: &str| -> Vec<String> {
            params.iter().filter(|(k, _)| k == name).map(|(_, v)| v.clone()).collect()
        };
        assert_eq!(
            get(&vm, "query"),
            ["max_over_time(monitorflow_disk_used_ratio{peer=\"acme-web\",site=\"acme-site\",instance=\"/var\"}[7200s])"]
        );
        assert_eq!(get(&vm, "extra_label"), ["peer=acme-web", "site=acme-site"]);
        let vl = fake.params("/select/logsql/query");
        assert_eq!(
            get(&vl, "query"),
            ["{peer=\"acme-web\",site=\"acme-site\"} | sort by (_time desc) | limit 3"]
        );
        assert_eq!(get(&vl, "start"), ["2026-10-22T12:00:00Z"]);
    }

    #[sqlx::test]
    async fn another_tenants_data_is_refused(pool: PgPool) {
        let (_, server) = tenant(&pool, "acme").await;
        let (globex, _) = tenant(&pool, "globex").await;
        let id = incident(&pool, server).await;
        let trigger = Trigger::of_incident(&pool, id).await.unwrap().unwrap();
        let fake = Fake::start().await;
        let backends = fake.backends(&pool);

        // A log line of another stream.
        fake.logs(&[log_line("globex-web", "globex-site", 1, "stream closed")]);
        let error = gather(&backends, &trigger, &Limits::default(), now()).await.unwrap_err();
        assert!(matches!(error, ContextError::ForeignData { section: "logs", .. }), "{error}");

        // A series of another server.
        fake.logs(&[]);
        fake.series(json!([{"metric": {"peer": "globex-web", "site": "globex-site"}, "values": []}]));
        let error = gather(&backends, &trigger, &Limits::default(), now()).await.unwrap_err();
        assert!(matches!(error, ContextError::ForeignData { section: "history", .. }), "{error}");

        // A campaign of another customer targeting this server, which the
        // API never creates.
        fake.series(json!([]));
        campaign(&pool, globex, server, "globex october").await;
        let error = gather(&backends, &trigger, &Limits::default(), now()).await.unwrap_err();
        assert!(matches!(error, ContextError::ForeignData { section: "changes", .. }), "{error}");
    }

    #[sqlx::test]
    async fn failing_backends_make_sections_unavailable(pool: PgPool) {
        let (_, server) = tenant(&pool, "acme").await;
        let id = incident(&pool, server).await;
        let trigger = Trigger::of_incident(&pool, id).await.unwrap().unwrap();

        let none = Backends { pool: pool.clone(), metrics: None, logs: None };
        let context = gather(&none, &trigger, &Limits::default(), now()).await.unwrap();
        assert_eq!(context.history, Section::Unavailable("VictoriaMetrics is not configured"));
        assert_eq!(context.logs, Section::Unavailable("VictoriaLogs is not configured"));
        assert!(matches!(context.changes, Section::Ready(_)));

        let fake = Fake::start().await;
        fake.answer("/select/logsql/query", 500, format!("password={SECRET}"));
        let limits = Limits { section_timeout: Duration::from_millis(200), ..Limits::default() };
        *fake.inner.delay.lock().unwrap() = Duration::from_millis(0);
        let context = gather(&fake.backends(&pool), &trigger, &limits, now()).await.unwrap();
        assert_eq!(context.logs, Section::Unavailable("the backend failed"));
        assert!(!context.render().unwrap().contains(SECRET));

        *fake.inner.delay.lock().unwrap() = Duration::from_millis(400);
        let context = gather(&fake.backends(&pool), &trigger, &limits, now()).await.unwrap();
        assert_eq!(context.history, Section::Unavailable("timed out"));

        let missing = Trigger { server_id: Uuid::new_v4(), ..trigger };
        assert!(matches!(
            gather(&none, &missing, &Limits::default(), now()).await,
            Err(ContextError::ServerNotFound(_))
        ));
        assert!(Trigger::of_incident(&pool, Uuid::new_v4()).await.unwrap().is_none());
    }
}

/// Writes log lines for two servers to the VictoriaLogs at
/// `VICTORIALOGS_URL` and reads one server's back into a context.
#[sqlx::test]
#[cfg_attr(
    not(all(db_tests, vl_tests)),
    ignore = "DATABASE_URL or VICTORIALOGS_URL is not set; run `docker compose -f deploy/docker-compose.dev.yml up -d` and export both from .env.example"
)]
async fn real_victorialogs_returns_one_servers_lines(pool: PgPool) {
    use monitorflow_server::ingest::{JsonLines, LogRecord, Sink};

    let base = std::env::var("VICTORIALOGS_URL").unwrap();
    let tag = Uuid::new_v4().simple().to_string();
    let (_, server) = tenant(&pool, &format!("ctx-{tag}")).await;
    let id = incident(&pool, server).await;
    let trigger = Trigger::of_incident(&pool, id).await.unwrap().unwrap();
    let site = format!("ctx-{tag}-site");
    let records: Vec<LogRecord> = [format!("ctx-{tag}-web"), format!("other-{tag}")]
        .into_iter()
        .map(|peer| {
            LogRecord::new(format!("stream closed password={SECRET}"))
                .field("peer", peer)
                .field("site", site.clone())
        })
        .collect();
    JsonLines::new(&base)
        .unwrap()
        .write(&records)
        .await
        .unwrap();

    let client = reqwest::Client::new();
    let backends = Backends {
        pool: pool.clone(),
        metrics: None,
        logs: Some(VmQuery::new(&base).unwrap()),
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        client
            .get(format!("{base}/internal/force_flush"))
            .send()
            .await
            .unwrap();
        let context = gather(&backends, &trigger, &Limits::default(), Utc::now())
            .await
            .unwrap();
        if let Section::Ready(logs) = &context.logs {
            if !logs.lines.is_empty() {
                assert_eq!(logs.lines.len(), 1);
                assert_eq!(
                    logs.lines[0].text.as_str(),
                    "stream closed password=[REDACTED:password]"
                );
                break;
            }
        }
        assert!(tokio::time::Instant::now() < deadline, "{:?}", context.logs);
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
