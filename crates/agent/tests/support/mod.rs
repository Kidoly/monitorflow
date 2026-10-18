//! Real server pieces in-process on 127.0.0.1, adapted from
//! `crates/server/tests/` (copied, not shared, so the server's tests stay
//! theirs to change).
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use monitorflow_common::config::{AgentConfig, TlsPaths};
use monitorflow_common::redact::Secret;
use monitorflow_common::tls::{Identity, PeerRole};
use monitorflow_server::ca::{CaOptions, CertificateAuthority};
use monitorflow_server::enroll::token::{self, Creator, TokenRequest};
use monitorflow_server::enroll::{self, server_cert, Enroller, FailureLimiter};
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
