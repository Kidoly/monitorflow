//! The relay's mirror report (ADR-0037): sent on its own stream after
//! Hello and on every change, stored by the server per relay.

mod support;

use std::sync::Arc;
use std::time::Duration;

use monitorflow_common::client::backoff::Backoff;
use monitorflow_common::client::shutdown;
use monitorflow_common::config::RelayConfig;
use monitorflow_common::proto::v1::{RelayMirrors, ServedMirror};
use monitorflow_relay::control::{Control, ControlSettings};
use monitorflow_server::grpc::{AgentStreamService, DbInventory, DbRelayPolicy};
use sqlx::PgPool;
use support::{enroll_relay, serve_enroll, server_settings, wait_for, Pki};
use tokio::sync::watch;

fn report(url: &str, snapshots: &[&str]) -> RelayMirrors {
    RelayMirrors {
        public_url: url.into(),
        mirrors: vec![ServedMirror {
            family: "apt".into(),
            name: "debian-12".into(),
            current: snapshots.last().copied().unwrap_or_default().into(),
            snapshots: snapshots.iter().map(|s| (*s).to_owned()).collect(),
        }],
    }
}

/// The stored URL and snapshots of `debian-12` for the relay `paris-1`.
async fn stored(pool: &PgPool) -> (Option<String>, Option<Vec<String>>) {
    sqlx::query_as(
        "SELECT r.mirror_url, m.snapshots FROM relays r
         LEFT JOIN relay_mirrors m ON m.relay_id = r.id AND m.name = 'debian-12'
         WHERE r.peer_id = 'paris-1'",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

db_tests! {
    #[sqlx::test(migrations = "../server/migrations")]
    async fn the_server_follows_the_relays_mirror_report(pool: PgPool) {
        let pki = Pki::new();
        let enroll_url = serve_enroll(&pki, &pool).await;
        let identity = enroll_relay(&pki, &pool, &enroll_url, "paris-1", "paris", None).await;
        let upstream = pki
            .serve(|registry| {
                AgentStreamService::new(registry, Arc::new(DbRelayPolicy::new(pool.clone())), server_settings())
                    .with_inventory(Arc::new(DbInventory::new(pool.clone())))
            })
            .await;
        let config = RelayConfig {
            upstream_url: upstream.url.clone(),
            heartbeat_interval: 1,
            ..RelayConfig::default()
        };
        let settings = ControlSettings {
            backoff: Backoff::new(Duration::from_millis(50), Duration::from_millis(200)),
            ..ControlSettings::from_config(&config)
        };
        let (reports, served) = watch::channel(report("http://relay.example:7480", &["s1"]));
        let (_identity, identities) = watch::channel(identity);
        let (control, _up) = Control::new(settings, identities);
        let (_trigger, stop) = shutdown::channel();
        tokio::spawn(control.with_mirrors(served).run(stop));

        let url = Some("http://relay.example:7480".to_owned());
        let expect = |snapshots: &[&str]| (url.clone(), Some(snapshots.iter().map(|s| (*s).to_owned()).collect()));
        let want = expect(&["s1"]);
        wait_for("first report stored", || async { stored(&pool).await == want }).await;

        // A change is sent without a reconnect.
        reports.send_replace(report("http://relay.example:7480", &["s1", "s2"]));
        let want = expect(&["s1", "s2"]);
        wait_for("second report stored", || async { stored(&pool).await == want }).await;

        // An empty report, as from a relay whose mirroring stopped, clears it.
        reports.send_replace(RelayMirrors::default());
        wait_for("report cleared", || async { stored(&pool).await == (None, None) }).await;
    }
}
