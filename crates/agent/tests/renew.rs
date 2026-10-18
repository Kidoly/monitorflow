//! Certificate renewal against the real `CertificateService` and
//! `AgentService` over mTLS on 127.0.0.1. Tests that need PostgreSQL are
//! reported as ignored without `DATABASE_URL` at build time (see
//! `build.rs`).

mod support;

use std::time::{Duration, SystemTime};

use monitorflow_agent::backoff::Backoff;
use monitorflow_agent::identity::{self, AgentIdentity};
use monitorflow_agent::os::{HostInfo, OsFamily};
use monitorflow_agent::renew::{Renewal, RenewalSettings};
use monitorflow_agent::shutdown::{self, Shutdown, Trigger};
use monitorflow_agent::stream::{Agent, StreamSettings};
use monitorflow_common::config::AgentConfig;
use monitorflow_server::ca::{IssuedCert, Serial};
use monitorflow_server::grpc::{self, Registry};
use sqlx::PgPool;
use support::{agent_config, record, stored_serial, Pki, Upstream};
use tempfile::TempDir;
use tokio::task::JoinHandle;

const TIMEOUT: Duration = Duration::from_secs(10);
/// Short enough to be due for renewal at once (see `agent_identity_valid_for`).
const SHORT: Duration = Duration::from_secs(60);

fn host() -> HostInfo {
    HostInfo {
        hostname: "web-01.example".into(),
        os_family: OsFamily::Debian,
        os_version: "debian 12".into(),
    }
}

fn fast() -> Backoff {
    Backoff::new(Duration::from_millis(20), Duration::from_millis(200))
}

fn renewal_settings(config: &AgentConfig) -> RenewalSettings {
    RenewalSettings {
        check_every: Duration::from_millis(100),
        jitter: Duration::ZERO,
        backoff: fast(),
        ..RenewalSettings::from_config(config)
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

struct Setup {
    _data: TempDir,
    upstream: Upstream,
    config: AgentConfig,
    identity: AgentIdentity,
    issued: IssuedCert,
}

async fn setup(pki: &Pki, upstream: Upstream) -> Setup {
    let data = TempDir::new().unwrap();
    let config = agent_config(data.path().into(), &upstream.url, "https://localhost:1");
    let (identity, issued) = pki.agent_identity_valid_for(&config, "web-01", Some(SHORT));
    assert!(identity.renewal_due(SystemTime::now()));
    Setup {
        _data: data,
        upstream,
        config,
        identity,
        issued,
    }
}

/// The agent's stream and renewal task, wired as in `main.rs`. The stream
/// starts at once, the renewal task with [`Self::renew`].
struct Running {
    trigger: Trigger,
    stop: Shutdown,
    renewal: Option<Renewal>,
    tasks: Vec<JoinHandle<()>>,
}

impl Running {
    fn start(s: &Setup) -> Self {
        let (trigger, stop) = shutdown::channel();
        let (renewal, updates) = Renewal::new(
            renewal_settings(&s.config),
            s.config.tls_paths(),
            s.identity.clone(),
        );
        let settings = StreamSettings {
            heartbeat_interval: Duration::from_millis(100),
            backoff: fast(),
            ..StreamSettings::from_config(&s.config)
        };
        let agent = Agent::new(settings, s.identity.clone(), host()).follow(updates);
        let agent_stop = stop.clone();
        let stream = tokio::spawn(async move { agent.run(agent_stop).await.unwrap() });
        Self {
            trigger,
            stop,
            renewal: Some(renewal),
            tasks: vec![stream],
        }
    }

    fn renew(&mut self) {
        let renewal = self.renewal.take().unwrap();
        self.tasks
            .push(tokio::spawn(renewal.run(self.stop.clone())));
    }

    async fn stop(self) {
        self.trigger.fire();
        for task in self.tasks {
            tokio::time::timeout(TIMEOUT, task).await.unwrap().unwrap();
        }
    }
}

fn serial_on_server(registry: &Registry) -> Option<Serial> {
    registry.get("web-01").and_then(|peer| peer.cert_serial)
}

fn connection(registry: &Registry) -> Option<u64> {
    registry.get("web-01").map(|peer| peer.connection)
}

#[tokio::test]
async fn failed_renewal_keeps_the_identity_and_the_stream() {
    // No CertificateService: every attempt fails with UNIMPLEMENTED.
    let pki = Pki::new();
    let upstream = pki.serve_grpc(server_settings()).await;
    let s = setup(&pki, upstream).await;
    let paths = s.config.tls_paths();
    let (cert, key) = (
        std::fs::read(&paths.cert).unwrap(),
        std::fs::read(&paths.key).unwrap(),
    );
    let mut agent = Running::start(&s);
    let registry = &s.upstream.registry;
    eventually("registration", || connection(registry).is_some()).await;
    let first = connection(registry);
    agent.renew();

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(std::fs::read(&paths.cert).unwrap(), cert);
    assert_eq!(std::fs::read(&paths.key).unwrap(), key);
    assert_eq!(connection(registry), first, "the stream must not reconnect");
    assert_eq!(serial_on_server(registry), Some(s.issued.serial.clone()));
    agent.stop().await;
}

db_tests! {
    #[sqlx::test(migrations = "../server/migrations")]
    async fn renews_then_reconnects_with_the_new_certificate(pool: PgPool) {
        let pki = Pki::new();
        let upstream = pki.serve_grpc_with_renewer(server_settings(), &pool).await;
        let s = setup(&pki, upstream).await;
        record(&pool, "web-01", &s.issued).await;
        let old = s.issued.serial.clone();
        let paths = s.config.tls_paths();
        let key = std::fs::read(&paths.key).unwrap();

        // Connected with the old certificate first.
        let mut agent = Running::start(&s);
        let registry = &s.upstream.registry;
        eventually("registration", || serial_on_server(registry) == Some(old.clone())).await;
        let first = connection(registry).unwrap();

        agent.renew();
        eventually("reconnect with the renewed certificate", || {
            serial_on_server(registry).is_some_and(|serial| serial != old)
        })
        .await;
        let renewed = serial_on_server(registry).unwrap();
        assert_eq!(stored_serial(&s.config), renewed);
        assert!(connection(registry).unwrap() > first);
        assert_ne!(std::fs::read(&paths.key).unwrap(), key);
        let stored = identity::load(&paths).unwrap().unwrap();
        assert_eq!(stored.id, s.identity.id);
        assert!(!stored.renewal_due(SystemTime::now()), "renewed with the CA's default validity");

        // One renewal, then nothing more is due.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let serials: Vec<Vec<u8>> = sqlx::query_scalar("SELECT serial FROM certificates ORDER BY issued_at")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(serials, [old.as_bytes().to_vec(), renewed.as_bytes().to_vec()]);
        assert_eq!(serial_on_server(registry), Some(renewed));
        agent.stop().await;
    }
}
