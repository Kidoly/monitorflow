//! Chained relays (ADR-0028): agent → relay b → relay a → server, with the
//! real relay and server pieces over mTLS and the server's database.

mod support;

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use monitorflow_common::client::renew::{Renewal, RenewalSettings};
use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::revocation_service_client::RevocationServiceClient;
use monitorflow_common::proto::v1::{GetCrlRequest, Sample, SampleBatch};
use monitorflow_common::tls::{self, PeerIdentity, PeerRole};
use monitorflow_common::via::Via;
use monitorflow_server::grpc::{AgentStreamService, DbInventory, DbRelayPolicy};
use monitorflow_server::ingest::{Ingest, Series, Sink, WriteError, Writer, WriterSettings};
use monitorflow_server::renew::Renewer;
use monitorflow_server::sensor_config::{SensorConfigStore, SensorConfigs, WatcherSettings};
use sqlx::PgPool;
use support::{
    enroll_as, enroll_relay, hello, open, open_via, serve_enroll, server_settings, Pki, Relay,
    TIMEOUT,
};
use tonic::Code;

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

fn agent(id: &str) -> PeerIdentity {
    PeerIdentity::new(PeerRole::Agent, id).unwrap()
}

fn relay(id: &str) -> PeerIdentity {
    PeerIdentity::new(PeerRole::Relay, id).unwrap()
}

fn denied(result: Result<support::AgentStream, Box<tonic::Status>>, what: &str) -> String {
    let status = result
        .err()
        .unwrap_or_else(|| panic!("{what} was accepted"));
    assert_eq!(status.code(), Code::PermissionDenied, "{what}: {status}");
    status.message().to_owned()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

db_tests! {
    #[sqlx::test(migrations = "../server/migrations")]
    async fn agents_reach_the_server_through_two_chained_relays(pool: PgPool) {
        let pki = Pki::new();
        let enroll_url = serve_enroll(&pki, &pool).await;
        // a serves the dmz site and is the parent of b, which serves paris.
        // c serves lyon and connects to the server directly.
        let a_id = enroll_relay(&pki, &pool, &enroll_url, "a", "dmz", None).await;
        let b_id = enroll_relay(&pki, &pool, &enroll_url, "b", "paris", Some("a")).await;
        let c_id = enroll_relay(&pki, &pool, &enroll_url, "c", "lyon", None).await;
        let web01 = enroll_as(&pki, &pool, &enroll_url, PeerRole::Agent, "web-01", "paris", vec![]).await;
        let web02 = enroll_as(&pki, &pool, &enroll_url, PeerRole::Agent, "web-02", "lyon", vec![]).await;
        let web03 = enroll_as(&pki, &pool, &enroll_url, PeerRole::Agent, "web-03", "dmz", vec![]).await;
        let parent: Option<String> = sqlx::query_scalar(
            "SELECT p.peer_id FROM relays r JOIN relays p ON p.id = r.parent_relay_id WHERE r.peer_id = 'b'",
        )
        .fetch_optional(&pool).await.unwrap();
        assert_eq!(parent.as_deref(), Some("a"));

        let captured = Captured::default();
        let metrics = Writer::spawn(captured.clone(), WriterSettings {
            flush_interval: Duration::from_millis(20),
            ..WriterSettings::default()
        });
        let renewer = Renewer::new(pool.clone(), pki.ca.clone()).with_min_interval(Duration::ZERO);
        let upstream = pki.serve_with_renewer(|registry| {
            let configs = SensorConfigs::spawn(SensorConfigStore::new(pool.clone()), registry.clone(), WatcherSettings::default());
            AgentStreamService::new(registry, Arc::new(DbRelayPolicy::new(pool.clone())), server_settings())
                .with_inventory(Arc::new(DbInventory::new(pool.clone())))
                .with_sensor_configs(configs)
                .with_ingest(Ingest { metrics: Some(metrics.queue()), events: None })
        }, Some(renewer)).await;

        // b fetches its CRL from a, then opens its own stream through a.
        let a = Relay::start(a_id, &upstream.url).await;
        let b = Relay::start(b_id.clone(), &a.url).await;
        let own = upstream.registry.get("b").unwrap();
        assert_eq!((own.role, own.via_relay.as_deref()), (PeerRole::Relay, Some("a")));
        let seen: Option<bool> = sqlx::query_scalar("SELECT last_seen_at IS NOT NULL FROM relays WHERE peer_id = 'b'")
            .fetch_optional(&pool).await.unwrap();
        assert_eq!(seen, Some(true));

        // The CRL b serves downstream is the server's.
        let channel = tls::connect(&b.url, b_id.tls.client_config().unwrap(), TIMEOUT).await.unwrap();
        let served = RevocationServiceClient::new(channel).get_crl(GetCrlRequest {}).await.unwrap().into_inner();
        assert_eq!(served.crl_der, upstream.crl.subscribe().borrow().as_ref().to_vec());
        let channel = tls::connect(&b.url, web01.tls.client_config().unwrap(), TIMEOUT).await.unwrap();
        let status = RevocationServiceClient::new(channel).get_crl(GetCrlRequest {}).await.unwrap_err();
        assert_eq!(status.code(), Code::PermissionDenied);

        // Hello accepted through both relays; the configuration comes down.
        let mut stream = open(&b.url, &web01, hello("web-01")).await.unwrap();
        let first = stream.next().await.unwrap().unwrap();
        assert!(matches!(first.payload, Some(Payload::SensorConfig(_))), "{first:?}");
        let info = upstream.registry.get("web-01").unwrap();
        assert_eq!((info.role, info.via_relay.as_deref()), (PeerRole::Agent, Some("b")));
        let via: Option<String> = sqlx::query_scalar(
            "SELECT r.peer_id FROM servers s JOIN relays r ON r.id = s.relay_id WHERE s.peer_id = 'web-01'",
        ).fetch_optional(&pool).await.unwrap();
        assert_eq!(via.as_deref(), Some("b"));

        // Samples arrive with the agent's identity and site.
        let sample = Sample { sensor: "load1".into(), timestamp_unix_ms: now_ms(), value: 0.5, ..Sample::default() };
        stream.send(2, Payload::Samples(SampleBatch { samples: vec![sample] })).await;
        support::wait_until("sample written", || {
            captured.has(&[("__name__", "monitorflow_load1"), ("peer", "web-01"), ("site", "paris")])
        }).await;

        // Renewal through both relays, for the agent and for b itself.
        // b renews with its upstream, a; the agent with its own, b.
        for (identity, url) in [(&web01, &b.url), (&b_id, &a.url)] {
            let paths = support::paths(pki.dir.path(), identity.id.id());
            let (renewal, _rx) = Renewal::new(RenewalSettings::for_upstream(url), paths, identity.clone());
            let renewed = renewal.renew(identity).await.unwrap();
            assert_eq!(renewed.id, identity.id);
        }
        let vias: Vec<(String, String)> = sqlx::query_as(
            "SELECT target_id, details->>'via_relay' FROM audit_log WHERE action = 'peer.renew' ORDER BY id",
        ).fetch_all(&pool).await.unwrap();
        assert_eq!(vias, [("web-01".into(), "b".into()), ("b".into(), "a".into())]);

        // Agents of another site are refused at each level.
        for url in [&b.url, &a.url] {
            denied(open(url, &web02, hello("web-02")).await, "web-02");
        }
        // A dmz agent may use a, not b.
        denied(open(&b.url, &web03, hello("web-03")).await, "web-03 through b");
        let _web03 = open(&a.url, &web03, hello("web-03")).await.unwrap();
        assert_eq!(upstream.registry.get("web-03").unwrap().via_relay.as_deref(), Some("a"));

        // A relay that is not a's child cannot use a, for itself or others.
        denied(open(&a.url, &c_id, hello("c")).await, "c through a");
        assert!(upstream.registry.get("c").is_none());

        // A compromised b can claim anything below it, but the server only
        // accepts agents of its own site, through relays that are its
        // children.
        let lyon_agent = Via::origin(agent("web-02"));
        denied(open_via(&a.url, &b_id, hello("web-02"), Some(lyon_agent)).await, "forged lyon agent");
        let through_c = Via::extend(Some(Via::origin(agent("web-02"))), &relay("c")).unwrap();
        denied(open_via(&a.url, &b_id, hello("web-02"), Some(through_c)).await, "forged child relay");
        let as_a = Via::origin(relay("a"));
        denied(open_via(&a.url, &b_id, hello("a"), Some(as_a)).await, "b posing as its parent");
        assert!(upstream.registry.get("web-02").is_none());
    }
}
