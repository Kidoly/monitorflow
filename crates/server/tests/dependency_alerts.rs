//! Dependencies in alerting (ADR-0152), against PostgreSQL and a chat
//! receiver on loopback: a parent going down is one incident and one
//! message, what is behind it opens nothing until after the parent is
//! back, and a child that failed a moment before its parent is never told.
//! Ignored without `DATABASE_URL`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::http::StatusCode;
use chrono::{TimeDelta, Utc};
use monitorflow_common::proto::v1::{Sample, SampleBatch};
use monitorflow_common::redact::Secret;
use monitorflow_common::tls::PeerRole;
use monitorflow_server::alerting::offline;
use monitorflow_server::alerting::{AlertStore, Alerts, EngineSettings, LogNotifier};
use monitorflow_server::ingest::{check_batch, Identity};
use monitorflow_server::notify::{self, Chat, Coverage, Destination, Dispatcher};
use serde_json::Value;
use sqlx::PgPool;
use tokio::net::TcpListener;
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

async fn site(pool: &PgPool) -> Uuid {
    let customer: Uuid =
        sqlx::query_scalar("INSERT INTO customers (name) VALUES ('acme') RETURNING id")
            .fetch_one(pool)
            .await
            .unwrap();
    sqlx::query_scalar("INSERT INTO sites (name, customer_id) VALUES ('paris', $1) RETURNING id")
        .bind(customer)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn agent(pool: &PgPool, site: Uuid, name: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family, last_seen_at)
         VALUES ($1, $2, $1, 'debian', now() - interval '10 minutes') RETURNING id",
    )
    .bind(name)
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn device(pool: &PgPool, site: Uuid, name: &str) -> Uuid {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family, kind, address, package_updates)
         VALUES ($1, $2, $1, 'device', 'device', '192.0.2.1', false) RETURNING id",
    )
    .bind(name)
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO sensor_configs (server_id, kind, name, interval_ms, probe)
         VALUES ($1, 'ping', 'ping', 60000, 'central')",
    )
    .bind(id)
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn depend(pool: &PgPool, child: Uuid, parent: Uuid) {
    sqlx::query("INSERT INTO dependencies (server_id, parent_server_id) VALUES ($1, $2)")
        .bind(child)
        .bind(parent)
        .execute(pool)
        .await
        .unwrap();
}

/// A critical ping incident of `device`, with its opening event, as the
/// engine writes them.
async fn ping_down(pool: &PgPool, device: Uuid) -> Uuid {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO incidents (server_id, metric, instance, severity, opened_at, last_value,
                                last_value_at, peak_value, threshold)
         VALUES ($1, 'monitorflow_ping_up', '', 'critical', now() - interval '10 minutes', 0,
                 now(), 0, '{}')
         RETURNING id",
    )
    .bind(device)
    .fetch_one(pool)
    .await
    .unwrap();
    event(pool, id, "opened", Some("critical")).await;
    id
}

async fn event(pool: &PgPool, incident: Uuid, kind: &str, severity: Option<&str>) {
    sqlx::query(
        "INSERT INTO incident_events (incident_id, occurred_at, kind, severity)
         VALUES ($1, now(), $2, $3)",
    )
    .bind(incident)
    .bind(kind)
    .bind(severity)
    .execute(pool)
    .await
    .unwrap();
}

/// A Slack channel for everything, recording each message's title.
async fn slack(pool: &PgPool) -> Arc<Mutex<Vec<Value>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let store = seen.clone();
    let app = axum::Router::new().fallback(move |body: Bytes| {
        let store = store.clone();
        async move {
            store
                .lock()
                .unwrap()
                .push(serde_json::from_slice(&body).unwrap_or(Value::Null));
            StatusCode::OK
        }
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let named = Chat {
        name: "#ops".into(),
        host: "hooks.slack.com".into(),
    };
    let url = format!("http://{address}/services/T0/B0/TOKEN");
    notify::insert(
        pool,
        &Destination::Slack(named),
        Some(&Secret::new(url)),
        Coverage::Everything,
        false,
    )
    .await
    .unwrap();
    seen
}

fn titles(seen: &Mutex<Vec<Value>>) -> Vec<String> {
    std::mem::take(&mut *seen.lock().unwrap())
        .iter()
        .map(|body| {
            body["blocks"][0]["text"]["text"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

fn dispatcher(pool: &PgPool) -> Dispatcher {
    Dispatcher::new(pool.clone(), "https://mf.example")
        .with_retries(Vec::new())
        .without_pace()
        .without_waits()
}

async fn open_offline(pool: &PgPool) -> Vec<Uuid> {
    sqlx::query_scalar(
        "SELECT server_id FROM incidents WHERE metric = 'offline' AND state = 'open'
         ORDER BY server_id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

db_tests! {
    #[sqlx::test]
    async fn a_host_going_down_is_one_incident_and_one_message(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let paris = site(&pool).await;
        let chat = slack(&pool).await;
        let host = agent(&pool, paris, "gw-host").await;
        let mut behind = Vec::new();
        for n in 0..5 {
            let web = agent(&pool, paris, &format!("web-{n}")).await;
            depend(&pool, web, host).await;
            behind.push(web);
        }
        // Everyone silent for ten minutes: only the host opens.
        let long_ago = Utc::now() - TimeDelta::days(1);
        let none = Default::default();
        let changes = offline::check(&pool, &none, long_ago, &Default::default()).await.unwrap();
        assert_eq!(changes.len(), 1, "{changes:?}");
        assert_eq!(open_offline(&pool).await, [host]);
        dispatcher(&pool).pass().await.unwrap();
        assert_eq!(
            titles(&chat),
            ["Critical: gw-host (acme / paris) is offline; 5 behind it are unreachable"]
        );
        // Later checks still open nothing for them.
        offline::check(&pool, &none, long_ago, &Default::default()).await.unwrap();
        assert_eq!(open_offline(&pool).await, [host]);

        // The host is back: its servers get five minutes to reconnect.
        let online = [((PeerRole::Agent, "gw-host".to_owned()), Utc::now())].into();
        let changes = offline::check(&pool, &online, long_ago, &Default::default()).await.unwrap();
        assert_eq!(changes.len(), 1, "{changes:?}");
        assert!(open_offline(&pool).await.is_empty());
        dispatcher(&pool).pass().await.unwrap();
        assert_eq!(titles(&chat), ["Resolved: gw-host (acme / paris) is back online"]);
        // Six minutes later, those still silent open their own.
        sqlx::query("UPDATE incidents SET resolved_at = now() - interval '6 minutes', opened_at = now() - interval '20 minutes'")
            .execute(&pool)
            .await
            .unwrap();
        let mut online = online;
        online.insert((PeerRole::Agent, "web-0".to_owned()), Utc::now());
        offline::check(&pool, &online, long_ago, &Default::default()).await.unwrap();
        let mut still = behind[1..].to_vec();
        still.sort();
        assert_eq!(open_offline(&pool).await, still);
    }

    #[sqlx::test]
    async fn a_child_that_fails_a_moment_before_is_never_told(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let paris = site(&pool).await;
        let chat = slack(&pool).await;
        let switch = device(&pool, paris, "sw-core").await;
        let printer = device(&pool, paris, "printer").await;
        depend(&pool, printer, switch).await;
        // The printer's ping fails first, the switch's a moment later.
        let child = ping_down(&pool, printer).await;
        let parent = ping_down(&pool, switch).await;
        dispatcher(&pool).pass().await.unwrap();
        assert_eq!(
            titles(&chat),
            ["Critical: ping_up on sw-core (acme / paris); 1 behind it is unreachable"]
        );
        let hidden: Option<Uuid> = sqlx::query_scalar("SELECT hidden_by FROM incidents WHERE id = $1")
            .bind(child)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(hidden, Some(parent));
        // While the switch is down, or for two minutes after, nothing more.
        dispatcher(&pool).pass().await.unwrap();
        assert!(titles(&chat).is_empty());
        sqlx::query("UPDATE incidents SET state = 'resolved', resolved_at = now() - interval '1 minute' WHERE id = $1")
            .bind(parent)
            .execute(&pool)
            .await
            .unwrap();
        dispatcher(&pool).pass().await.unwrap();
        assert!(titles(&chat).is_empty());
        // Three minutes after, the printer still fails: told now.
        sqlx::query("UPDATE incidents SET resolved_at = now() - interval '3 minutes' WHERE id = $1")
            .bind(parent)
            .execute(&pool)
            .await
            .unwrap();
        dispatcher(&pool).pass().await.unwrap();
        dispatcher(&pool).pass().await.unwrap();
        assert_eq!(titles(&chat), ["Critical: ping_up is 0 on printer (acme / paris)"]);
        let hidden: Option<Uuid> = sqlx::query_scalar("SELECT hidden_by FROM incidents WHERE id = $1")
            .bind(child)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(hidden, None);
        sqlx::query("UPDATE incidents SET state = 'resolved', resolved_at = now() WHERE id = $1")
            .bind(child)
            .execute(&pool)
            .await
            .unwrap();
        event(&pool, child, "resolved", None).await;
        dispatcher(&pool).pass().await.unwrap();
        assert_eq!(titles(&chat), ["Resolved: ping_up on printer (acme / paris)"]);
        // A child hidden that recovers before is never told at all.
        let again = ping_down(&pool, switch).await;
        let child = ping_down(&pool, printer).await;
        dispatcher(&pool).pass().await.unwrap();
        assert_eq!(titles(&chat).len(), 1);
        let hidden: Option<Uuid> = sqlx::query_scalar("SELECT hidden_by FROM incidents WHERE id = $1")
            .bind(child)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(hidden, Some(again));
        sqlx::query("UPDATE incidents SET state = 'resolved', resolved_at = now() WHERE id = $1")
            .bind(child)
            .execute(&pool)
            .await
            .unwrap();
        event(&pool, child, "resolved", None).await;
        dispatcher(&pool).pass().await.unwrap();
        assert!(titles(&chat).is_empty());
        // A child whose opening was told before keeps being told.
        let told = ping_down(&pool, printer).await;
        sqlx::query("DELETE FROM dependencies").execute(&pool).await.unwrap();
        dispatcher(&pool).pass().await.unwrap();
        assert_eq!(titles(&chat).len(), 1);
        depend(&pool, printer, switch).await;
        sqlx::query("UPDATE incidents SET state = 'resolved', resolved_at = now() WHERE id = $1")
            .bind(told)
            .execute(&pool)
            .await
            .unwrap();
        event(&pool, told, "resolved", None).await;
        dispatcher(&pool).pass().await.unwrap();
        assert_eq!(titles(&chat), ["Resolved: ping_up on printer (acme / paris)"]);
    }

    #[sqlx::test]
    async fn a_child_behind_a_parent_in_doubt_waits_for_it(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let paris = site(&pool).await;
        let chat = slack(&pool).await;
        // The host's agent has had no stream for ten minutes.
        let host = agent(&pool, paris, "gw-host").await;
        let printer = device(&pool, paris, "printer").await;
        depend(&pool, printer, host).await;
        let child = ping_down(&pool, printer).await;
        dispatcher(&pool).pass().await.unwrap();
        assert!(titles(&chat).is_empty());
        // The host was only restarting: the printer's incident is told.
        sqlx::query("UPDATE servers SET last_seen_at = now() WHERE id = $1")
            .bind(host)
            .execute(&pool)
            .await
            .unwrap();
        dispatcher(&pool).pass().await.unwrap();
        assert_eq!(titles(&chat), ["Critical: ping_up on printer (acme / paris)"]);
        let hidden: Option<Uuid> = sqlx::query_scalar("SELECT hidden_by FROM incidents WHERE id = $1")
            .bind(child)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(hidden, None);
    }

    #[sqlx::test]
    async fn the_engine_opens_nothing_behind_a_parent_down_and_judges_it_after(pool: PgPool) {
        let paris = site(&pool).await;
        let host = agent(&pool, paris, "gw-host").await;
        let web = agent(&pool, paris, "web-1").await;
        depend(&pool, web, host).await;
        sqlx::query(
            "INSERT INTO thresholds (server_id, metric, comparison, critical, duration_s)
             VALUES ($1, 'monitorflow_disk_used_ratio', '>', 0.9, 0)",
        )
        .bind(web)
        .execute(&pool)
        .await
        .unwrap();
        let down: Uuid = sqlx::query_scalar(
            "INSERT INTO incidents (server_id, metric, instance, severity, opened_at, last_value,
                                    last_value_at, peak_value, threshold)
             VALUES ($1, 'offline', '', 'critical', now() - interval '10 minutes', 0, now(), 0, '{}')
             RETURNING id",
        )
        .bind(host)
        .fetch_one(&pool)
        .await
        .unwrap();
        let settings = EngineSettings {
            poll_interval: Duration::from_millis(50),
            holds_interval: Duration::from_millis(50),
            ..EngineSettings::default()
        };
        let alerts = Alerts::start(AlertStore::new(pool.clone()), settings, Arc::new(LogNotifier))
            .await
            .unwrap();
        let t0 = Utc::now().timestamp_millis() - 600_000;
        let feed = |at: i64, value: f64| {
            let identity = Identity { peer: "web-1".into(), site: "paris".into() };
            let batch = SampleBatch {
                samples: vec![Sample {
                    sensor: "disk_used_ratio".into(),
                    instance: String::new(),
                    timestamp_unix_ms: t0 + at * 1000,
                    value,
                    labels: Default::default(),
                }],
            };
            check_batch(&identity, batch, Utc::now().timestamp_millis()).series
        };
        let incidents = || async {
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM incidents WHERE server_id = $1")
                .bind(web)
                .fetch_one(&pool)
                .await
                .unwrap()
        };
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(alerts.submit(&feed(0, 0.99)));
        alerts.flush().await;
        assert!(alerts.submit(&feed(10, 0.99)));
        alerts.flush().await;
        assert_eq!(incidents().await, 0);
        // Back one minute ago: still settling.
        sqlx::query("UPDATE incidents SET state = 'resolved', resolved_at = now() - interval '1 minute' WHERE id = $1")
            .bind(down)
            .execute(&pool)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(alerts.submit(&feed(20, 0.99)));
        alerts.flush().await;
        assert_eq!(incidents().await, 0);
        // Back three minutes ago: the next sample that still fails opens it.
        sqlx::query("UPDATE incidents SET resolved_at = now() - interval '3 minutes' WHERE id = $1")
            .bind(down)
            .execute(&pool)
            .await
            .unwrap();
        // The host's agent is back too, so it is not in doubt.
        sqlx::query("UPDATE servers SET last_seen_at = now() WHERE id = $1")
            .bind(host)
            .execute(&pool)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(alerts.submit(&feed(30, 0.99)));
        alerts.flush().await;
        assert_eq!(incidents().await, 1);
        let opened: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM incident_events e JOIN incidents i ON i.id = e.incident_id
             WHERE i.server_id = $1 AND e.kind = 'opened'",
        )
        .bind(web)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(opened, 1);
    }
}
