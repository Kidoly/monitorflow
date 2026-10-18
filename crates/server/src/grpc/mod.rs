//! gRPC ingress: the `AgentService::Stream` endpoint and the registry of
//! connected agents and relays.

mod policy;
mod registry;
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

pub use policy::{AllowAllRelayedAgents, DbRelayPolicy, DenyRelayedAgents, RelayPolicy};
pub use registry::{CloseReason, Counters, PeerInfo, Registration, Registry, RegistryError};
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
}

impl Default for StreamSettings {
    fn default() -> Self {
        Self {
            hello_timeout: Duration::from_secs(10),
            heartbeat_timeout: Duration::from_secs(ServerConfig::default().heartbeat_timeout),
            outbound_capacity: 64,
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
    let registry = service.registry().clone();
    let incoming = tls::incoming(listener, tls, AcceptOptions::default());
    Server::builder()
        .add_service(AgentServiceServer::new(service))
        .serve_with_incoming_shutdown(incoming, async move {
            shutdown.await;
            registry.close_all();
        })
        .await
}
