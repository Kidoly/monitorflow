//! The forwarding loop between an agent, the relay and the server's stream
//! service, in-process over mTLS, without a database.

mod support;

use std::sync::Arc;

use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::{Command, Envelope, Heartbeat};
use monitorflow_common::tls::PeerRole;
use monitorflow_server::ca::RevocationReason;
use monitorflow_server::grpc::{AgentStreamService, AllowAllRelayedAgents, DenyRelayedAgents};
use support::{hello, open, server_settings, Pki, Relay};
use tonic::Code;

fn allow_all(registry: monitorflow_server::grpc::Registry) -> AgentStreamService {
    AgentStreamService::new(registry, Arc::new(AllowAllRelayedAgents), server_settings())
}

#[tokio::test]
async fn envelopes_go_both_ways_unchanged() {
    let pki = Pki::new();
    let upstream = pki.serve(allow_all).await;
    let relay = Relay::start(pki.relay("paris-1"), &upstream.url).await;

    let mut agent = open(&relay.url, &pki.agent("web-01"), hello("web-01"))
        .await
        .unwrap();
    let info = upstream.registry.get("web-01").unwrap();
    assert_eq!(info.role, PeerRole::Agent);
    assert_eq!(info.via_relay.as_deref(), Some("paris-1"));
    assert_eq!(info.hostname, "web-01.example");

    // Up: the agent's own sequence numbers reach the server.
    agent
        .send(7, Payload::Heartbeat(Heartbeat { sent_at_unix_ms: 1 }))
        .await;
    upstream
        .wait_until("heartbeat forwarded", |r| {
            r.get("web-01").unwrap().last_sequence == 7
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
    let status = open(&relay.url, &pki.relay("lyon-1"), hello("lyon-1"))
        .await
        .err()
        .expect("no relay chaining yet");
    assert_eq!(status.code(), Code::PermissionDenied, "{status}");
    assert!(status.message().contains("chaining"), "{status}");
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
async fn agents_are_refused_while_the_upstream_is_down() {
    let pki = Pki::new();
    let mut upstream = pki.serve(allow_all).await;
    let relay = Relay::start(pki.relay("paris-1"), &upstream.url).await;
    let mut agent = open(&relay.url, &pki.agent("web-01"), hello("web-01"))
        .await
        .unwrap();

    // A server shutdown reaches the agent as the server said it.
    upstream.stop();
    let status = agent.next().await.unwrap_err();
    assert_eq!(status.code(), Code::Unavailable, "{status}");
    assert_eq!(status.message(), "server shutting down");

    // While the upstream counts as down, new agents are refused without
    // dialing.
    relay.up.send_replace(false);
    let status = open(&relay.url, &pki.agent("web-02"), hello("web-02"))
        .await
        .err()
        .expect("the upstream is down");
    assert_eq!(status.code(), Code::Unavailable, "{status}");
    assert_eq!(status.message(), "upstream unavailable; retry later");
}
