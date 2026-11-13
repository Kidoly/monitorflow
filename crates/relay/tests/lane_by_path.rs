//! A relay keeps each stream's session and buffered records under its full
//! path, the sending peer included (ADR-0027, ADR-0028): another relay
//! claiming an agent's ID never stops that agent's stream, nor adds to,
//! overwrites or deletes its records.

mod support;

use std::sync::Arc;
use std::time::Duration;

use monitorflow_common::proto::v1::Envelope;
use monitorflow_common::tls::{PeerIdentity, PeerRole};
use monitorflow_common::via::Via;
use monitorflow_relay::buffer::{Buffer, BufferSettings, Limits};
use monitorflow_server::grpc::{AgentStreamService, Registry, RelayPolicy};
use support::{
    hello, open, open_via, samples, server_settings, BufferOptions, Captured, Pki, Relay,
};
use tonic::Code;

/// Refuses every path through a relay named `evil`.
struct NoEvil;

#[tonic::async_trait]
impl RelayPolicy for NoEvil {
    async fn may_speak_for(&self, relay_id: &str, _agent_id: &str) -> bool {
        relay_id != "evil"
    }

    async fn may_route(&self, relays: &[&str], _origin: &PeerIdentity) -> bool {
        !relays.contains(&"evil")
    }
}

fn no_evil(captured: &Captured) -> impl FnOnce(Registry) -> AgentStreamService {
    let ingest = captured.ingest();
    move |registry| {
        AgentStreamService::new(registry, Arc::new(NoEvil), server_settings()).with_ingest(ingest)
    }
}

fn web01() -> Via {
    Via::origin(PeerIdentity::new(PeerRole::Agent, "web-01").unwrap())
}

#[tokio::test]
async fn another_relay_cannot_take_or_feed_an_agents_lane() {
    let pki = Pki::new();
    let captured = Captured::default();
    let mut upstream = pki.serve(no_evil(&captured)).await;
    let relay = Relay::start(pki.relay("paris-1"), &upstream.url).await;
    let agent = open(&relay.url, &pki.agent("web-01"), hello("web-01"))
        .await
        .unwrap();
    agent.send(2, samples(1.0)).await;
    support::wait_until("live sample", || captured.values("web-01") == [1.0]).await;

    // evil claims web-01 below it: the server refuses the path, and the
    // real agent's stream goes on.
    let evil = pki.relay("evil");
    let status = open_via(&relay.url, &evil, hello("web-01"), Some(web01()))
        .await
        .err()
        .expect("the server refuses evil's path");
    assert_eq!(status.code(), Code::PermissionDenied, "{status}");
    agent.send(3, samples(2.0)).await;
    support::wait_until("still live", || captured.values("web-01") == [1.0, 2.0]).await;

    // The server goes away. evil's path was never accepted, so the relay
    // refuses it rather than buffering for it.
    let up = relay.up.clone();
    upstream.stop();
    support::wait_until("relay stream closed", || !*up.borrow()).await;
    let status = open_via(&relay.url, &evil, hello("web-01"), Some(web01()))
        .await
        .err()
        .expect("an unaccepted path is refused while the upstream is down");
    assert_eq!(status.code(), Code::Unavailable, "{status}");
    agent.send(4, samples(3.0)).await;
    support::wait_for("web-01 buffered", || async {
        relay.buffer.has_pending("web-01").await.unwrap()
    })
    .await;

    // Back: web-01 gets its own records, in order, and nothing of evil's.
    let _upstream = pki.serve_at(upstream.addr, no_evil(&captured), None).await;
    agent.send(5, samples(4.0)).await;
    support::wait_until("replayed", || {
        captured.values("web-01") == [1.0, 2.0, 3.0, 4.0]
    })
    .await;
    assert_eq!(relay.buffer.stats().await.unwrap().dropped_refused, 0);
}

#[tokio::test]
async fn relayed_records_filed_under_an_agent_id_are_dropped_on_upgrade() {
    let pki = Pki::new();
    let dir = tempfile::TempDir::new().unwrap();
    // What an earlier relay left: a path through b, filed under web-01.
    {
        let buffer = Buffer::open(&BufferSettings {
            dir: dir.path().to_owned(),
            limits: Limits::new(1 << 30, Duration::from_secs(3600)),
            fsync_interval: Duration::ZERO,
        })
        .await
        .unwrap();
        let b = PeerIdentity::new(PeerRole::Relay, "b").unwrap();
        let via = Via::extend(Some(web01()), &b).unwrap();
        buffer
            .set_hello("web-01", &hello("web-01"), &via)
            .await
            .unwrap();
        let batch = Envelope {
            sequence: 2,
            payload: Some(samples(666.0)),
        };
        assert!(buffer.append("web-01", &batch).await.unwrap());
    }
    let captured = Captured::default();
    let upstream = pki.serve(no_evil(&captured)).await;
    let relay = Relay::start_with(
        pki.relay("paris-1"),
        &upstream.url,
        BufferOptions::at(dir.path(), 1 << 30),
    )
    .await;
    support::wait_for("legacy records dropped", || async {
        !relay.buffer.has_pending("web-01").await.unwrap()
    })
    .await;
    assert!(captured.values("web-01").is_empty());
}
