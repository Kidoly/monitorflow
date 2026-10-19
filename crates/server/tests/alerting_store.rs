//! Thresholds, incidents and alert states in PostgreSQL (ADR-0017):
//! precedence of threshold rows, one open incident under concurrency and
//! the state reloaded at start.

use monitorflow_server::alerting::store::{IncidentStep, Step};
use monitorflow_server::alerting::{AlertStore, SeriesKey, SeriesState, Severity, Transition};
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

/// A recent base time, so samples pass ingest validation.
fn base() -> i64 {
    chrono::Utc::now().timestamp_millis() - 3_600 * S
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

fn event(kind: &str, severity: Option<&str>) -> (String, Option<String>) {
    (kind.into(), severity.map(Into::into))
}

db_tests! {
    #[sqlx::test]
    async fn server_rows_override_site_rows_which_override_defaults(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let web = server(&pool, "web-01", paris).await;
        server(&pool, "db-01", paris).await;
        let store = AlertStore::new(pool.clone());
        assert!(store.load_rules("nobody").await.unwrap().is_none());

        let defaults = store.load_rules("web-01").await.unwrap().unwrap();
        assert_eq!((defaults.server_id, defaults.revision), (web, 0));
        assert_eq!(defaults.rules.lookup(DISK, "/").unwrap().warning, Some(0.85));

        threshold(&pool, (Some(paris), None), None, Some(0.7), 60, true).await;
        threshold(&pool, (Some(paris), None), Some("/boot"), None, 0, false).await;
        threshold(&pool, (None, Some(web)), None, Some(0.6), 30, true).await;
        let web_rules = store.load_rules("web-01").await.unwrap().unwrap();
        let db_rules = store.load_rules("db-01").await.unwrap().unwrap();
        let rule = web_rules.rules.lookup(DISK, "/").unwrap();
        assert_eq!((rule.warning, rule.duration_s), (Some(0.6), 30));
        assert_eq!(db_rules.rules.lookup(DISK, "/").unwrap().warning, Some(0.7));
        // The site mutes /boot for every server, whatever the instance-wide row.
        assert!(web_rules.rules.lookup(DISK, "/boot").is_none());
        assert!(db_rules.rules.lookup(DISK, "/boot").is_none());
        assert!(web_rules.revision > defaults.revision);
        let revisions = store.revisions(&[web]).await.unwrap();
        assert_eq!(revisions, [(web, web_rules.revision)]);
    }

    #[sqlx::test]
    async fn concurrent_opens_share_one_incident(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let web = server(&pool, "web-01", paris).await;
        let store = AlertStore::new(pool.clone());
        let rule = store.load_rules("web-01").await.unwrap().unwrap().rules.lookup(DISK, "/").unwrap().clone();
        let key = SeriesKey { server_id: web, metric: DISK.into(), instance: "/".into() };
        let state = SeriesState { firing: Some(Severity::Warning), warning_since: Some(0), critical_since: None, last_at: Some(base()) };
        let opens = (0..8).map(|i| {
            let (store, key, rule, state) = (store.clone(), key.clone(), rule.clone(), state.clone());
            tokio::spawn(async move {
                let step = Step {
                    key: &key,
                    at: base() + i,
                    value: Some(0.9 + i as f64 / 100.0),
                    incident: IncidentStep::Open { severity: Severity::Warning, rule: &rule },
                    transition: Some(Transition::Opened(Severity::Warning)),
                    reason: None,
                    state: Some(&state),
                };
                store.apply(&step).await.unwrap().unwrap()
            })
        });
        let mut ids = Vec::new();
        for open in opens.collect::<Vec<_>>() {
            ids.push(open.await.unwrap().0);
        }
        ids.dedup();
        assert_eq!(ids.len(), 1, "{ids:?}");
        let all = incidents(&pool).await;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].peak_value, 0.97);
        // Only the writer that inserted the row recorded its opening.
        assert_eq!(events(&pool, ids[0]).await, [event("opened", Some("warning"))]);
    }

    #[sqlx::test]
    async fn stored_states_and_open_incidents_reload(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let web = server(&pool, "web-01", paris).await;
        let store = AlertStore::new(pool.clone());
        let rule = store.load_rules("web-01").await.unwrap().unwrap().rules.lookup(DISK, "/").unwrap().clone();
        let t0 = base();
        let key = |instance: &str| SeriesKey { server_id: web, metric: DISK.into(), instance: instance.into() };
        let (root, var) = (key("/"), key("/var"));
        // / fires warning with a pending critical timer; /var is pending.
        let firing = SeriesState { firing: Some(Severity::Warning), warning_since: Some(t0), critical_since: Some(t0 + 60 * S), last_at: Some(t0 + 300 * S) };
        let step = Step {
            key: &root,
            at: t0 + 300 * S,
            value: Some(0.96),
            incident: IncidentStep::Open { severity: Severity::Warning, rule: &rule },
            transition: Some(Transition::Opened(Severity::Warning)),
            reason: None,
            state: Some(&firing),
        };
        let (id, peak) = store.apply(&step).await.unwrap().unwrap();
        assert_eq!(peak, 0.96);
        let update = Step { at: t0 + 360 * S, value: Some(0.94), incident: IncidentStep::Update { id, severity: Severity::Warning, peak }, transition: None, state: None, ..step };
        assert_eq!(store.apply(&update).await.unwrap(), None);
        let pending = SeriesState { warning_since: Some(t0), last_at: Some(t0), ..SeriesState::default() };
        store.apply(&Step { key: &var, at: t0, value: Some(0.9), incident: IncidentStep::None, transition: None, reason: None, state: Some(&pending) }).await.unwrap();

        let mut stored = store.load_states().await.unwrap();
        stored.sort_by(|a, b| a.key.cmp(&b.key));
        assert_eq!(stored.len(), 2);
        assert_eq!((&stored[0].key, stored[0].incident), (&root, Some((id, 0.96))));
        // The last sample time is the incident's last value, newer than the row.
        assert_eq!(stored[0].state, SeriesState { last_at: Some(t0 + 360 * S), ..firing });
        assert_eq!((&stored[1].key, &stored[1].state, stored[1].incident), (&var, &pending, None));

        // Resolving without a sample keeps the last value; an OK state
        // deletes the row.
        let ok = SeriesState::default();
        let resolve = Step { key: &root, at: t0, value: None, incident: IncidentStep::Resolve { id }, transition: Some(Transition::Resolved { from: Severity::Warning }), reason: Some("threshold_removed"), state: Some(&ok) };
        store.apply(&resolve).await.unwrap();
        let all = incidents(&pool).await;
        assert_eq!((all[0].state.as_str(), all[0].last_value), ("resolved", 0.94));
        assert_eq!(events(&pool, id).await, [event("opened", Some("warning")), event("resolved", None)]);
        let stored = store.load_states().await.unwrap();
        assert_eq!(stored.iter().map(|s| &s.key).collect::<Vec<_>>(), [&var]);
    }
}
