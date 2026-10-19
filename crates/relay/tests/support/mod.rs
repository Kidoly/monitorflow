//! An upstream made of the real server pieces and a relay, in-process on
//! 127.0.0.1 over mTLS from one CA.
#![allow(dead_code)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use monitorflow_common::client::backoff::Backoff;
use monitorflow_common::client::identity::{self, Enrolled, StoredIdentity};
use monitorflow_common::client::shutdown::{self, Trigger};
use monitorflow_common::config::{RelayConfig, TlsPaths};
use monitorflow_common::proto::v1::agent_service_client::AgentServiceClient;
use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::{Envelope, Hello};
use monitorflow_common::redact::Secret;
use monitorflow_common::tls::{self, Identity, PeerIdentity, PeerRole};
use monitorflow_common::PROTOCOL_VERSION;
use monitorflow_relay::control::{Control, ControlSettings};
use monitorflow_relay::forward::{ForwardSettings, Forwarder};
use monitorflow_relay::listener::{self, CrlSettings, TlsPublisher};
use monitorflow_server::ca::{CaOptions, CertificateAuthority, Serial, ServerName};
use monitorflow_server::crl::CrlPublisher;
use monitorflow_server::enroll::server_cert;
use monitorflow_server::grpc::{self, AgentStreamService, CrlService, Registry};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Status, Streaming};

pub const TIMEOUT: Duration = Duration::from_secs(10);

/// Skips a test, with a reason, when the build had no `DATABASE_URL`.
#[macro_export]
macro_rules! db_tests {
    ($($item:item)*) => {
        $(
            #[cfg_attr(
                not(db_tests),
                ignore = "DATABASE_URL is not set; run `docker compose -f deploy/docker-compose.dev.yml up -d postgres` and export DATABASE_URL from .env.example"
            )]
            $item
        )*
    };
}

/// The server's CA and its own certificate for `localhost`.
pub struct Pki {
    pub dir: TempDir,
    pub ca: Arc<CertificateAuthority>,
    pub server: Identity,
}

impl Pki {
    pub fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let ca = CertificateAuthority::open_or_create(&dir.path().join("ca"), CaOptions::default())
            .unwrap();
        let paths = paths(dir.path(), "server");
        server_cert::ensure(&ca, &paths, &[ServerName::Dns("localhost".into())]).unwrap();
        let server = Identity::load(&paths).unwrap();
        Self {
            dir,
            ca: Arc::new(ca),
            server,
        }
    }

    /// A certificate from the CA, stored and loaded as a peer would.
    pub fn issue(
        &self,
        role: PeerRole,
        id: &str,
        names: &[ServerName],
    ) -> (StoredIdentity, Serial) {
        let key = rcgen::KeyPair::generate().unwrap();
        let csr = rcgen::CertificateParams::default()
            .serialize_request(&key)
            .unwrap()
            .pem()
            .unwrap();
        let peer = PeerIdentity::new(role, id).unwrap();
        let issued = self.ca.issue(&csr, &peer, None, names).unwrap();
        let enrolled = Enrolled {
            key_pem: Secret::new(key.serialize_pem()),
            cert_pem: issued.cert_pem,
            ca_cert_pem: self.ca.ca_cert_pem().to_owned(),
        };
        let paths = paths(self.dir.path(), id);
        identity::store(&paths, &enrolled).unwrap();
        (
            identity::load(&paths, role).unwrap().unwrap(),
            issued.serial,
        )
    }

    pub fn agent(&self, id: &str) -> StoredIdentity {
        self.issue(PeerRole::Agent, id, &[]).0
    }

    /// A relay certificate valid for `localhost`.
    pub fn relay(&self, id: &str) -> StoredIdentity {
        self.issue(PeerRole::Relay, id, &[ServerName::Dns("localhost".into())])
            .0
    }

    /// Serves the service `make` builds, with `RevocationService`.
    pub async fn serve(&self, make: impl FnOnce(Registry) -> AgentStreamService) -> Upstream {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = url(&listener);
        let (crl, rx) = CrlPublisher::start(self.ca.clone(), self.server.clone())
            .await
            .unwrap();
        let registry = Registry::default();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(grpc::serve_all(
            listener,
            rx,
            make(registry.clone()),
            None,
            Some(CrlService::new(crl.subscribe())),
            async move {
                let _ = stopped.await;
            },
        ));
        Upstream {
            url,
            registry,
            crl,
            stop: Some(stop),
        }
    }
}

pub fn paths(dir: &Path, name: &str) -> TlsPaths {
    let dir = dir.join(name);
    TlsPaths {
        ca_cert: dir.join("ca.pem"),
        cert: dir.join("cert.pem"),
        key: dir.join("key.pem"),
    }
}

pub fn url(listener: &TcpListener) -> String {
    format!(
        "https://localhost:{}",
        listener.local_addr().unwrap().port()
    )
}

/// Short server timeouts, so tests see disconnects quickly.
pub fn server_settings() -> grpc::StreamSettings {
    grpc::StreamSettings {
        hello_timeout: Duration::from_secs(2),
        heartbeat_timeout: Duration::from_secs(30),
        outbound_capacity: 8,
        last_seen_interval: Duration::from_secs(3600),
    }
}

pub struct Upstream {
    pub url: String,
    pub registry: Registry,
    pub crl: CrlPublisher,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Upstream {
    /// Stops serving and closes every stream, like a server shutdown.
    pub fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }

    pub async fn wait_until(&self, what: &str, f: impl Fn(&Registry) -> bool) {
        wait_until(what, || f(&self.registry)).await
    }
}

pub async fn wait_until(what: &str, f: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while !f() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A relay with its own stream, CRL and listener.
pub struct Relay {
    pub url: String,
    pub up: watch::Receiver<bool>,
    _identity: watch::Sender<StoredIdentity>,
    _stop: Trigger,
}

impl Relay {
    /// Starts a relay and waits for its own stream to open.
    pub async fn start(identity: StoredIdentity, upstream_url: &str) -> Self {
        let relay = Self::start_unchecked(identity, upstream_url).await;
        let up = relay.up.clone();
        wait_until("relay stream open", || *up.borrow()).await;
        relay
    }

    pub async fn start_unchecked(identity: StoredIdentity, upstream_url: &str) -> Self {
        let config = RelayConfig {
            upstream_url: upstream_url.into(),
            heartbeat_interval: 1,
            ..RelayConfig::default()
        };
        let (trigger, stop) = shutdown::channel();
        let (identity_tx, identities) = watch::channel(identity);
        let fast = || Backoff::new(Duration::from_millis(50), Duration::from_millis(200));
        let control = ControlSettings {
            backoff: fast(),
            ..ControlSettings::from_config(&config)
        };
        let (control, up) = Control::new(control, identities.clone());
        tokio::spawn(control.run(stop.clone()));
        let (publisher, tls) = TlsPublisher::start(
            CrlSettings::for_upstream(upstream_url),
            identities.clone(),
            fast(),
            stop.clone(),
        )
        .await
        .unwrap()
        .unwrap();
        tokio::spawn(publisher.run(stop.clone()));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = url(&listener);
        let forwarder = Forwarder::new(
            ForwardSettings::from_config(&config),
            identities,
            up.clone(),
        );
        let mut stopped = stop;
        tokio::spawn(listener::serve(listener, tls, forwarder, async move {
            stopped.wait().await
        }));
        Self {
            url,
            up,
            _identity: identity_tx,
            _stop: trigger,
        }
    }
}

pub fn hello(agent_id: &str) -> Envelope {
    Envelope {
        sequence: 1,
        payload: Some(Payload::Hello(Hello {
            agent_id: agent_id.into(),
            protocol_version: PROTOCOL_VERSION.into(),
            agent_version: "test".into(),
            hostname: format!("{agent_id}.example"),
            os_family: "debian".into(),
            os_version: "debian 12".into(),
        })),
    }
}

/// An agent's raw stream, to see exactly what goes up and down.
pub struct AgentStream {
    pub tx: mpsc::Sender<Envelope>,
    pub inbound: Streaming<Envelope>,
}

impl AgentStream {
    pub async fn send(&self, sequence: u64, payload: Payload) {
        let envelope = Envelope {
            sequence,
            payload: Some(payload),
        };
        self.tx.send(envelope).await.unwrap();
    }

    /// The next envelope from upstream.
    pub async fn next(&mut self) -> Result<Option<Envelope>, Box<Status>> {
        tokio::time::timeout(TIMEOUT, self.inbound.message())
            .await
            .expect("no envelope in time")
            .map_err(Box::new)
    }
}

/// Opens a stream to `url` as `agent`, with `first` as its first envelope.
pub async fn open(
    url: &str,
    agent: &StoredIdentity,
    first: Envelope,
) -> Result<AgentStream, Box<Status>> {
    let config = agent.tls.client_config().unwrap();
    let channel = match tls::connect(url, config, TIMEOUT).await {
        Ok(channel) => channel,
        Err(error) => return Err(Box::new(Status::unavailable(error.to_string()))),
    };
    let (tx, rx) = mpsc::channel(8);
    tx.send(first).await.unwrap();
    let mut client = AgentServiceClient::new(channel);
    let call = client.stream(ReceiverStream::new(rx));
    let response = tokio::time::timeout(TIMEOUT, call)
        .await
        .expect("no answer in time")
        .map_err(Box::new)?;
    Ok(AgentStream {
        tx,
        inbound: response.into_inner(),
    })
}
