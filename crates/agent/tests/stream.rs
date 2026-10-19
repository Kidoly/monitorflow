//! The agent's stream against the real `AgentService` over mTLS on
//! 127.0.0.1: Hello and heartbeats, reconnects, revocation, shutdown.

mod support;

use std::time::Duration;

use monitorflow_agent::backoff::Backoff;
use monitorflow_agent::enroll::ensure_identity;
use monitorflow_agent::identity::AgentIdentity;
use monitorflow_agent::os::{HostInfo, OsFamily};
use monitorflow_agent::shutdown::{self, Trigger};
use monitorflow_agent::stream::{Agent, Failure, StreamSettings};
use monitorflow_common::config::AgentConfig;
use monitorflow_common::tls::PeerRole;
use monitorflow_server::ca::RevocationReason;
use monitorflow_server::grpc::{self, Registry};
use sqlx::PgPool;
use support::{agent_config, Pki, Upstream};
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const TIMEOUT: Duration = Duration::from_secs(10);

fn host() -> HostInfo {
    HostInfo {
        hostname: "web-01.example".into(),
        os_family: OsFamily::Debian,
        os_version: "debian 12".into(),
    }
}

fn settings(config: &AgentConfig) -> StreamSettings {
    StreamSettings {
        heartbeat_interval: Duration::from_millis(100),
        backoff: Backoff::new(Duration::from_millis(20), Duration::from_millis(200)),
        ..StreamSettings::from_config(config)
    }
}

fn server_settings() -> grpc::StreamSettings {
    grpc::StreamSettings {
        heartbeat_timeout: TIMEOUT,
        ..grpc::StreamSettings::default()
    }
}

async fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while !check() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn connection(registry: &Registry, id: &str) -> Option<u64> {
    registry.get(id).map(|peer| peer.connection)
}

struct Running {
    trigger: Trigger,
    task: JoinHandle<()>,
}

impl Running {
    fn start(settings: StreamSettings, identity: AgentIdentity) -> Self {
        let (trigger, stop) = shutdown::channel();
        let agent = Agent::new(settings, identity, host());
        let task = tokio::spawn(async move { agent.run(stop).await.unwrap() });
        Self { trigger, task }
    }

    async fn stop(self) {
        self.trigger.fire();
        tokio::time::timeout(TIMEOUT, self.task)
            .await
            .unwrap()
            .unwrap();
    }
}

struct Setup {
    _data: TempDir,
    pki: Pki,
    upstream: Upstream,
    config: AgentConfig,
}

async fn setup(server: grpc::StreamSettings) -> Setup {
    let pki = Pki::new();
    let upstream = pki.serve_grpc(server).await;
    let data = TempDir::new().unwrap();
    let config = agent_config(data.path().into(), &upstream.url, "https://localhost:1");
    Setup {
        _data: data,
        pki,
        upstream,
        config,
    }
}

#[tokio::test]
async fn hello_then_heartbeats_then_clean_shutdown() {
    let s = setup(server_settings()).await;
    let (identity, _) = s.pki.agent_identity(&s.config, "web-01");
    let agent = Running::start(settings(&s.config), identity);

    let registry = &s.upstream.registry;
    eventually("registration", || registry.get("web-01").is_some()).await;
    let peer = registry.get("web-01").unwrap();
    assert_eq!(
        (peer.role, peer.hostname.as_str()),
        (PeerRole::Agent, "web-01.example")
    );
    assert_eq!(peer.agent_version, env!("CARGO_PKG_VERSION"));
    let first = peer.last_sequence;
    eventually("heartbeats", || {
        registry.get("web-01").unwrap().last_sequence >= first + 3
    })
    .await;
    assert_eq!(registry.get("web-01").unwrap().counters.stale_sequences, 0);

    agent.stop().await;
    eventually("removal after shutdown", || {
        registry.get("web-01").is_none()
    })
    .await;
}

#[tokio::test]
async fn reconnects_after_being_replaced() {
    let s = setup(server_settings()).await;
    let (identity, _) = s.pki.agent_identity(&s.config, "web-01");
    let agent = Running::start(settings(&s.config), identity);
    let registry = &s.upstream.registry;
    eventually("registration", || registry.get("web-01").is_some()).await;

    // An intruder with the same ID makes the server abort the agent's stream.
    let mut intruder = registry.get("web-01").unwrap();
    intruder.hostname = "intruder".into();
    let (tx, _rx) = mpsc::channel(1);
    let taken = registry.register(intruder, tx).unwrap().connection;
    eventually("reconnect", || connection(registry, "web-01") > Some(taken)).await;
    let peer = registry.get("web-01").unwrap();
    assert_eq!(peer.hostname, "web-01.example");
    // Sequences keep growing across connections.
    assert!(peer.last_sequence > 1, "{peer:?}");
    agent.stop().await;
}

#[tokio::test]
async fn reconnects_after_a_heartbeat_timeout() {
    let s = setup(grpc::StreamSettings {
        heartbeat_timeout: Duration::from_millis(300),
        ..server_settings()
    })
    .await;
    let (identity, _) = s.pki.agent_identity(&s.config, "web-01");
    let slow = StreamSettings {
        heartbeat_interval: Duration::from_secs(60),
        ..settings(&s.config)
    };
    let agent = Running::start(slow, identity);
    let registry = &s.upstream.registry;
    eventually("registration", || registry.get("web-01").is_some()).await;
    let first = connection(registry, "web-01").unwrap();
    eventually("reconnect", || connection(registry, "web-01") > Some(first)).await;
    agent.stop().await;
}

/// One session, stopped after `TIMEOUT` if still open.
async fn session(agent: &mut Agent) -> Result<(), Failure> {
    let (trigger, mut stop) = shutdown::channel();
    let guard = tokio::spawn(async move {
        tokio::time::sleep(TIMEOUT).await;
        trigger.fire();
    });
    let result = agent.session(&mut stop, &mut None).await;
    guard.abort();
    result
}

#[tokio::test]
async fn revoked_certificate_is_reported_as_such() {
    let s = setup(server_settings()).await;
    let (identity, serial) = s.pki.agent_identity(&s.config, "web-01");
    s.upstream
        .crl
        .revoke(serial, RevocationReason::KeyCompromise)
        .await
        .unwrap();
    let mut agent = Agent::new(settings(&s.config), identity, host());
    assert_eq!(session(&mut agent).await, Err(Failure::Revoked));
    assert!(s.upstream.registry.get("web-01").is_none());
}

#[tokio::test]
async fn certificate_from_another_ca_is_rejected() {
    let s = setup(server_settings()).await;
    // Our certificate comes from another CA, but we trust the server's: the
    // server is the side that refuses.
    Pki::new().agent_identity(&s.config, "web-01");
    let paths = s.config.tls_paths();
    std::fs::write(&paths.ca_cert, s.pki.ca.ca_cert_pem()).unwrap();
    let identity = monitorflow_agent::identity::load(&paths).unwrap().unwrap();
    let mut agent = Agent::new(settings(&s.config), identity, host());
    let failure = session(&mut agent).await.unwrap_err();
    assert!(matches!(failure, Failure::Rejected(_)), "{failure:?}");
}

#[tokio::test]
async fn shutdown_interrupts_the_backoff() {
    let s = setup(server_settings()).await;
    let (identity, _) = s.pki.agent_identity(&s.config, "web-01");
    let mut config = s.config.clone();
    config.upstream_url = support::closed_port_url().await;
    let long = StreamSettings {
        backoff: Backoff::new(Duration::from_secs(3600), Duration::from_secs(3600)),
        ..settings(&config)
    };
    // The first delay is random in [0, 1 h]: shutdown must cut it short.
    let agent = Running::start(long, identity);
    tokio::time::sleep(Duration::from_millis(200)).await;
    agent.stop().await;
}

db_tests! {
    #[sqlx::test(migrations = "../server/migrations")]
    async fn enrolls_then_appears_in_the_registry(pool: PgPool) {
        let pki = Pki::new();
        let enroll_url = pki.serve_enroll(&pool).await;
        let upstream = pki.serve_grpc(server_settings()).await;
        let data = TempDir::new().unwrap();
        let mut config = agent_config(data.path().into(), &upstream.url, &enroll_url);
        config.enrollment_token = Some(pki.token(&pool, "web-01").await);

        let (_trigger, stop) = shutdown::channel();
        let identity = ensure_identity(&config, settings(&config).backoff, stop).await.unwrap();
        assert_eq!(identity.peer_id(), "web-01");
        let agent = Running::start(settings(&config), identity);
        let registry = &upstream.registry;
        eventually("registration", || registry.get("web-01").is_some()).await;
        assert_eq!(registry.get("web-01").unwrap().role, PeerRole::Agent);
        agent.stop().await;
    }
}
