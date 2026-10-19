//! The forwarding loop between an agent, the relay and the server's stream
//! service, in-process over mTLS, without a database, and the relay's
//! store-and-forward buffer (ADR-0027).

mod support;

use std::sync::Arc;

use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::{Command, Envelope, Heartbeat};
use monitorflow_common::tls::{PeerIdentity, PeerRole};
use monitorflow_server::ca::RevocationReason;
use monitorflow_server::grpc::{
    AgentStreamService, AllowAllRelayedAgents, DenyRelayedAgents, Registry, RelayPolicy,
};
use support::{hello, open, samples, server_settings, BufferOptions, Captured, Pki, Relay};
use tonic::Code;

fn allow_all(registry: Registry) -> AgentStreamService {
    AgentStreamService::new(registry, Arc::new(AllowAllRelayedAgents), server_settings())
}

/// Every relayed agent but `web-02`, writing samples to `captured`.
fn all_but_web02(captured: &Captured) -> impl FnOnce(Registry) -> AgentStreamService {
    struct NotWeb02;
    #[tonic::async_trait]
    impl RelayPolicy for NotWeb02 {
        async fn may_speak_for(&self, _relay_id: &str, agent_id: &str) -> bool {
            agent_id != "web-02"
        }
    }
    let ingest = captured.ingest();
    move |registry| {
        AgentStreamService::new(registry, Arc::new(NotWeb02), server_settings()).with_ingest(ingest)
    }
}

fn capturing(captured: &Captured) -> impl FnOnce(Registry) -> AgentStreamService {
    let ingest = captured.ingest();
    move |registry| allow_all(registry).with_ingest(ingest)
}

/// Stops the upstream and waits for the relay to notice.
async fn stop(upstream: &mut support::Upstream, relay: &Relay) {
    let up = relay.up.clone();
    support::wait_until("relay stream open", || *up.borrow()).await;
    upstream.stop();
    support::wait_until("relay stream closed", || !*up.borrow()).await;
}

async fn appended(relay: &Relay, count: u64) {
    let deadline = tokio::time::Instant::now() + support::TIMEOUT;
    loop {
        let stats = relay.buffer.stats().await.unwrap();
        if stats.appended == count {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out: {count} batches buffered: {stats:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn envelopes_go_both_ways_unchanged() {
    let pki = Pki::new();
    let upstream = pki.serve(allow_all).await;
    let relay = Relay::start(pki.relay("paris-1"), &upstream.url).await;
    let own = upstream.registry.get("paris-1").unwrap();
    assert_eq!((own.role, own.via_relay), (PeerRole::Relay, None));

    let mut agent = open(&relay.url, &pki.agent("web-01"), hello("web-01"))
        .await
        .unwrap();
    let info = upstream.registry.get("web-01").unwrap();
    assert_eq!(info.role, PeerRole::Agent);
    assert_eq!(info.via_relay.as_deref(), Some("paris-1"));
    assert_eq!(info.hostname, "web-01.example");

    // Up: the relay numbers its own upstream stream, Hello first, and
    // drops what the agent sends out of sequence.
    agent
        .send(7, Payload::Heartbeat(Heartbeat { sent_at_unix_ms: 1 }))
        .await;
    agent
        .send(7, Payload::Heartbeat(Heartbeat { sent_at_unix_ms: 2 }))
        .await;
    agent
        .send(9, Payload::Heartbeat(Heartbeat { sent_at_unix_ms: 3 }))
        .await;
    upstream
        .wait_until("heartbeats forwarded", |r| {
            r.get("web-01").unwrap().last_sequence == 3
        })
        .await;

    // Down: a command queued for the agent arrives as the server sent it.
    let command = upstream.registry.envelope(Payload::Command(Command {
        command_id: "cmd-1".into(),
        kind: "shell".into(),
        payload: b"uptime".to_vec(),
        signature: vec![1, 2, 3],
        ..Command::default()
    }));
    upstream
        .registry
        .send("web-01", command.clone())
        .await
        .unwrap();
    assert_eq!(agent.next().await.unwrap(), Some(command));

    // The agent leaves: the server sees it offline, the relay stays.
    drop(agent);
    upstream
        .wait_until("agent removed", |r| r.get("web-01").is_none())
        .await;
    assert!(upstream.registry.get("paris-1").is_some());
}

#[tokio::test]
async fn upstream_refusals_reach_the_agent_unchanged() {
    let pki = Pki::new();
    let upstream = pki
        .serve(|registry| {
            AgentStreamService::new(registry, Arc::new(DenyRelayedAgents), server_settings())
        })
        .await;
    let relay = Relay::start(pki.relay("paris-1"), &upstream.url).await;
    let status = open(&relay.url, &pki.agent("web-01"), hello("web-01"))
        .await
        .err()
        .expect("the upstream policy refuses web-01");
    assert_eq!(status.code(), Code::PermissionDenied, "{status}");
    assert!(
        status.message().contains("may not speak for agent web-01"),
        "{status}"
    );
    assert!(upstream.registry.get("web-01").is_none());
}

#[tokio::test]
async fn the_relay_checks_roles_and_the_hello() {
    let pki = Pki::new();
    let upstream = pki.serve(allow_all).await;
    let relay = Relay::start(pki.relay("paris-1"), &upstream.url).await;

    let status = open(&relay.url, &pki.agent("web-01"), hello("web-02"))
        .await
        .err()
        .expect("web-01 may not say it is web-02");
    assert_eq!(status.code(), Code::PermissionDenied, "{status}");
    // A downstream relay without a path speaks only for itself.
    let status = open(&relay.url, &pki.relay("lyon-1"), hello("web-01"))
        .await
        .err()
        .expect("lyon-1 may not say it is web-01");
    assert_eq!(status.code(), Code::PermissionDenied, "{status}");
    let status = open(
        &relay.url,
        &pki.agent("web-01"),
        Envelope {
            sequence: 1,
            payload: Some(Payload::Heartbeat(Heartbeat::default())),
        },
    )
    .await
    .err()
    .expect("the first envelope must be Hello");
    assert_eq!(status.code(), Code::InvalidArgument, "{status}");
    assert!(upstream.registry.get("web-01").is_none());
    assert!(upstream.registry.get("web-02").is_none());
}

#[tokio::test]
async fn revoked_agents_are_refused_with_the_upstream_crl() {
    let pki = Pki::new();
    let upstream = pki.serve(allow_all).await;
    let (revoked, serial) = pki.issue(PeerRole::Agent, "web-09", &[]);
    upstream
        .crl
        .revoke(serial, RevocationReason::KeyCompromise)
        .await
        .unwrap();
    // The relay fetches the CRL at start.
    let relay = Relay::start(pki.relay("paris-1"), &upstream.url).await;
    assert!(open(&relay.url, &revoked, hello("web-09")).await.is_err());
    assert!(upstream.registry.get("web-09").is_none());
    open(&relay.url, &pki.agent("web-01"), hello("web-01"))
        .await
        .unwrap();
}

#[tokio::test]
async fn batches_wait_on_disk_while_the_upstream_is_down() {
    let pki = Pki::new();
    let captured = Captured::default();
    let mut upstream = pki.serve(capturing(&captured)).await;
    let relay = Relay::start(pki.relay("paris-1"), &upstream.url).await;
    let web01 = open(&relay.url, &pki.agent("web-01"), hello("web-01"))
        .await
        .unwrap();
    web01.send(2, samples(1.0)).await;
    support::wait_until("live sample", || captured.values("web-01") == [1.0]).await;

    // The server stops: web-01 stays connected, and web-02 is accepted
    // although the relay cannot ask the server about it.
    stop(&mut upstream, &relay).await;
    let web02 = open(&relay.url, &pki.agent("web-02"), hello("web-02"))
        .await
        .unwrap();
    for (sequence, value) in (3..).zip([2.0, 3.0, 4.0]) {
        web01.send(sequence, samples(value)).await;
        web02.send(2 * sequence, samples(value + 10.0)).await;
        // Not buffered, not forwarded later.
        web02
            .send(2 * sequence + 1, Payload::Heartbeat(Heartbeat::default()))
            .await;
    }
    appended(&relay, 6).await;
    assert!(captured.values("web-02").is_empty());

    // Back at the same address: each agent's backlog goes first, then
    // what it sends next.
    let upstream = pki
        .serve_at(upstream.addr, capturing(&captured), None)
        .await;
    web01.send(6, samples(5.0)).await;
    support::wait_until("web-01 in order", || {
        captured.values("web-01") == [1.0, 2.0, 3.0, 4.0, 5.0]
    })
    .await;
    support::wait_until("web-02 in order", || {
        captured.values("web-02") == [12.0, 13.0, 14.0]
    })
    .await;
    web02.send(200, samples(15.0)).await;
    support::wait_until("web-02 live", || {
        captured.values("web-02") == [12.0, 13.0, 14.0, 15.0]
    })
    .await;
    assert!(upstream.registry.get("web-02").is_some());
    support::wait_for("buffer emptied", || async {
        relay.buffer.stats().await.unwrap().bytes == 0
    })
    .await;
}

#[tokio::test]
async fn the_buffer_survives_a_relay_restart() {
    let pki = Pki::new();
    let dir = tempfile::TempDir::new().unwrap();
    let captured = Captured::default();
    let mut upstream = pki.serve(capturing(&captured)).await;
    let relay = Relay::start_with(
        pki.relay("paris-1"),
        &upstream.url,
        BufferOptions::at(dir.path(), 1 << 30),
    )
    .await;
    stop(&mut upstream, &relay).await;
    let agent = open(&relay.url, &pki.agent("web-01"), hello("web-01"))
        .await
        .unwrap();
    for (sequence, value) in (2..).zip([1.0, 2.0, 3.0]) {
        agent.send(sequence, samples(value)).await;
    }
    appended(&relay, 3).await;
    drop(agent);
    relay.stop().await;

    // A new relay on the same directory replays them although the agent
    // does not come back, then closes the agent's stream.
    let upstream = pki
        .serve_at(upstream.addr, capturing(&captured), None)
        .await;
    let relay = Relay::start_with(
        pki.relay("paris-1"),
        &upstream.url,
        BufferOptions::at(dir.path(), 1 << 30),
    )
    .await;
    support::wait_until("replayed in order", || {
        captured.values("web-01") == [1.0, 2.0, 3.0]
    })
    .await;
    upstream
        .wait_until("replay stream closed", |r| r.get("web-01").is_none())
        .await;
    assert!(!relay.buffer.has_pending("web-01").await.unwrap());
}

#[tokio::test]
async fn a_full_buffer_drops_the_oldest_batches() {
    let pki = Pki::new();
    let dir = tempfile::TempDir::new().unwrap();
    let captured = Captured::default();
    let mut upstream = pki.serve(capturing(&captured)).await;
    // Room for about ten batches.
    let relay = Relay::start_with(
        pki.relay("paris-1"),
        &upstream.url,
        BufferOptions::at(dir.path(), 600),
    )
    .await;
    stop(&mut upstream, &relay).await;
    let agent = open(&relay.url, &pki.agent("web-01"), hello("web-01"))
        .await
        .unwrap();
    for value in 1..=40 {
        agent
            .send(value + 1, samples(f64::from(value as u32)))
            .await;
    }
    appended(&relay, 40).await;
    let stats = relay.buffer.stats().await.unwrap();
    assert!(stats.dropped_full > 0 && stats.bytes <= 600, "{stats:?}");

    let _upstream = pki
        .serve_at(upstream.addr, capturing(&captured), None)
        .await;
    support::wait_until("newest batch", || {
        captured.values("web-01").last() == Some(&40.0)
    })
    .await;
    // What is left is the newest batches, in order.
    let values = captured.values("web-01");
    assert!(values.len() < 40, "{values:?}");
    let expected: Vec<f64> = (41 - values.len() as u32..=40).map(f64::from).collect();
    assert_eq!(values, expected);
}

#[tokio::test]
async fn records_of_agents_the_upstream_refuses_are_dropped() {
    let pki = Pki::new();
    let captured = Captured::default();
    let mut upstream = pki.serve(all_but_web02(&captured)).await;
    let relay = Relay::start(pki.relay("paris-1"), &upstream.url).await;
    stop(&mut upstream, &relay).await;
    let mut web02 = open(&relay.url, &pki.agent("web-02"), hello("web-02"))
        .await
        .unwrap();
    web02.send(2, samples(1.0)).await;
    appended(&relay, 1).await;

    let _upstream = pki
        .serve_at(upstream.addr, all_but_web02(&captured), None)
        .await;
    let status = web02.next().await.unwrap_err();
    assert_eq!(status.code(), Code::PermissionDenied, "{status}");
    assert!(
        status.message().contains("may not speak for agent web-02"),
        "{status}"
    );
    assert!(!relay.buffer.has_pending("web-02").await.unwrap());
    assert_eq!(relay.buffer.stats().await.unwrap().dropped_refused, 1);
}

/// Routes the server's policy was asked about: relays from the connecting
/// one down, then the origin.
#[derive(Clone, Default)]
struct Routes(Arc<std::sync::Mutex<Vec<Route>>>);

type Route = (Vec<String>, String);

#[tonic::async_trait]
impl RelayPolicy for Routes {
    async fn may_speak_for(&self, _relay_id: &str, _agent_id: &str) -> bool {
        true
    }

    async fn may_route(&self, relays: &[&str], origin: &PeerIdentity) -> bool {
        let relays = relays.iter().map(|r| r.to_string()).collect();
        self.0
            .lock()
            .unwrap()
            .push((relays, origin.id().to_owned()));
        true
    }
}

fn routing(captured: &Captured, routes: &Routes) -> impl FnOnce(Registry) -> AgentStreamService {
    let (ingest, routes) = (captured.ingest(), routes.clone());
    move |registry| {
        AgentStreamService::new(registry, Arc::new(routes), server_settings()).with_ingest(ingest)
    }
}

#[tokio::test]
async fn batches_from_behind_a_downstream_relay_replay_with_their_path() {
    let pki = Pki::new();
    let dir = tempfile::TempDir::new().unwrap();
    let (captured, routes) = (Captured::default(), Routes::default());
    let mut upstream = pki.serve(routing(&captured, &routes)).await;
    let a_options = || BufferOptions::at(dir.path(), 1 << 30);
    let a = Relay::start_with(pki.relay("a"), &upstream.url, a_options()).await;
    let up = a.up.clone();
    support::wait_until("a's stream open", || *up.borrow()).await;
    let b = Relay::start(pki.relay("b"), &a.url).await;
    let agent = open(&b.url, &pki.agent("web-01"), hello("web-01"))
        .await
        .unwrap();
    upstream
        .wait_until("web-01 behind b", |r| {
            r.get("web-01")
                .is_some_and(|info| info.via_relay.as_deref() == Some("b"))
        })
        .await;

    // The server goes away: b still reaches a, which buffers.
    stop(&mut upstream, &a).await;
    for (sequence, value) in (2..).zip([1.0, 2.0, 3.0]) {
        agent.send(sequence, samples(value)).await;
    }
    appended(&a, 3).await;
    assert!(*b.up.borrow());

    // a restarts with neither b nor the agent: the replay comes from disk
    // and must still say the batches came through b.
    drop(agent);
    drop(b);
    a.stop().await;
    routes.0.lock().unwrap().clear();
    let upstream = pki
        .serve_at(upstream.addr, routing(&captured, &routes), None)
        .await;
    let _a = Relay::start_with(pki.relay("a"), &upstream.url, a_options()).await;
    support::wait_until("replayed in order", || {
        captured.values("web-01") == [1.0, 2.0, 3.0]
    })
    .await;
    let routes = routes.0.lock().unwrap().clone();
    assert!(
        routes.contains(&(vec!["a".to_owned(), "b".to_owned()], "web-01".to_owned())),
        "{routes:?}"
    );
}
