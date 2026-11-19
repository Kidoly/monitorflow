//! Richer alert rules through the engine and PostgreSQL (ADR-0153):
//! windows, forecasts with history, comparisons with the past, no data and
//! conditions joined with "and", beside a plain threshold of the same
//! series.

use std::sync::Arc;
use std::time::Duration;

use monitorflow_common::proto::v1::{Sample, SampleBatch};
use monitorflow_server::alerting::history::{Backend, Fetch};
use monitorflow_server::alerting::{AlertStore, Alerts, EngineSettings, LogNotifier};
use monitorflow_server::ingest::{check_batch, Identity};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

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

const S: i64 = 1000;
const MIN: i64 = 60 * S;

fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

async fn server(pool: &PgPool, peer: &str) -> Uuid {
    let site: Uuid = sqlx::query_scalar("INSERT INTO sites (name) VALUES ('paris') RETURNING id")
        .fetch_one(pool)
        .await
        .unwrap();
    sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family)
         VALUES ($1, $2, $1, 'debian') RETURNING id",
    )
    .bind(peer)
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Inserts a threshold row of `server` from JSON columns.
async fn rule(pool: &PgPool, server: Uuid, row: Value) {
    sqlx::query(
        "INSERT INTO thresholds (server_id, metric, instance, comparison, warning, critical,
                                 duration_s, hysteresis, kind, function, window_s, offset_s,
                                 target, conditions)
         SELECT $1, r.metric, r.instance, r.comparison, r.warning, r.critical,
                coalesce(r.duration_s, 0), coalesce(r.hysteresis, 0), coalesce(r.kind, 'value'),
                r.function, r.window_s, r.offset_s, r.target, coalesce(r.conditions, '[]')
         FROM jsonb_to_record($2) AS r(metric text, instance text, comparison text,
              warning float8, critical float8, duration_s int, hysteresis float8, kind text,
              function text, window_s int, offset_s int, target float8, conditions jsonb)",
    )
    .bind(server)
    .bind(row)
    .execute(pool)
    .await
    .unwrap();
}

/// Answers history queries from a function of the query text.
type Answerer = Box<dyn Fn(&str, &str) -> Value + Send + Sync>;

struct Fake(Answerer);

impl Backend for Fake {
    fn get<'a>(&'a self, path: &'a str, params: &'a [(&'a str, String)]) -> Fetch<'a> {
        let query = params
            .iter()
            .find(|(k, _)| *k == "query" || *k == "match[]")
            .map(|(_, v)| v.as_str())
            .unwrap_or_default();
        let answer = (self.0)(path, query);
        Box::pin(async move { Ok(answer) })
    }
}

/// A matrix answer of one series of `web-1`.
fn matrix(instance: &str, points: &[(i64, f64)]) -> Value {
    let values: Vec<Value> = points
        .iter()
        .map(|(t, v)| json!([*t as f64 / 1000.0, v.to_string()]))
        .collect();
    json!({"resultType": "matrix", "result": [
        {"metric": {"peer": "web-1", "site": "paris", "instance": instance}, "values": values}
    ]})
}

async fn engine(pool: &PgPool, history: Option<Fake>) -> Alerts {
    let settings = EngineSettings {
        poll_interval: Duration::from_millis(50),
        history: history.map(|h| Arc::new(h) as Arc<dyn Backend>),
        ..EngineSettings::default()
    };
    Alerts::start(
        AlertStore::new(pool.clone()),
        settings,
        Arc::new(LogNotifier),
    )
    .await
    .unwrap()
}

/// Submits `(sensor metric, instance, Unix ms, value)` samples of `web-1`.
async fn feed(alerts: &Alerts, samples: &[(&str, &str, i64, f64)]) {
    let identity = Identity {
        peer: "web-1".into(),
        site: "paris".into(),
    };
    for &(sensor, instance, at, value) in samples {
        let batch = SampleBatch {
            samples: vec![Sample {
                sensor: sensor.into(),
                instance: instance.into(),
                timestamp_unix_ms: at,
                value,
                labels: Default::default(),
            }],
        };
        let checked = check_batch(&identity, batch, now());
        assert_eq!(checked.invalid, 0, "{sensor}");
        assert!(alerts.submit(&checked.series));
    }
    alerts.flush().await;
}

#[derive(Debug, PartialEq, sqlx::FromRow)]
struct Incident {
    kind: String,
    instance: String,
    severity: String,
    state: String,
    last_value: f64,
}

async fn incidents(pool: &PgPool) -> Vec<Incident> {
    sqlx::query_as(
        "SELECT kind, instance, severity, state, last_value FROM incidents
         ORDER BY opened_at, kind",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

/// Waits until `check` holds, up to 5 s.
async fn until<F: Fn(&[Incident]) -> bool>(pool: &PgPool, check: F) -> Vec<Incident> {
    for _ in 0..100 {
        let found = incidents(pool).await;
        if check(&found) {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    incidents(pool).await
}

db_tests! {
    #[sqlx::test(migrations = "./migrations")]
    async fn a_window_average_ignores_a_spike_and_catches_a_sustained_load(pool: PgPool) {
        let web = server(&pool, "web-1").await;
        rule(&pool, web, json!({
            "metric": "monitorflow_cpu_load1", "instance": "", "comparison": ">",
            "warning": 4.0, "kind": "window", "function": "avg", "window_s": 600,
        })).await;
        let alerts = engine(&pool, None).await;
        let t0 = now() - 40 * MIN;
        // Ten quiet minutes, then one minute at 20: the average is 2.7.
        let mut samples: Vec<_> = (0..=10).map(|i| ("cpu_load1", "", t0 + i * MIN, 1.0)).collect();
        samples.push(("cpu_load1", "", t0 + 11 * MIN, 20.0));
        samples.push(("cpu_load1", "", t0 + 12 * MIN, 1.0));
        feed(&alerts, &samples).await;
        assert_eq!(incidents(&pool).await, []);
        // A sustained load of 6 lifts the average past 4 within minutes.
        let sustained: Vec<_> =
            (13..=25).map(|i| ("cpu_load1", "", t0 + i * MIN, 6.0)).collect();
        feed(&alerts, &sustained).await;
        let found = incidents(&pool).await;
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!((found[0].kind.as_str(), found[0].state.as_str()), ("window", "open"));
        assert!(found[0].last_value > 4.0 && found[0].last_value <= 6.0);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn a_forecast_opens_from_history_beside_the_plain_threshold(pool: PgPool) {
        let web = server(&pool, "web-1").await;
        rule(&pool, web, json!({
            "metric": "monitorflow_disk_used_ratio", "comparison": ">=", "warning": 259200.0,
            "critical": 86400.0, "kind": "forecast", "window_s": 3600, "target": 1.0,
        })).await;
        rule(&pool, web, json!({
            "metric": "monitorflow_disk_used_ratio", "comparison": ">", "warning": 0.85,
            "critical": 0.95,
        })).await;
        let start = now() - 60 * MIN;
        // The last hour from VictoriaMetrics: 50% to 60%, 10% an hour.
        let history: Vec<_> =
            (0..60).map(|i| (start + i * MIN, 0.5 + 0.1 * i as f64 / 60.0)).collect();
        let answer = matrix("/", &history);
        let alerts = engine(&pool, Some(Fake(Box::new(move |path, query| {
            assert_eq!(path, "/api/v1/query_range");
            assert!(query.starts_with("avg_over_time(monitorflow_disk_used_ratio{"), "{query}");
            answer.clone()
        })))).await;
        feed(&alerts, &[("disk_used_ratio", "/", now() - 2 * S, 0.6)]).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        feed(&alerts, &[("disk_used_ratio", "/", now(), 0.6)]).await;
        let found = until(&pool, |f| !f.is_empty()).await;
        assert_eq!(found.len(), 1, "only the forecast: the disk is below 85%");
        assert_eq!((found[0].kind.as_str(), found[0].severity.as_str()), ("forecast", "critical"));
        // Four hours left at 10% an hour.
        assert!((found[0].last_value - 4.0 * 3600.0).abs() < 600.0, "{:?}", found[0]);
        // Past 85% the plain threshold opens its own incident too.
        feed(&alerts, &[("disk_used_ratio", "/", now() + S, 0.97)]).await;
        let kinds: Vec<_> = incidents(&pool).await.into_iter().map(|i| i.kind).collect();
        assert_eq!(kinds.len(), 2, "{kinds:?}");
        assert!(kinds.contains(&"value".to_owned()));
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn a_comparison_with_last_week_needs_its_past(pool: PgPool) {
        let web = server(&pool, "web-1").await;
        rule(&pool, web, json!({
            "metric": "monitorflow_net_received_bytes_per_second", "instance": "eth0",
            "comparison": ">", "warning": 2.0, "kind": "compare", "function": "ratio",
            "window_s": 300, "offset_s": 604800,
        })).await;
        let alerts = engine(&pool, Some(Fake(Box::new(|_, query| {
            assert!(query.contains("[300s]"), "{query}");
            let from = now() - 604_800 * S - 3 * 3_600 * S;
            matrix("eth0", &(0..72).map(|i| (from + i * 5 * MIN, 100.0)).collect::<Vec<_>>())
        })))).await;
        let t0 = now() - 10 * MIN;
        // The first sample asks for last week and judges nothing yet.
        feed(&alerts, &[("net_received_bytes_per_second", "eth0", t0, 350.0)]).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let samples: Vec<_> = (1..=6)
            .map(|i| ("net_received_bytes_per_second", "eth0", t0 + i * MIN, 350.0))
            .collect();
        feed(&alerts, &samples).await;
        let found = incidents(&pool).await;
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].kind, "compare");
        assert!((found[0].last_value - 3.5).abs() < 1e-9);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn no_data_opens_on_silence_and_a_sample_resolves_it(pool: PgPool) {
        let web = server(&pool, "web-1").await;
        // One second, so the test does not wait minutes; the API asks 60.
        rule(&pool, web, json!({
            "metric": "monitorflow_port_443_up", "instance": "", "comparison": ">",
            "warning": 1.0, "kind": "nodata",
        })).await;
        let alerts = engine(&pool, None).await;
        feed(&alerts, &[("port_443_up", "", now(), 1.0)]).await;
        let found = until(&pool, |f| !f.is_empty()).await;
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!((found[0].kind.as_str(), found[0].state.as_str()), ("nodata", "open"));
        assert!(found[0].last_value > 1.0);
        feed(&alerts, &[("port_443_up", "", now(), 1.0)]).await;
        let found = until(&pool, |f| f.iter().all(|i| i.state == "resolved")).await;
        assert_eq!(found[0].state, "resolved");
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn a_rule_naming_its_instance_watches_it_before_any_sample(pool: PgPool) {
        let web = server(&pool, "web-1").await;
        rule(&pool, web, json!({
            "metric": "monitorflow_service_nginx_up", "instance": "", "comparison": ">",
            "critical": 1.0, "kind": "nodata",
        })).await;
        let alerts = engine(&pool, None).await;
        // Another check loads the server; nginx never reports.
        feed(&alerts, &[("cpu_load1", "", now(), 0.1)]).await;
        let found = until(&pool, |f| !f.is_empty()).await;
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!((found[0].kind.as_str(), found[0].severity.as_str()), ("nodata", "critical"));
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn conditions_joined_with_and_must_all_hold(pool: PgPool) {
        let web = server(&pool, "web-1").await;
        rule(&pool, web, json!({
            "metric": "monitorflow_cpu_usage_ratio", "instance": "total", "comparison": ">",
            "warning": 0.9, "conditions": [
                {"metric": "monitorflow_cpu_load1", "comparison": ">", "value": 8.0}
            ],
        })).await;
        let alerts = engine(&pool, None).await;
        let t0 = now() - 10 * MIN;
        // Busy processor, low load: nothing.
        feed(&alerts, &[
            ("cpu_load1", "", t0, 2.0),
            ("cpu_usage_ratio", "total", t0 + S, 0.95),
        ]).await;
        assert_eq!(incidents(&pool).await, []);
        // Both past their limits: open.
        feed(&alerts, &[
            ("cpu_load1", "", t0 + MIN, 12.0),
            ("cpu_usage_ratio", "total", t0 + MIN + S, 0.95),
        ]).await;
        let found = incidents(&pool).await;
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].instance.as_str(), found[0].state.as_str()), ("total", "open"));
        // The load drops: the rule no longer holds and resolves.
        feed(&alerts, &[
            ("cpu_load1", "", t0 + 2 * MIN, 1.0),
            ("cpu_usage_ratio", "total", t0 + 2 * MIN + S, 0.95),
        ]).await;
        assert_eq!(incidents(&pool).await[0].state, "resolved");
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn a_restart_reloads_the_window_from_history(pool: PgPool) {
        let web = server(&pool, "web-1").await;
        rule(&pool, web, json!({
            "metric": "monitorflow_cpu_load1", "instance": "", "comparison": ">",
            "warning": 4.0, "kind": "window", "function": "min", "window_s": 600,
        })).await;
        // Ten minutes of load 6 in VictoriaMetrics before the start.
        let from = now() - 11 * MIN;
        let history: Vec<_> = (0..11).map(|i| (from + i * MIN, 6.0)).collect();
        let answer = matrix("", &history);
        let alerts = engine(&pool, Some(Fake(Box::new(move |_, query| {
            assert!(query.starts_with("min_over_time("), "{query}");
            answer.clone()
        })))).await;
        feed(&alerts, &[("cpu_load1", "", now() - 2 * S, 6.0)]).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        // Without history a fresh window would wait five minutes.
        feed(&alerts, &[("cpu_load1", "", now(), 6.0)]).await;
        let found = incidents(&pool).await;
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].kind, "window");
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn a_short_window_stays_precise_beside_a_long_forecast(pool: PgPool) {
        let web = server(&pool, "web-1").await;
        // A forecast learned over a day on the same metric: its buckets
        // are 24 minutes wide, which must not blur the 3-minute window.
        rule(&pool, web, json!({
            "metric": "monitorflow_disk_used_ratio", "comparison": ">=", "warning": 259200.0,
            "kind": "forecast", "window_s": 86400, "target": 1.0,
        })).await;
        rule(&pool, web, json!({
            "metric": "monitorflow_disk_used_ratio", "instance": "/spike", "comparison": ">",
            "critical": 0.5, "kind": "window", "function": "avg", "window_s": 180,
        })).await;
        let alerts = engine(&pool, None).await;
        let t0 = now() - 20 * MIN;
        let quiet: Vec<_> = (0..60).map(|i| ("disk_used_ratio", "/spike", t0 + i * 10 * S, 0.0)).collect();
        feed(&alerts, &quiet).await;
        // 0.9 from t0 + 10 min: the 3-minute average passes 0.5 after
        // 100 s, whatever the other summary's buckets.
        let held: Vec<_> = (60..80).map(|i| ("disk_used_ratio", "/spike", t0 + i * 10 * S, 0.9)).collect();
        feed(&alerts, &held).await;
        let opened: chrono::DateTime<chrono::Utc> =
            sqlx::query_scalar("SELECT opened_at FROM incidents WHERE kind = 'window'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let after = opened.timestamp_millis() - (t0 + 60 * 10 * S);
        assert!((90 * S..=120 * S).contains(&after), "opened {after} ms after the load");
    }
}
