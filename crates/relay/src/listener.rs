//! The relay's mTLS listener for agents (ADR-0025): the relay's own
//! certificate, agents' certificates checked against the CA and the CRL
//! fetched from the upstream with `RevocationService.GetCrl`. It serves
//! `AgentService` and `CertificateService`, and `RevocationService` from
//! that copy to downstream relays (ADR-0028).
//!
//! [`TlsPublisher`] refetches the CRL every hour, retrying every minute
//! after a failure, and rebuilds the configuration when the relay's
//! certificate is renewed. Like the server, the listener enforces the CRL's
//! `next_update`: a relay that cannot refresh it for about a day refuses
//! its agents.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use monitorflow_common::client::backoff::Backoff;
use monitorflow_common::client::identity::StoredIdentity;
use monitorflow_common::client::shutdown::Shutdown;
use monitorflow_common::proto::v1::agent_service_server::AgentServiceServer;
use monitorflow_common::proto::v1::revocation_service_client::RevocationServiceClient;
use monitorflow_common::proto::v1::revocation_service_server::{
    RevocationService, RevocationServiceServer,
};
use monitorflow_common::proto::v1::{GetCrlRequest, GetCrlResponse};
use monitorflow_common::tls::{self, error_chain, AcceptOptions, PeerRole};
use rustls::pki_types::CertificateRevocationListDer;
use rustls::ServerConfig;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tonic::transport::Server;
use x509_parser::extensions::GeneralName;
use x509_parser::prelude::{FromDer, X509Certificate};

use crate::forward::Forwarder;
use crate::renew::RenewProxy;

const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// HTTP/2 pings towards agents: a silent agent's stream ends, and so does
/// its upstream stream, so the server sees it offline.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, thiserror::Error)]
pub enum CrlError {
    #[error("cannot fetch the CRL: {0}")]
    Fetch(String),
    #[error("the upstream refused the CRL request: {0}")]
    Refused(String),
    #[error("cannot build the listener's TLS configuration: {0}")]
    Config(String),
}

#[derive(Debug, Clone)]
pub struct CrlSettings {
    pub upstream_url: String,
    pub refresh_every: Duration,
    pub retry: Duration,
    pub connect_timeout: Duration,
}

impl CrlSettings {
    pub fn for_upstream(upstream_url: &str) -> Self {
        Self {
            upstream_url: upstream_url.to_owned(),
            refresh_every: Duration::from_secs(60 * 60),
            retry: Duration::from_secs(60),
            connect_timeout: tls::DEFAULT_TIMEOUT,
        }
    }
}

/// One `GetCrl` call with the relay's certificate.
pub async fn fetch_crl(
    identity: &StoredIdentity,
    settings: &CrlSettings,
) -> Result<CertificateRevocationListDer<'static>, CrlError> {
    let config = identity
        .tls
        .client_config()
        .map_err(|e| CrlError::Config(e.to_string()))?;
    let channel = tls::connect(&settings.upstream_url, config, settings.connect_timeout)
        .await
        .map_err(|e| CrlError::Fetch(e.to_string()))?;
    let mut client = RevocationServiceClient::new(channel);
    let call = client.get_crl(GetCrlRequest {});
    match tokio::time::timeout(CALL_TIMEOUT, call).await {
        Err(_) => Err(CrlError::Fetch("no answer in time".into())),
        Ok(Err(status)) if status.code() == tonic::Code::PermissionDenied => {
            Err(CrlError::Refused(status.message().to_owned()))
        }
        Ok(Err(status)) => Err(CrlError::Fetch(error_chain(&status))),
        Ok(Ok(response)) => Ok(CertificateRevocationListDer::from(
            response.into_inner().crl_der,
        )),
    }
}

/// Server configuration for the agent listener: the relay's certificate,
/// clients checked against the CA and `crl`. An unparsable CRL fails here.
pub fn server_config(
    identity: &StoredIdentity,
    crl: &CertificateRevocationListDer<'static>,
) -> Result<Arc<ServerConfig>, CrlError> {
    identity
        .tls
        .server_config(vec![crl.clone()])
        .map_err(|e| CrlError::Config(e.to_string()))
}

/// DNS names and IP addresses in the relay's certificate, which agents
/// verify against their `upstream_url`. Empty means the relay cannot accept
/// agents.
pub fn server_names(identity: &StoredIdentity) -> Result<Vec<String>, String> {
    let (_, cert) = X509Certificate::from_der(identity.tls.leaf().as_ref())
        .map_err(|e| format!("cannot parse the relay certificate: {e}"))?;
    let sans = cert
        .subject_alternative_name()
        .map_err(|e| format!("cannot parse the relay certificate: {e}"))?;
    Ok(sans
        .into_iter()
        .flat_map(|ext| ext.value.general_names.iter())
        .filter_map(|name| match name {
            GeneralName::DNSName(dns) => Some((*dns).to_owned()),
            GeneralName::IPAddress(ip) => ip_addr(ip).map(|ip| ip.to_string()),
            _ => None,
        })
        .collect())
}

fn ip_addr(bytes: &[u8]) -> Option<std::net::IpAddr> {
    match bytes.len() {
        4 => <[u8; 4]>::try_from(bytes).ok().map(Into::into),
        16 => <[u8; 16]>::try_from(bytes).ok().map(Into::into),
        _ => None,
    }
}

/// Keeps the listener's configuration current.
pub struct TlsPublisher {
    settings: CrlSettings,
    identity: watch::Receiver<StoredIdentity>,
    crl: watch::Sender<CertificateRevocationListDer<'static>>,
    tx: watch::Sender<Arc<ServerConfig>>,
}

impl TlsPublisher {
    /// Fetches the first CRL, retrying with backoff; `None` on shutdown.
    /// Pass the receiver to [`serve`].
    pub async fn start(
        settings: CrlSettings,
        mut identity: watch::Receiver<StoredIdentity>,
        mut backoff: Backoff,
        mut shutdown: Shutdown,
    ) -> Result<Option<(Self, watch::Receiver<Arc<ServerConfig>>)>, CrlError> {
        loop {
            let current = identity.borrow_and_update().clone();
            let attempt = tokio::select! {
                result = fetch_crl(&current, &settings) => result,
                () = shutdown.wait() => return Ok(None),
            };
            let error = match attempt.and_then(|crl| Ok((server_config(&current, &crl)?, crl))) {
                Ok((config, crl)) => {
                    let (tx, rx) = watch::channel(config);
                    let publisher = Self {
                        settings,
                        identity,
                        crl: watch::channel(crl).0,
                        tx,
                    };
                    return Ok(Some((publisher, rx)));
                }
                Err(error @ CrlError::Config(_)) => return Err(error),
                Err(error) => error,
            };
            let delay = backoff.next_delay();
            tracing::warn!(%error, retry_in = ?delay, "no CRL yet; agents are not accepted until there is one");
            tokio::select! {
                () = tokio::time::sleep(delay) => {}
                () = shutdown.wait() => return Ok(None),
            }
        }
    }

    /// The CRL the listener enforces, for [`CrlCache`].
    pub fn crl(&self) -> watch::Receiver<CertificateRevocationListDer<'static>> {
        self.crl.subscribe()
    }

    pub async fn run(mut self, mut shutdown: Shutdown) {
        let mut wait = self.settings.refresh_every;
        // Set when the renewal task is gone.
        let mut fixed = false;
        loop {
            tokio::select! {
                () = shutdown.wait() => return,
                () = tokio::time::sleep(wait) => {
                    let current = self.identity.borrow().clone();
                    wait = match fetch_crl(&current, &self.settings).await {
                        Ok(crl) => {
                            self.crl.send_replace(crl);
                            self.publish(&current);
                            self.settings.refresh_every
                        }
                        Err(error) => {
                            tracing::error!(%error, "cannot refresh the CRL; agents are refused once it expires");
                            self.settings.retry.min(self.settings.refresh_every)
                        }
                    };
                }
                changed = self.identity.changed(), if !fixed => match changed {
                    Ok(()) => {
                        let current = self.identity.borrow_and_update().clone();
                        self.publish(&current);
                    }
                    Err(_) => fixed = true,
                },
            }
        }
    }

    fn publish(&self, identity: &StoredIdentity) {
        match server_config(identity, &self.crl.borrow()) {
            Ok(config) => {
                self.tx.send_replace(config);
                tracing::debug!("listener TLS configuration updated");
            }
            Err(error) => tracing::error!(%error, "keeping the previous TLS configuration"),
        }
    }
}

/// `RevocationService` for downstream relays, from the relay's own copy of
/// the CRL. The CRL is signed by the CA, so a relay can serve an old one
/// but not a forged one, and its `next_update` still bounds how old.
#[derive(Clone)]
pub struct CrlCache {
    crl: watch::Receiver<CertificateRevocationListDer<'static>>,
}

impl CrlCache {
    pub fn new(crl: watch::Receiver<CertificateRevocationListDer<'static>>) -> Self {
        Self { crl }
    }
}

#[tonic::async_trait]
impl RevocationService for CrlCache {
    async fn get_crl(
        &self,
        request: tonic::Request<GetCrlRequest>,
    ) -> Result<tonic::Response<GetCrlResponse>, tonic::Status> {
        let peer = tls::peer_identity(&request)?;
        if peer.role() != PeerRole::Relay {
            tracing::info!(%peer, "CRL request refused: not a relay");
            return Err(tonic::Status::permission_denied(
                "only relays may fetch the CRL",
            ));
        }
        let crl_der = self.crl.borrow().as_ref().to_vec();
        tracing::debug!(%peer, bytes = crl_der.len(), "CRL sent");
        Ok(tonic::Response::new(GetCrlResponse { crl_der }))
    }
}

/// What the relay serves on its listener.
pub struct Services {
    pub forwarder: Forwarder,
    pub renew: RenewProxy,
    pub crl: CrlCache,
}

/// Serves `services` to agents and downstream relays over mTLS until
/// `shutdown` resolves.
pub async fn serve(
    listener: TcpListener,
    tls: watch::Receiver<Arc<ServerConfig>>,
    services: Services,
    shutdown: impl Future<Output = ()>,
) -> Result<(), tonic::transport::Error> {
    let incoming = tls::incoming(listener, tls, AcceptOptions::default());
    Server::builder()
        .http2_keepalive_interval(Some(KEEPALIVE_INTERVAL))
        .http2_keepalive_timeout(Some(KEEPALIVE_TIMEOUT))
        .add_service(AgentServiceServer::new(services.forwarder))
        .add_service(services.renew.into_service())
        .add_service(RevocationServiceServer::new(services.crl))
        .serve_with_incoming_shutdown(incoming, shutdown)
        .await
}
