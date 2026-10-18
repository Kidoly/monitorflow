//! The agent stream over real mTLS on 127.0.0.1.

#[path = "../../common/tests/support/mod.rs"]
mod support;

use std::sync::Arc;
use std::time::Duration;

use monitorflow_common::config::TlsPaths;
use monitorflow_common::proto::v1::agent_service_client::AgentServiceClient;
use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::{Command, Envelope, Heartbeat, Hello, Sample, SampleBatch};
use monitorflow_common::tls::{self, Identity, PeerRole};
use monitorflow_common::PROTOCOL_VERSION;
use monitorflow_server::grpc::{
    self, AgentStreamService, AllowAllRelayedAgents, DenyRelayedAgents, Registry, RelayPolicy,
    StreamSettings,
};
use support::{uri_san, Ca, Leaf};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Code, Status, Streaming};

const TIMEOUT: Duration = Duration::from_secs(5);

fn identity(ca: &Ca, leaf: &Leaf) -> Identity {
    let dir = TempDir::new().unwrap();
    let (ca_cert, cert, key) = support::write(dir.path(), "id", ca, leaf);
    Identity::load(&TlsPaths { ca_cert, cert, key }).unwrap()
}

struct Harness {
    ca: Ca,
    url: String,
    registry: Registry,
    shutdown: tokio::sync::oneshot::Sender<()>,
    server: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
}

fn settings() -> StreamSettings {
    StreamSettings {
        hello_timeout: Duration::from_millis(200),
        heartbeat_timeout: TIMEOUT,
        outbound_capacity: 8,
    }
}

impl Harness {
    async fn start(settings: StreamSettings, policy: Arc<dyn RelayPolicy>) -> Self {
        let ca = Ca::new("monitorflow test CA");
        let server = identity(&ca, &ca.server());
        let (_tx, tls_config) = watch::channel(server.server_config(Vec::new()).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "https://127.0.0.1:{}",
            listener.local_addr().unwrap().port()
        );
        let registry = Registry::default();
        let service = AgentStreamService::new(registry.clone(), policy, settings);
        let (shutdown, stop) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(grpc::serve(listener, tls_config, service, async {
            let _ = stop.await;
        }));
        Self {
            ca,
            url,
            registry,
            shutdown,
            server,
        }
    }

    async fn default() -> Self {
        Self::start(settings(), Arc::new(DenyRelayedAgents)).await
    }

    async fn open(&self, leaf: &Leaf, first: Option<Envelope>) -> Result<Peer, Status> {
        let config = identity(&self.ca, leaf).client_config().unwrap();
        let channel = tls::connect(&self.url, config, TIMEOUT).await.unwrap();
        let (tx, rx) = mpsc::channel(8);
        if let Some(first) = first {
            tx.send(first).await.unwrap();
        }
        let mut client = AgentServiceClient::new(channel);
        let call = client.stream(ReceiverStream::new(rx));
        let rx = tokio::time::timeout(TIMEOUT, call)
            .await
            .unwrap()?
            .into_inner();
        Ok(Peer { tx, rx })
    }

    async fn wait_until(&self, what: &str, done: impl Fn(&Registry) -> bool) {
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        while !done(&self.registry) {
            assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

struct Peer {
    tx: mpsc::Sender<Envelope>,
    rx: Streaming<Envelope>,
}

impl Peer {
    async fn send(&self, sequence: u64, payload: Payload) {
        let envelope = Envelope {
            sequence,
            payload: Some(payload),
        };
        self.tx.send(envelope).await.unwrap();
    }

    /// Next message from the server, or the status that ended the stream.
    async fn next(&mut self) -> Result<Option<Envelope>, Status> {
        tokio::time::timeout(TIMEOUT, self.rx.message())
            .await
            .unwrap()
    }
}

fn hello_v(agent_id: &str, version: &str) -> Envelope {
    Envelope {
        sequence: 1,
        payload: Some(Payload::Hello(Hello {
            agent_id: agent_id.into(),
            protocol_version: version.into(),
            agent_version: "0.1.0".into(),
            hostname: format!("{agent_id}.example"),
        })),
    }
}

fn hello(agent_id: &str) -> Option<Envelope> {
    Some(hello_v(agent_id, PROTOCOL_VERSION))
}

fn heartbeat() -> Payload {
    Payload::Heartbeat(Heartbeat { sent_at_unix_ms: 0 })
}

fn refused(result: Result<Peer, Status>) -> Status {
    match result {
        Ok(_) => panic!("stream unexpectedly accepted"),
        Err(status) => status,
    }
}

#[tokio::test]
async fn agent_registers_and_disappears_on_disconnect() {
    let h = Harness::default().await;
    let mut peer = h
        .open(&h.ca.client("agent-1", 10), hello("agent-1"))
        .await
        .unwrap();
    let info = h.registry.get("agent-1").unwrap();
    assert_eq!(info.role, PeerRole::Agent);
    assert_eq!(info.via_relay, None);
    assert_eq!(info.hostname, "agent-1.example");
    assert_eq!(info.agent_version, "0.1.0");
    assert_eq!(info.last_sequence, 1);
    assert_eq!(h.registry.connected().len(), 1);

    // Closing our side ends the stream cleanly.
    drop(peer.tx);
    assert_eq!(peer.rx.message().await.unwrap(), None);
    assert!(h.registry.get("agent-1").is_none());
}

#[tokio::test]
async fn agent_claiming_another_id_is_rejected() {
    let h = Harness::default().await;
    let status = refused(h.open(&h.ca.client("agent-1", 10), hello("agent-2")).await);
    assert_eq!(status.code(), Code::PermissionDenied);
    assert!(status.message().contains("agent-2"), "{status}");
    assert!(h.registry.connected().is_empty());
}

#[tokio::test]
async fn server_role_is_rejected() {
    let h = Harness::default().await;
    let leaf =
        h.ca.leaf("central", vec![uri_san("monitorflow://server/central")], 10);
    let status = refused(h.open(&leaf, hello("central")).await);
    assert_eq!(status.code(), Code::PermissionDenied);
}

#[tokio::test]
async fn missing_or_wrong_first_envelope_is_rejected() {
    let h = Harness::default().await;
    let leaf = h.ca.client("agent-1", 10);
    let status = refused(h.open(&leaf, None).await);
    assert_eq!(status.code(), Code::DeadlineExceeded);
    let not_hello = Envelope {
        sequence: 1,
        payload: Some(heartbeat()),
    };
    let status = refused(h.open(&leaf, Some(not_hello)).await);
    assert_eq!(status.code(), Code::InvalidArgument);
}

#[tokio::test]
async fn other_protocol_major_is_rejected() {
    let h = Harness::default().await;
    let leaf = h.ca.client("agent-1", 10);
    let status = refused(h.open(&leaf, Some(hello_v("agent-1", "2.0"))).await);
    assert_eq!(status.code(), Code::FailedPrecondition);
    assert!(status.message().contains("2.0"), "{status}");
    assert!(status.message().contains(PROTOCOL_VERSION), "{status}");
    // A newer minor of our major is fine.
    h.open(&leaf, Some(hello_v("agent-1", "1.99")))
        .await
        .unwrap();
}

#[tokio::test]
async fn duplicate_connection_replaces_the_old_one() {
    let h = Harness::default().await;
    let mut old = h
        .open(&h.ca.client("agent-1", 10), hello("agent-1"))
        .await
        .unwrap();
    let first = h.registry.get("agent-1").unwrap().connection;
    let mut new = h
        .open(&h.ca.client("agent-1", 11), hello("agent-1"))
        .await
        .unwrap();
    let second = h.registry.get("agent-1").unwrap().connection;
    assert_ne!(first, second);

    // The old stream is closed, and its teardown left the new entry alone.
    assert_eq!(old.next().await.unwrap_err().code(), Code::Aborted);
    assert_eq!(h.registry.get("agent-1").unwrap().connection, second);
    assert_eq!(h.registry.connected().len(), 1);

    // The new stream is the one that receives.
    let command = Envelope {
        sequence: 1,
        payload: Some(Payload::Command(Command::default())),
    };
    h.registry.send("agent-1", command.clone()).await.unwrap();
    assert_eq!(new.next().await.unwrap(), Some(command));
}

#[tokio::test]
async fn silent_peer_is_removed_after_heartbeat_timeout() {
    let settings = StreamSettings {
        heartbeat_timeout: Duration::from_millis(300),
        ..settings()
    };
    let h = Harness::start(settings, Arc::new(DenyRelayedAgents)).await;
    let mut peer = h
        .open(&h.ca.client("agent-1", 10), hello("agent-1"))
        .await
        .unwrap();
    // Heartbeats keep it alive past the timeout...
    for sequence in 2..6 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        peer.send(sequence, heartbeat()).await;
    }
    h.wait_until("last heartbeat", |r| {
        r.get("agent-1").unwrap().last_sequence == 5
    })
    .await;
    // ...and silence closes it.
    assert_eq!(
        peer.next().await.unwrap_err().code(),
        Code::DeadlineExceeded
    );
    assert!(h.registry.get("agent-1").is_none());
}

#[tokio::test]
async fn stale_sequence_numbers_are_ignored() {
    let h = Harness::default().await;
    let peer = h
        .open(&h.ca.client("agent-1", 10), hello("agent-1"))
        .await
        .unwrap();
    let batch = |n| {
        Payload::Samples(SampleBatch {
            samples: vec![Sample::default(); n],
        })
    };
    peer.send(2, batch(3)).await;
    peer.send(2, batch(5)).await; // duplicate
    peer.send(1, heartbeat()).await; // older than the last one
    peer.send(4, heartbeat()).await;
    h.wait_until("sequence 4", |r| {
        r.get("agent-1").unwrap().last_sequence == 4
    })
    .await;
    let counters = h.registry.get("agent-1").unwrap().counters;
    assert_eq!(counters.stale_sequences, 2);
    assert_eq!(counters.sample_batches, 1);
    assert_eq!(counters.samples, 3);
}

#[tokio::test]
async fn registry_send_reaches_the_client() {
    let h = Harness::default().await;
    let mut peer = h
        .open(&h.ca.client("agent-1", 10), hello("agent-1"))
        .await
        .unwrap();
    let command = Envelope {
        sequence: 42,
        payload: Some(Payload::Command(Command {
            command_id: "cmd-1".into(),
            ..Command::default()
        })),
    };
    h.registry.send("agent-1", command.clone()).await.unwrap();
    assert_eq!(peer.next().await.unwrap(), Some(command));
}

#[tokio::test]
async fn relay_speaks_for_itself_and_others_only_if_allowed() {
    let h = Harness::default().await;
    let _relay = h
        .open(&h.ca.relay("relay-1", 10), hello("relay-1"))
        .await
        .unwrap();
    assert_eq!(h.registry.get("relay-1").unwrap().role, PeerRole::Relay);
    let status = refused(h.open(&h.ca.relay("relay-1", 11), hello("agent-9")).await);
    assert_eq!(status.code(), Code::PermissionDenied);

    let h = Harness::start(settings(), Arc::new(AllowAllRelayedAgents)).await;
    let _relay = h
        .open(&h.ca.relay("relay-1", 10), hello("agent-9"))
        .await
        .unwrap();
    let info = h.registry.get("agent-9").unwrap();
    assert_eq!(info.role, PeerRole::Agent);
    assert_eq!(info.via_relay.as_deref(), Some("relay-1"));
}

#[tokio::test]
async fn shutdown_closes_streams_and_stops_serving() {
    let h = Harness::default().await;
    let mut peer = h
        .open(&h.ca.client("agent-1", 10), hello("agent-1"))
        .await
        .unwrap();
    h.shutdown.send(()).unwrap();
    assert_eq!(peer.next().await.unwrap_err().code(), Code::Unavailable);
    assert!(h.registry.connected().is_empty());
    let _keep = peer;
    let served = tokio::time::timeout(TIMEOUT, h.server).await.unwrap();
    served.unwrap().unwrap();
}
