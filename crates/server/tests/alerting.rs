//! The threshold engine against PostgreSQL (ADR-0017): incidents opening,
//! escalating and resolving, restart reload, removed thresholds.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use monitorflow_common::proto::v1::{Sample, SampleBatch};
use monitorflow_server::alerting::{
    AlertStore, Alerts, EngineSettings, Notification, Notifier, Severity, Transition,
};
use monitorflow_server::ingest::{check_batch, Identity};
use sqlx::PgPool;
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

const DISK: &str = "monitorflow_disk_used_ratio";
const S: i64 = 1000;

async fn site(pool: &PgPool, name: &str) -> Uuid {
    sqlx::query_scalar("INSERT INTO sites (name) VALUES ($1) RETURNING id")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn server(pool: &PgPool, peer_id: &str, site_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family)
         VALUES ($1, $2, $1, 'debian') RETURNING id",
    )
    .bind(peer_id)
    .bind(site_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// A `>` threshold on the disk metric for a site or a server.
async fn threshold(
    pool: &PgPool,
    owner: (Option<Uuid>, Option<Uuid>),
    instance: Option<&str>,
    warning: Option<f64>,
    duration_s: i32,
    enabled: bool,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO thresholds (site_id, server_id, metric, instance, comparison, warning,
                                 critical, duration_s, enabled)
         VALUES ($1, $2, $3, $4, '>', $5, 0.95, $6, $7) RETURNING id",
    )
    .bind(owner.0)
    .bind(owner.1)
    .bind(DISK)
    .bind(instance)
    .bind(warning)
    .bind(duration_s)
    .bind(enabled)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Remembers transitions.
#[derive(Default)]
struct Recorder(Mutex<Vec<(String, Transition)>>);

impl Notifier for Recorder {
    fn notify(&self, n: &Notification<'_>) {
        self.0
            .lock()
            .unwrap()
            .push((n.key.instance.clone(), n.transition));
    }
}

fn settings() -> EngineSettings {
    EngineSettings {
        poll_interval: Duration::from_millis(50),
        ..EngineSettings::default()
    }
}

async fn engine(pool: &PgPool) -> (Alerts, Arc<Recorder>) {
    let recorder = Arc::new(Recorder::default());
    let alerts = Alerts::start(AlertStore::new(pool.clone()), settings(), recorder.clone())
        .await
        .unwrap();
    (alerts, recorder)
}

/// A recent base time, so samples pass ingest validation.
fn base() -> i64 {
    chrono::Utc::now().timestamp_millis() - 3_600 * S
}

/// Submits `(instance, seconds after t0, value)` disk samples of `peer`.
async fn feed(alerts: &Alerts, peer: &str, t0: i64, samples: &[(&str, i64, f64)]) {
    let identity = Identity {
        peer: peer.into(),
        site: "paris".into(),
    };
    for &(instance, t, value) in samples {
        let batch = SampleBatch {
            samples: vec![Sample {
                sensor: "disk_used_ratio".into(),
                instance: instance.into(),
                timestamp_unix_ms: t0 + t * S,
                value,
                labels: Default::default(),
            }],
        };
        let checked = check_batch(&identity, batch, chrono::Utc::now().timestamp_millis());
        assert_eq!(checked.invalid, 0);
        assert!(alerts.submit(&checked.series));
    }
    alerts.flush().await;
}

#[derive(Debug, PartialEq, sqlx::FromRow)]
struct Incident {
    id: Uuid,
    instance: String,
    severity: String,
    state: String,
    last_value: f64,
    peak_value: f64,
}

async fn incidents(pool: &PgPool) -> Vec<Incident> {
    sqlx::query_as(
        "SELECT id, instance, severity, state, last_value, peak_value FROM incidents
         ORDER BY opened_at, instance",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn events(pool: &PgPool, incident: Uuid) -> Vec<(String, Option<String>)> {
    sqlx::query_as("SELECT kind, severity FROM incident_events WHERE incident_id = $1 ORDER BY id")
        .bind(incident)
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn alert_states(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM alert_states")
        .fetch_one(pool)
        .await
        .unwrap()
}

fn event(kind: &str, severity: Option<&str>) -> (String, Option<String>) {
    (kind.into(), severity.map(Into::into))
}

db_tests! {
    #[sqlx::test]
    async fn incidents_open_escalate_and_resolve(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        server(&pool, "web-01", paris).await;
        let (alerts, recorder) = engine(&pool).await;
        let t0 = base();
        feed(&alerts, "web-01", t0, &[("/", 0, 0.9), ("/var", 0, 0.5), ("/", 200, 0.9)]).await;
        assert!(incidents(&pool).await.is_empty());
        assert_eq!(alert_states(&pool).await, 1, "the pending timer of /");
        feed(&alerts, "web-01", t0, &[("/", 300, 0.9), ("/", 400, 0.97), ("/", 700, 0.98), ("/", 760, 0.9), ("/", 820, 0.5)]).await;
        let all = incidents(&pool).await;
        assert_eq!(all.len(), 1);
        let incident = &all[0];
        assert_eq!((incident.instance.as_str(), incident.state.as_str(), incident.severity.as_str()), ("/", "resolved", "warning"));
        assert_eq!((incident.last_value, incident.peak_value), (0.5, 0.98));
        assert_eq!(
            events(&pool, incident.id).await,
            [
                event("opened", Some("warning")),
                event("escalated", Some("critical")),
                event("deescalated", Some("warning")),
                event("resolved", None),
            ]
        );
        assert_eq!(alert_states(&pool).await, 0);
        assert_eq!(recorder.0.lock().unwrap().len(), 4);
        // Unknown peers and metrics without thresholds are ignored.
        feed(&alerts, "relay-1", t0, &[("/", 900, 0.99)]).await;
        let stats = alerts.stats();
        assert_eq!((stats.transitions, stats.db_errors, stats.dropped), (4, 0, 0));
        // An old sample is counted, not evaluated.
        feed(&alerts, "web-01", t0, &[("/", 10, 0.99)]).await;
        assert_eq!(alerts.stats().out_of_order, 1);
    }

    #[sqlx::test]
    async fn a_restart_resumes_incidents_and_timers_without_duplicates(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        server(&pool, "web-01", paris).await;
        let t0 = base();
        let (alerts, _) = engine(&pool).await;
        // / fires; /var is pending since 0.
        feed(&alerts, "web-01", t0, &[("/", 0, 0.9), ("/", 300, 0.9), ("/var", 0, 0.9), ("/data", 0, 0.9)]).await;
        drop(alerts);

        let (alerts, recorder) = engine(&pool).await;
        // The old sample of / is still known to be old after the restart.
        feed(&alerts, "web-01", t0, &[("/", 300, 0.1)]).await;
        assert_eq!(alerts.stats().out_of_order, 1);
        // /var's timer resumes: 300 s after its start, it fires. /data comes
        // back after more than max_gap, so its timer restarts.
        feed(&alerts, "web-01", t0, &[("/var", 300, 0.9), ("/data", 1000, 0.9)]).await;
        let open: Vec<_> = incidents(&pool).await.into_iter().filter(|i| i.state == "open").collect();
        assert_eq!(open.iter().map(|i| i.instance.as_str()).collect::<Vec<_>>(), ["/", "/var"]);
        // / resolves on the incident opened before the restart.
        feed(&alerts, "web-01", t0, &[("/", 360, 0.1)]).await;
        let all = incidents(&pool).await;
        assert_eq!(all.len(), 2, "no duplicate incident");
        assert_eq!(all[0].state, "resolved");
        assert_eq!(events(&pool, all[0].id).await, [event("opened", Some("warning")), event("resolved", None)]);
        assert_eq!(
            *recorder.0.lock().unwrap(),
            [("/var".to_string(), Transition::Opened(Severity::Warning)), ("/".to_string(), Transition::Resolved { from: Severity::Warning })]
        );
    }

    #[sqlx::test]
    async fn removing_a_threshold_resolves_its_incidents(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let web = server(&pool, "web-01", paris).await;
        let row = threshold(&pool, (None, Some(web)), Some("/tmp"), Some(0.5), 0, true).await;
        let (alerts, _) = engine(&pool).await;
        feed(&alerts, "web-01", base(), &[("/tmp", 0, 0.6), ("/", 0, 0.9)]).await;
        assert_eq!(incidents(&pool).await.len(), 1);
        // Muting /tmp brings it back to the instance-wide default, which
        // 0.6 does not cross.
        sqlx::query("UPDATE thresholds SET enabled = false WHERE id = $1").bind(row).execute(&pool).await.unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while incidents(&pool).await[0].state == "open" {
            assert!(tokio::time::Instant::now() < deadline, "never resolved");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let id = incidents(&pool).await[0].id;
        let reason: serde_json::Value = sqlx::query_scalar(
            "SELECT details FROM incident_events WHERE incident_id = $1 AND kind = 'resolved'",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(reason["reason"], "threshold_removed");
        // The pending timer of / is untouched: its threshold is still there.
        assert_eq!(alert_states(&pool).await, 1);
    }
}
