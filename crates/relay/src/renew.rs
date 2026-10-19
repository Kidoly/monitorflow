//! The relay's `CertificateService` (ADR-0028): an agent behind the relay
//! renews its certificate as it would with the server, and the relay
//! forwards the call upstream as `RenewForwarded`, with its own certificate
//! and the agent's.
//!
//! The agent's certificate is the one its connection presented, which the
//! listener verified against the CA and the CRL; nothing in the request
//! says who the agent is. The upstream applies the relay policy, the rate
//! limit and every check of ADR-0010 to that agent.
//!
//! Downstream relays renew here too, for themselves with `Renew` or for
//! what is below them with `RenewForwarded`; the relay passes those on
//! with the path of the call (ADR-0028), like streams.

// Every check here ends as the `Status` tonic returns by value.
#![allow(clippy::result_large_err)]

use std::sync::Arc;
use std::time::Duration;

use monitorflow_common::client::identity::StoredIdentity;
use monitorflow_common::proto::v1::certificate_service_client::CertificateServiceClient;
use monitorflow_common::proto::v1::certificate_service_server::{
    CertificateService, CertificateServiceServer,
};
use monitorflow_common::proto::v1::{ForwardedRenewRequest, RenewRequest, RenewResponse};
use monitorflow_common::tls::{self, PeerIdentity, PeerRole};
use monitorflow_common::via::Via;
use tokio::sync::watch;
use tonic::{Request, Response, Status};

use crate::forward::{downstream_status, UPSTREAM_UNAVAILABLE};

/// Largest request accepted, as on the server: a CSR fits in a few KiB.
const MAX_REQUEST_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone)]
pub struct RenewSettings {
    pub upstream_url: String,
    pub connect_timeout: Duration,
    /// Time allowed for the upstream to answer once connected.
    pub call_timeout: Duration,
}

impl RenewSettings {
    pub fn for_upstream(upstream_url: &str) -> Self {
        Self {
            upstream_url: upstream_url.to_owned(),
            connect_timeout: tls::DEFAULT_TIMEOUT,
            // Below the agent's own 30 s, so the agent gets our answer.
            call_timeout: Duration::from_secs(20),
        }
    }
}

/// Forwards renewals upstream. Cheap to clone.
#[derive(Clone)]
pub struct RenewProxy {
    settings: Arc<RenewSettings>,
    identity: watch::Receiver<StoredIdentity>,
}

impl RenewProxy {
    /// `identity` is the relay's own, followed through renewals.
    pub fn new(settings: RenewSettings, identity: watch::Receiver<StoredIdentity>) -> Self {
        Self {
            settings: Arc::new(settings),
            identity,
        }
    }

    /// The tonic service, with the request size limit.
    pub fn into_service(self) -> CertificateServiceServer<Self> {
        CertificateServiceServer::new(self).max_decoding_message_size(MAX_REQUEST_BYTES)
    }

    async fn forward(
        &self,
        peer: &PeerIdentity,
        via: Via,
        request: ForwardedRenewRequest,
    ) -> Result<RenewResponse, Status> {
        let url = &self.settings.upstream_url;
        let unavailable = |error: String| {
            tracing::warn!(%peer, %error, "cannot forward a renewal");
            Status::unavailable(UPSTREAM_UNAVAILABLE)
        };
        let config = self
            .identity
            .borrow()
            .tls
            .client_config()
            .map_err(|e| unavailable(e.to_string()))?;
        let channel = tls::connect(url, config, self.settings.connect_timeout)
            .await
            .map_err(|e| unavailable(tls::error_chain(&e)))?;
        let mut client = CertificateServiceClient::new(channel);
        let mut request = Request::new(request);
        via.insert_into(request.metadata_mut());
        let call = client.renew_forwarded(request);
        match tokio::time::timeout(self.settings.call_timeout, call).await {
            Err(_) => Err(unavailable("no answer in time".into())),
            Ok(Err(status)) => {
                let status = downstream_status(peer.id(), status);
                tracing::info!(%peer, %status, "the upstream refused a renewal");
                Err(status)
            }
            Ok(Ok(response)) => {
                tracing::info!(%peer, "renewal forwarded");
                Ok(response.into_inner())
            }
        }
    }
}

/// Agents and downstream relays renew through the relay; the upstream
/// decides which.
pub fn check_peer(peer: &PeerIdentity) -> Result<(), Status> {
    match peer.role() {
        PeerRole::Agent | PeerRole::Relay => Ok(()),
        PeerRole::Server => Err(Status::permission_denied(
            "role server may not renew through a relay",
        )),
    }
}

#[tonic::async_trait]
impl CertificateService for RenewProxy {
    async fn renew(
        &self,
        request: Request<RenewRequest>,
    ) -> Result<Response<RenewResponse>, Status> {
        let leaf = request
            .peer_certs()
            .and_then(|certs| certs.first().cloned())
            .ok_or_else(|| Status::unauthenticated("no client certificate"))?;
        let peer =
            PeerIdentity::from_cert(&leaf).map_err(|e| Status::unauthenticated(e.to_string()))?;
        check_peer(&peer).inspect_err(|status| {
            tracing::warn!(%peer, %status, "renewal refused");
        })?;
        let forwarded = ForwardedRenewRequest {
            csr_pem: request.into_inner().csr_pem,
            peer_cert_der: leaf.to_vec(),
        };
        let via = Via::origin(peer.clone());
        self.forward(&peer, via, forwarded).await.map(Response::new)
    }

    /// A downstream relay's forwarded renewal: passed on with the path it
    /// sent plus that relay. The upstream checks the certificate is the
    /// path's origin.
    async fn renew_forwarded(
        &self,
        request: Request<ForwardedRenewRequest>,
    ) -> Result<Response<RenewResponse>, Status> {
        let peer = tls::peer_identity(&request)?;
        if peer.role() != PeerRole::Relay {
            tracing::warn!(%peer, "forwarded renewal refused: not a relay");
            return Err(Status::permission_denied(format!(
                "{peer} may not forward renewals"
            )));
        }
        let via = Via::extend(Via::from_metadata(request.metadata())?, &peer)?;
        self.forward(&peer, via, request.into_inner())
            .await
            .map(Response::new)
    }
}

#[cfg(test)]
mod tests {
    use tonic::Code;

    use super::*;

    #[test]
    fn agents_and_relays_renew_through_the_relay() {
        for role in [PeerRole::Agent, PeerRole::Relay] {
            assert!(check_peer(&PeerIdentity::new(role, "x").unwrap()).is_ok());
        }
        let server = PeerIdentity::new(PeerRole::Server, "x").unwrap();
        assert_eq!(
            check_peer(&server).unwrap_err().code(),
            Code::PermissionDenied
        );
    }
}
