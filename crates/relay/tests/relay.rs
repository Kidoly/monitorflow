//! Agent → relay → server with the server's database: enrollment of the
//! relay and its agents, the relay policy, the inventory, sensor
//! configuration and ingest.

mod support;

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use monitorflow_common::client::backoff::Backoff;
use monitorflow_common::client::enroll::{ensure_identity, EnrollSettings};
use monitorflow_common::client::identity::StoredIdentity;
use monitorflow_common::client::shutdown;
use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::{Sample, SampleBatch};
use monitorflow_common::tls::PeerRole;
use monitorflow_server::enroll::token::{self, Creator, TokenRequest};
use monitorflow_server::enroll::{self, Enroller, FailureLimiter};
use monitorflow_server::grpc::{AgentStreamService, DbInventory, DbRelayPolicy};
use monitorflow_server::ingest::{Ingest, Series, Sink, WriteError, Writer, WriterSettings};
use monitorflow_server::sensor_config::{SensorConfigStore, SensorConfigs, WatcherSettings};
use sqlx::PgPool;
use support::{hello, open, server_settings, Pki, Relay};
use tokio::net::TcpListener;
use tonic::Code;

/// Keeps every series written, in memory.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<Series>>>);

impl Sink for Captured {
    type Item = Series;

    fn name(&self) -> &'static str {
        "captured"
    }

    async fn write(&self, batch: &[Series]) -> Result<(), WriteError> {
        self.0.lock().unwrap().extend_from_slice(batch);
        Ok(())
    }
}

impl Captured {
    fn has(&self, labels: &[(&str, &str)]) -> bool {
        self.0.lock().unwrap().iter().any(|series| {
            labels
                .iter()
                .all(|(k, v)| series.labels.iter().any(|(a, b)| a == k && b == v))
        })
    }
}

/// Enrolls `id` with a token for `site`, through the enrollment listener.
async fn enroll_as(
    pki: &Pki,
    pool: &PgPool,
    enroll_url: &str,
    role: PeerRole,
    id: &str,
    site: &str,
    server_names: Vec<String>,
) -> StoredIdentity {
    let site_id = token::site_id(pool, site, true).await.unwrap().unwrap();
    let request = TokenRequest {
        role,
        peer_id: id.into(),
        site_id,
        ttl: token::DEFAULT_TTL,
        creator: Creator::Cli,
        server_names,
    };
    let created = token::create(pool, enroll::ca_fingerprint(&pki.ca), &request)
        .await
        .unwrap();
    let settings = EnrollSettings {
        role,
        paths: support::paths(pki.dir.path(), id),
        token: Some(created.token.to_secret_string()),
        enroll_url: enroll_url.into(),
    };
    let (_trigger, stop) = shutdown::channel();
    let fast = Backoff::new(Duration::from_millis(50), Duration::from_millis(200));
    ensure_identity(&settings, fast, stop).await.unwrap()
}

async fn serve_enroll(pki: &Pki, pool: &PgPool) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = support::url(&listener);
    let enroller = Enroller::new(pool.clone(), pki.ca.clone(), FailureLimiter::default());
    let tls = pki.server.server_config_without_client_auth().unwrap();
    tokio::spawn(enroll::serve(
        listener,
        tls,
        enroller,
        std::future::pending(),
    ));
    url
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
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
        let metrics = Writer::spawn(captured.clone(), WriterSettings {
            flush_interval: Duration::from_millis(20),
            ..WriterSettings::default()
        });
        let upstream = pki.serve(|registry| {
            let configs = SensorConfigs::spawn(SensorConfigStore::new(pool.clone()), registry.clone(), WatcherSettings::default());
            AgentStreamService::new(registry, Arc::new(DbRelayPolicy::new(pool.clone())), server_settings())
                .with_inventory(Arc::new(DbInventory::new(pool.clone())))
                .with_sensor_configs(configs)
                .with_ingest(Ingest { metrics: Some(metrics.queue()), events: None })
        }).await;
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
}
