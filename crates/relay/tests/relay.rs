//! Agent → relay → server with the server's database: enrollment of the
//! relay and its agents, the relay policy, the inventory, sensor
//! configuration and ingest.

mod support;

use std::net::SocketAddr;
use std::sync::Arc;

use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::{Sample, SampleBatch};
use monitorflow_common::tls::PeerRole;
use monitorflow_server::grpc::{AgentStreamService, DbInventory, DbRelayPolicy};
use monitorflow_server::sensor_config::{SensorConfigStore, SensorConfigs, WatcherSettings};
use sqlx::PgPool;
use support::{
    enroll_as, hello, now_ms, open, samples, serve_enroll, server_settings, Captured, Pki, Relay,
    Upstream,
};
use tonic::Code;

/// The server's stream service with its database pieces, on `addr`.
async fn serve_db(pki: &Pki, pool: &PgPool, captured: &Captured, addr: SocketAddr) -> Upstream {
    pki.serve_at(
        addr,
        |registry| {
            let configs = SensorConfigs::spawn(
                SensorConfigStore::new(pool.clone()),
                registry.clone(),
                WatcherSettings::default(),
            );
            AgentStreamService::new(
                registry,
                Arc::new(DbRelayPolicy::new(pool.clone())),
                server_settings(),
            )
            .with_inventory(Arc::new(DbInventory::new(pool.clone())))
            .with_sensor_configs(configs)
            .with_ingest(captured.ingest())
        },
        None,
    )
    .await
}

db_tests! {
    #[sqlx::test(migrations = "../server/migrations")]
    async fn agents_reach_the_server_through_their_sites_relay(pool: PgPool) {
        let pki = Pki::new();
        let enroll_url = serve_enroll(&pki, &pool).await;
        let relay_id = enroll_as(&pki, &pool, &enroll_url, PeerRole::Relay, "paris-1", "paris", vec!["localhost".into()]).await;
        let web01 = enroll_as(&pki, &pool, &enroll_url, PeerRole::Agent, "web-01", "paris", vec![]).await;
        let web02 = enroll_as(&pki, &pool, &enroll_url, PeerRole::Agent, "web-02", "lyon", vec![]).await;

        let captured = Captured::default();
        let upstream = serve_db(&pki, &pool, &captured, "127.0.0.1:0".parse().unwrap()).await;
        let relay = Relay::start(relay_id, &upstream.url).await;
        let seen: Option<bool> = sqlx::query_scalar("SELECT last_seen_at IS NOT NULL FROM relays WHERE peer_id = 'paris-1'")
            .fetch_optional(&pool).await.unwrap();
        assert_eq!(seen, Some(true));

        // Hello accepted through the relay; the sensor configuration comes
        // down first, through the relay.
        let mut agent = open(&relay.url, &web01, hello("web-01")).await.unwrap();
        let first = agent.next().await.unwrap().unwrap();
        let Some(Payload::SensorConfig(config)) = first.payload else { panic!("{first:?}") };
        let kinds: Vec<_> = config.sensors.iter().map(|s| s.kind.as_str()).collect();
        assert!(kinds.contains(&"cpu"), "{kinds:?}");
        let info = upstream.registry.get("web-01").unwrap();
        assert_eq!(info.via_relay.as_deref(), Some("paris-1"));
        let via: Option<String> = sqlx::query_scalar(
            "SELECT r.peer_id FROM servers s JOIN relays r ON r.id = s.relay_id WHERE s.peer_id = 'web-01'",
        ).fetch_optional(&pool).await.unwrap();
        assert_eq!(via.as_deref(), Some("paris-1"));

        // Samples arrive at ingest with the agent's identity, not the relay's.
        let sample = Sample {
            sensor: "load1".into(),
            timestamp_unix_ms: now_ms(),
            value: 0.5,
            ..Sample::default()
        };
        agent.send(2, Payload::Samples(SampleBatch { samples: vec![sample] })).await;
        support::wait_until("sample written", || {
            captured.has(&[("__name__", "monitorflow_load1"), ("peer", "web-01"), ("site", "paris")])
        }).await;

        // An agent of another site is refused by the server's policy.
        let status = open(&relay.url, &web02, hello("web-02")).await.err().expect("lyon agent");
        assert_eq!(status.code(), Code::PermissionDenied, "{status}");
        assert!(status.message().contains("may not speak for agent web-02"), "{status}");
        assert!(upstream.registry.get("web-02").is_none());

        // The agent goes away: the server sees it offline.
        drop(agent);
        upstream.wait_until("web-01 offline", |r| r.get("web-01").is_none()).await;
        assert!(upstream.registry.get("paris-1").is_some());
    }

    #[sqlx::test(migrations = "../server/migrations")]
    async fn samples_sent_during_an_outage_reach_ingest_in_order(pool: PgPool) {
        let pki = Pki::new();
        let enroll_url = serve_enroll(&pki, &pool).await;
        let relay_id = enroll_as(&pki, &pool, &enroll_url, PeerRole::Relay, "paris-1", "paris", vec!["localhost".into()]).await;
        let web01 = enroll_as(&pki, &pool, &enroll_url, PeerRole::Agent, "web-01", "paris", vec![]).await;
        let captured = Captured::default();
        let mut upstream = serve_db(&pki, &pool, &captured, "127.0.0.1:0".parse().unwrap()).await;
        let relay = Relay::start(relay_id, &upstream.url).await;
        let agent = open(&relay.url, &web01, hello("web-01")).await.unwrap();
        agent.send(2, samples(1.0)).await;
        support::wait_until("first sample", || captured.values("web-01") == [1.0]).await;

        // The server goes away; the agent keeps sending through the relay.
        upstream.stop();
        let up = relay.up.clone();
        support::wait_until("relay stream closed", || !*up.borrow()).await;
        for (sequence, value) in (3..).zip([2.0, 3.0, 4.0]) {
            agent.send(sequence, samples(value)).await;
        }
        support::wait_for("batches buffered", || async {
            relay.buffer.stats().await.unwrap().appended == 3
        }).await;

        let upstream = serve_db(&pki, &pool, &captured, upstream.addr).await;
        agent.send(6, samples(5.0)).await;
        support::wait_until("samples in order", || {
            captured.values("web-01") == [1.0, 2.0, 3.0, 4.0, 5.0]
        }).await;
        assert!(captured.has(&[("peer", "web-01"), ("site", "paris")]));
        let info = upstream.registry.get("web-01").unwrap();
        assert_eq!(info.via_relay.as_deref(), Some("paris-1"));
        support::wait_for("buffer emptied", || async {
            !relay.buffer.has_pending("web-01").await.unwrap()
        }).await;
    }
}
