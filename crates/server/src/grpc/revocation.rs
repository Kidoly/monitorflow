//! `RevocationService`: hands relays the CRL the listener enforces, so they
//! refuse revoked agent certificates too (ADR-0025).

use monitorflow_common::proto::v1::revocation_service_server::{
    RevocationService, RevocationServiceServer,
};
use monitorflow_common::proto::v1::{GetCrlRequest, GetCrlResponse};
use monitorflow_common::tls::{self, PeerRole};
use tokio::sync::watch;
use tonic::{Request, Response, Status};

use crate::crl::Crl;

/// Serves the CRL published by [`crate::crl::CrlPublisher::subscribe`].
#[derive(Debug, Clone)]
pub struct CrlService {
    crl: watch::Receiver<Crl>,
}

impl CrlService {
    pub fn new(crl: watch::Receiver<Crl>) -> Self {
        Self { crl }
    }

    pub fn into_service(self) -> RevocationServiceServer<Self> {
        RevocationServiceServer::new(self)
    }
}

#[tonic::async_trait]
impl RevocationService for CrlService {
    async fn get_crl(
        &self,
        request: Request<GetCrlRequest>,
    ) -> Result<Response<GetCrlResponse>, Status> {
        let peer = tls::peer_identity(&request)?;
        if peer.role() != PeerRole::Relay {
            tracing::info!(%peer, "CRL request refused: not a relay");
            return Err(Status::permission_denied("only relays may fetch the CRL"));
        }
        let crl_der = self.crl.borrow().as_ref().to_vec();
        tracing::debug!(%peer, bytes = crl_der.len(), "CRL sent");
        Ok(Response::new(GetCrlResponse { crl_der }))
    }
}
