//! Real server pieces in-process on 127.0.0.1, adapted from
//! `crates/server/tests/` (copied, not shared, so the server's tests stay
//! theirs to change).
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use monitorflow_agent::identity::{self, AgentIdentity, Enrolled};
use monitorflow_common::config::{AgentConfig, TlsPaths};
use monitorflow_common::redact::Secret;
use monitorflow_common::tls::{Identity, PeerIdentity, PeerRole};
use monitorflow_server::ca::{CaOptions, CertificateAuthority, IssuedCert, Serial};
use monitorflow_server::crl::CrlPublisher;
use monitorflow_server::enroll::token::{self, Creator, TokenRequest};
use monitorflow_server::enroll::{self, server_cert, Enroller, FailureLimiter};
use monitorflow_server::grpc::{self, AgentStreamService, DenyRelayedAgents, Registry};
use monitorflow_server::renew::{PresentedCert, Renewer};
use sqlx::PgPool;
use tempfile::TempDir;
use tokio::net::TcpListener;

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
        let paths = TlsPaths {
            ca_cert: dir.path().join("server-ca.pem"),
            cert: dir.path().join("server.pem"),
            key: dir.path().join("server-key.pem"),
        };
        server_cert::ensure(
            &ca,
            &paths,
            &server_cert::parse_names(&["localhost".into()]),
        )
        .unwrap();
        let server = Identity::load(&paths).unwrap();
        Self {
            dir,
            ca: Arc::new(ca),
            server,
        }
    }

    pub fn fingerprint(&self) -> [u8; 32] {
        enroll::ca_fingerprint(&self.ca)
    }

    /// A one-time agent token for `peer_id`, as the administrator gets it.
    pub async fn token(&self, pool: &PgPool, peer_id: &str) -> Secret {
        let site_id = token::site_id(pool, "paris", true).await.unwrap().unwrap();
        let request = TokenRequest {
            role: PeerRole::Agent,
            peer_id: peer_id.into(),
            site_id,
            ttl: token::DEFAULT_TTL,
            creator: Creator::Cli,
            server_names: Vec::new(),
        };
        let created = token::create(pool, self.fingerprint(), &request)
            .await
            .unwrap();
        created.token.to_secret_string()
    }

    /// Serves `Enroll` with `pool`; returns its URL.
    pub async fn serve_enroll(&self, pool: &PgPool) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = url(&listener);
        let enroller = Enroller::new(pool.clone(), self.ca.clone(), FailureLimiter::default());
        let tls = self.server.server_config_without_client_auth().unwrap();
        tokio::spawn(enroll::serve(
            listener,
            tls,
            enroller,
            std::future::pending(),
        ));
        url
    }
}

/// The gRPC listener, with its registry and CRL publisher.
pub struct Upstream {
    pub url: String,
    pub registry: Registry,
    pub crl: CrlPublisher,
}

impl Pki {
    /// Serves `AgentService` over mTLS with a CRL from the CA.
    pub async fn serve_grpc(&self, settings: grpc::StreamSettings) -> Upstream {
        self.serve(settings, None).await
    }

    /// [`Self::serve_grpc`] plus `CertificateService`, without rate limit.
    pub async fn serve_grpc_with_renewer(
        &self,
        settings: grpc::StreamSettings,
        pool: &PgPool,
    ) -> Upstream {
        let renewer = Renewer::new(pool.clone(), self.ca.clone()).with_min_interval(Duration::ZERO);
        self.serve(settings, Some(renewer)).await
    }

    async fn serve(&self, settings: grpc::StreamSettings, renewer: Option<Renewer>) -> Upstream {
        let make =
            |registry| AgentStreamService::new(registry, Arc::new(DenyRelayedAgents), settings);
        self.serve_with(make, renewer).await
    }

    /// Serves the service `make` builds around a new registry.
    pub async fn serve_service(
        &self,
        make: impl FnOnce(Registry) -> AgentStreamService,
    ) -> Upstream {
        self.serve_with(make, None).await
    }

    async fn serve_with(
        &self,
        make: impl FnOnce(Registry) -> AgentStreamService,
        renewer: Option<Renewer>,
    ) -> Upstream {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = url(&listener);
        let (crl, rx) = CrlPublisher::start(self.ca.clone(), self.server.clone())
            .await
            .unwrap();
        let registry = Registry::default();
        let service = make(registry.clone());
        tokio::spawn(grpc::serve_with_renewer(
            listener,
            rx,
            service,
            renewer,
            std::future::pending(),
        ));
        Upstream { url, registry, crl }
    }

    /// Issues an agent certificate without a token, as enrollment would,
    /// and stores it where `config` looks for it.
    pub fn agent_identity(&self, config: &AgentConfig, id: &str) -> (AgentIdentity, Serial) {
        let (identity, issued) = self.agent_identity_valid_for(config, id, None);
        (identity, issued.serial)
    }

    /// [`Self::agent_identity`] valid for `validity` instead of the CA's
    /// default. `not_before` is backdated by the CA's clock skew, so
    /// anything under 2.5 minutes is due for renewal at once.
    pub fn agent_identity_valid_for(
        &self,
        config: &AgentConfig,
        id: &str,
        validity: Option<Duration>,
    ) -> (AgentIdentity, IssuedCert) {
        let key = rcgen::KeyPair::generate().unwrap();
        let csr = rcgen::CertificateParams::default()
            .serialize_request(&key)
            .unwrap()
            .pem()
            .unwrap();
        let peer = PeerIdentity::new(PeerRole::Agent, id).unwrap();
        let issued = self.ca.issue(&csr, &peer, validity, &[]).unwrap();
        let enrolled = Enrolled {
            key_pem: Secret::new(key.serialize_pem()),
            cert_pem: issued.cert_pem.clone(),
            ca_cert_pem: self.ca.ca_cert_pem().to_owned(),
        };
        let paths = config.tls_paths();
        identity::store(&paths, &enrolled).unwrap();
        (identity::load(&paths).unwrap().unwrap(), issued)
    }
}

/// Records `issued` for agent `id` in `certificates`, as enrollment does.
pub async fn record(pool: &PgPool, id: &str, issued: &IssuedCert) {
    sqlx::query(
        "INSERT INTO certificates (serial, peer_role, peer_id, not_before, not_after)
         VALUES ($1, 'agent', $2, to_timestamp($3), to_timestamp($4))",
    )
    .bind(issued.serial.as_bytes())
    .bind(id)
    .bind(unix(issued.not_before))
    .bind(unix(issued.not_after))
    .execute(pool)
    .await
    .unwrap();
}

fn unix(t: std::time::SystemTime) -> f64 {
    t.duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

/// Serial of the certificate stored where `config` looks for it.
pub fn stored_serial(config: &AgentConfig) -> Serial {
    let cert = monitorflow_common::tls::load_certs(&config.tls_paths().cert).unwrap();
    PresentedCert::from_der(&cert[0]).unwrap().serial
}

pub fn url(listener: &TcpListener) -> String {
    format!(
        "https://localhost:{}",
        listener.local_addr().unwrap().port()
    )
}

/// An agent configuration with its own data directory.
pub fn agent_config(data_dir: PathBuf, upstream_url: &str, enroll_url: &str) -> AgentConfig {
    AgentConfig {
        upstream_url: upstream_url.into(),
        enroll_url: Some(enroll_url.into()),
        data_dir,
        heartbeat_interval: 1,
        ..AgentConfig::default()
    }
}

/// A port nothing listens on.
pub async fn closed_port_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    url(&listener)
}
