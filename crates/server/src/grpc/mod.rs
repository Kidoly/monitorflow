//! gRPC ingress: the `AgentService::Stream` endpoint, the registry of
//! connected agents and relays, and `CertificateService` and
//! `RevocationService` on the same mTLS listener.

mod inventory;
mod policy;
mod registry;
mod revocation;
mod service;

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use monitorflow_common::config::ServerConfig;
use monitorflow_common::proto::v1::agent_service_server::AgentServiceServer;
use monitorflow_common::tls::{self, AcceptOptions};
use rustls::ServerConfig as TlsServerConfig;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tonic::transport::Server;

use crate::renew::Renewer;

pub use inventory::{AcceptedHello, DbInventory, Inventory, InventoryError, NoInventory};
pub use policy::{AllowAllRelayedAgents, DbRelayPolicy, DenyRelayedAgents, RelayPolicy};
pub use registry::{CloseReason, Counters, PeerInfo, Registration, Registry, RegistryError};
pub use revocation::CrlService;
pub use service::{AgentStreamService, HandshakeError, Outbound};

/// Timeouts and queue sizes of agent streams.
#[derive(Debug, Clone)]
pub struct StreamSettings {
    /// Time allowed for the first envelope, which must be `Hello`.
    pub hello_timeout: Duration,
    /// A stream with no message for this long is closed.
    pub heartbeat_timeout: Duration,
    /// Envelopes queued towards one peer before [`Registry::send`] waits.
    pub outbound_capacity: usize,
    /// Minimum time between two `last_seen_at` writes for one stream.
    pub last_seen_interval: Duration,
}

impl Default for StreamSettings {
    fn default() -> Self {
        Self {
            hello_timeout: Duration::from_secs(10),
            heartbeat_timeout: Duration::from_secs(ServerConfig::default().heartbeat_timeout),
            outbound_capacity: 64,
            last_seen_interval: Duration::from_secs(60),
        }
    }
}

impl StreamSettings {
    pub fn from_config(config: &ServerConfig) -> Self {
        Self {
            heartbeat_timeout: Duration::from_secs(config.heartbeat_timeout),
            ..Self::default()
        }
    }
}

/// Serves `service` over mTLS on `listener` until `shutdown` resolves, then
/// stops accepting, closes every stream and waits for connections to end.
///
/// `tls` carries the rustls server config; send a new one (with a fresh CRL)
/// to apply it to new connections.
pub async fn serve(
    listener: TcpListener,
    tls: watch::Receiver<Arc<TlsServerConfig>>,
    service: AgentStreamService,
    shutdown: impl Future<Output = ()>,
) -> Result<(), tonic::transport::Error> {
    serve_with_renewer(listener, tls, service, None, shutdown).await
}

/// [`serve`], plus `CertificateService` when `renewer` is set (ADR-0010).
pub async fn serve_with_renewer(
    listener: TcpListener,
    tls: watch::Receiver<Arc<TlsServerConfig>>,
    service: AgentStreamService,
    renewer: Option<Renewer>,
    shutdown: impl Future<Output = ()>,
) -> Result<(), tonic::transport::Error> {
    serve_all(listener, tls, service, renewer, None, shutdown).await
}

/// [`serve_with_renewer`], plus `RevocationService` when `crl` is set
/// (ADR-0025).
pub async fn serve_all(
    listener: TcpListener,
    tls: watch::Receiver<Arc<TlsServerConfig>>,
    service: AgentStreamService,
    renewer: Option<Renewer>,
    crl: Option<CrlService>,
    shutdown: impl Future<Output = ()>,
) -> Result<(), tonic::transport::Error> {
    let registry = service.registry().clone();
    let incoming = tls::incoming(listener, tls, AcceptOptions::default());
    Server::builder()
        .add_service(AgentServiceServer::new(service))
        .add_optional_service(renewer.map(Renewer::into_service))
        .add_optional_service(crl.map(CrlService::into_service))
        .serve_with_incoming_shutdown(incoming, async move {
            shutdown.await;
            registry.close_all();
        })
        .await
}
