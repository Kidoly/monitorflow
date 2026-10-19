//! `CertificateService::Renew` (ADR-0010): an enrolled agent or relay
//! exchanges a CSR for a new certificate before its current one expires.
//!
//! Served on the mTLS gRPC listener, so the client certificate of the
//! connection authenticates the call. The identity written in the new
//! certificate is the one of that certificate, never anything from the CSR,
//! and so are a relay's DNS names and IP addresses (ADR-0025).
//!
//! `RenewForwarded` is the same renewal for a peer behind a relay: the
//! relay verified the peer's certificate on its own listener and forwards
//! it, and the server checks the relay may speak for that peer (ADR-0028).

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use monitorflow_common::proto::v1::certificate_service_server::{
    CertificateService, CertificateServiceServer,
};
use monitorflow_common::proto::v1::{ForwardedRenewRequest, RenewRequest, RenewResponse};
use monitorflow_common::tls::{PeerIdentity, PeerRole};
use rcgen::CertificateSigningRequestParams;
use rustls::pki_types::CertificateDer;
use sqlx::{PgPool, Postgres, Transaction};
use tonic::{Request, Response, Status};
use x509_parser::extensions::GeneralName;
use x509_parser::prelude::{FromDer, X509Certificate};

use crate::api::audit::{self, Event};
use crate::ca::{CertificateAuthority, IssuedCert, Serial, ServerName};

/// Largest request accepted: a CSR fits in a few KiB.
const MAX_REQUEST_BYTES: usize = 16 * 1024;
/// Default minimum time between two certificates of one peer.
pub const DEFAULT_MIN_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Why a renewal failed. The caller is authenticated, so refusals tell it
/// why: that helps whoever reads the peer's logs, and says nothing an
/// attacker without the key could use.
#[derive(Debug, thiserror::Error)]
pub enum RenewError {
    #[error("renewal refused: {0}")]
    Refused(String),
    #[error("a certificate was issued for this peer less than {0:?} ago; retry later")]
    TooSoon(Duration),
    #[error("invalid CSR: expected a signed PKCS#10 request in PEM")]
    InvalidCsr,
    #[error("renewal failed: {0}")]
    Internal(String),
}

impl From<sqlx::Error> for RenewError {
    fn from(error: sqlx::Error) -> Self {
        Self::Internal(format!("database: {error}"))
    }
}

impl From<RenewError> for Status {
    fn from(error: RenewError) -> Self {
        let message = error.to_string();
        match error {
            RenewError::Refused(_) => Status::permission_denied(message),
            RenewError::TooSoon(_) => Status::resource_exhausted(message),
            RenewError::InvalidCsr => Status::invalid_argument(message),
            RenewError::Internal(_) => Status::internal("renewal failed"),
        }
    }
}

/// The verified client certificate of a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresentedCert {
    pub identity: PeerIdentity,
    pub serial: Serial,
    pub not_after: SystemTime,
    /// DNS names and IP addresses of a relay, copied into its next
    /// certificate. The CA wrote them from its enrollment token.
    pub server_names: Vec<ServerName>,
}

impl PresentedCert {
    pub fn from_der(der: &CertificateDer<'_>) -> Result<Self, String> {
        let identity = PeerIdentity::from_cert(der).map_err(|e| e.to_string())?;
        let (_, cert) =
            X509Certificate::from_der(der).map_err(|e| format!("cannot parse certificate: {e}"))?;
        let not_after = cert.validity().not_after.timestamp().max(0) as u64;
        let sans = cert
            .subject_alternative_name()
            .map_err(|e| format!("cannot parse the subject alternative names: {e}"))?;
        let server_names = sans
            .into_iter()
            .flat_map(|ext| ext.value.general_names.iter())
            .filter_map(|name| match name {
                GeneralName::DNSName(dns) => Some(ServerName::Dns((*dns).to_owned())),
                GeneralName::IPAddress(ip) => ip_addr(ip).map(ServerName::Ip),
                _ => None,
            })
            .collect();
        Ok(Self {
            identity,
            serial: Serial::from_bytes(cert.raw_serial()),
            not_after: UNIX_EPOCH + Duration::from_secs(not_after),
            server_names,
        })
    }

    /// The leaf certificate the client presented on the connection that
    /// carried `request`; `UNAUTHENTICATED` without one.
    #[allow(clippy::result_large_err)]
    pub fn from_request<T>(request: &Request<T>) -> Result<Self, Status> {
        let leaf = request
            .peer_certs()
            .and_then(|certs| certs.first().cloned())
            .ok_or_else(|| Status::unauthenticated("no client certificate"))?;
        Self::from_der(&leaf).map_err(Status::unauthenticated)
    }
}

/// An IPv4 or IPv6 address in SAN encoding.
fn ip_addr(bytes: &[u8]) -> Option<std::net::IpAddr> {
    match bytes.len() {
        4 => <[u8; 4]>::try_from(bytes).ok().map(Into::into),
        16 => <[u8; 16]>::try_from(bytes).ok().map(Into::into),
        _ => None,
    }
}

/// What a successful renewal returns.
#[derive(Debug)]
pub struct Renewed {
    pub identity: PeerIdentity,
    pub cert: IssuedCert,
}

/// Issues renewed certificates. Cheap to clone.
#[derive(Clone)]
pub struct Renewer {
    pool: PgPool,
    ca: Arc<CertificateAuthority>,
    min_interval: Duration,
}

impl Renewer {
    pub fn new(pool: PgPool, ca: Arc<CertificateAuthority>) -> Self {
        Self {
            pool,
            ca,
            min_interval: DEFAULT_MIN_INTERVAL,
        }
    }

    /// Minimum time between two certificates of one peer, enrollment
    /// included. Zero disables the limit (tests).
    pub fn with_min_interval(mut self, min_interval: Duration) -> Self {
        self.min_interval = min_interval;
        self
    }

    /// The tonic service, with the request size limit.
    pub fn into_service(self) -> CertificateServiceServer<Self> {
        CertificateServiceServer::new(self).max_decoding_message_size(MAX_REQUEST_BYTES)
    }

    /// Signs the key of `csr_pem` for the identity of `presented`. The
    /// certificate record and the audit row commit together. The presented
    /// certificate stays valid until it expires.
    pub async fn renew(
        &self,
        presented: &PresentedCert,
        csr_pem: &str,
    ) -> Result<Renewed, RenewError> {
        self.issue_renewal(presented, csr_pem, None).await
    }

    /// [`Self::renew`] for the peer whose certificate `relay` forwards
    /// (ADR-0028). The certificate must be one this CA issued, valid now,
    /// and the relay must be allowed to speak for its peer by the relay
    /// policy (ADR-0004): only agents of the relay's own site. Everything
    /// else, rate limit included, applies to the peer as if it had called
    /// directly.
    pub async fn renew_forwarded(
        &self,
        relay: &PeerIdentity,
        peer_cert: &CertificateDer<'_>,
        csr_pem: &str,
    ) -> Result<Renewed, RenewError> {
        let refused = |why: String| Err(RenewError::Refused(why));
        if relay.role() != PeerRole::Relay {
            return refused(format!("{relay} may not forward renewals"));
        }
        if let Err(why) = self.ca.verify_issued(peer_cert) {
            return refused(format!("the forwarded certificate is not valid: {why}"));
        }
        let presented = PresentedCert::from_der(peer_cert).map_err(RenewError::Refused)?;
        let peer = &presented.identity;
        let allowed = match peer.role() {
            PeerRole::Agent => crate::db::relay_may_speak_for(&self.pool, relay.id(), peer.id())
                .await
                .map_err(|e| RenewError::Internal(e.to_string()))?,
            PeerRole::Relay | PeerRole::Server => false,
        };
        if !allowed {
            return refused(format!("{relay} may not renew for {peer}"));
        }
        self.issue_renewal(&presented, csr_pem, Some(relay.id()))
            .await
    }

    /// The checks of ADR-0010 and the issuance, for `presented` as seen by
    /// the server or, when `via_relay` is set, by that relay.
    async fn issue_renewal(
        &self,
        presented: &PresentedCert,
        csr_pem: &str,
        via_relay: Option<&str>,
    ) -> Result<Renewed, RenewError> {
        let peer = &presented.identity;
        let refused = |why: &str| Err(RenewError::Refused(why.to_owned()));
        if peer.role() == PeerRole::Server {
            return refused("the server renews its own certificate at start");
        }
        // The handshake checked expiry and the CRL, but a connection can
        // outlive both.
        if presented.not_after <= SystemTime::now() {
            return refused("the presented certificate has expired");
        }
        if self.ca.is_revoked(&presented.serial) {
            return refused("the presented certificate is revoked");
        }
        CertificateSigningRequestParams::from_pem(csr_pem).map_err(|_| RenewError::InvalidCsr)?;

        let (role, id, uri) = (peer.role(), peer.id(), peer.uri());
        let mut tx = self.pool.begin().await?;
        // One renewal per peer at a time, so concurrent calls cannot both
        // pass the interval check below.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(&uri)
            .execute(&mut *tx)
            .await?;
        let recorded: Option<(String, String, bool, bool)> = sqlx::query_as(
            "SELECT peer_role, peer_id, revoked_at IS NOT NULL, not_after <= now()
             FROM certificates WHERE serial = $1",
        )
        .bind(presented.serial.as_bytes())
        .fetch_optional(&mut *tx)
        .await?;
        match recorded {
            None => return refused("the presented certificate was not issued by this server"),
            Some((r, i, ..)) if r != role.as_str() || i != id => {
                return refused("the presented certificate is recorded for another peer")
            }
            Some((_, _, true, _)) => return refused("the presented certificate is revoked"),
            Some((_, _, _, true)) => return refused("the presented certificate has expired"),
            Some(_) => {}
        }
        check_inventory(&mut tx, peer).await?;
        let too_soon: Option<bool> = sqlx::query_scalar(
            "SELECT max(issued_at) > now() - make_interval(secs => $3)
             FROM certificates WHERE peer_role = $1 AND peer_id = $2",
        )
        .bind(role.as_str())
        .bind(id)
        .bind(self.min_interval.as_secs_f64())
        .fetch_one(&mut *tx)
        .await?;
        if too_soon == Some(true) {
            return Err(RenewError::TooSoon(self.min_interval));
        }

        let (ca, csr, identity) = (self.ca.clone(), csr_pem.to_owned(), peer.clone());
        let names = presented.server_names.clone();
        let cert = tokio::task::spawn_blocking(move || ca.issue(&csr, &identity, None, &names))
            .await
            .map_err(|e| RenewError::Internal(e.to_string()))?
            .map_err(|e| RenewError::Internal(e.to_string()))?;
        sqlx::query(
            "INSERT INTO certificates (serial, peer_role, peer_id, not_before, not_after)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(cert.serial.as_bytes())
        .bind(role.as_str())
        .bind(id)
        .bind(DateTime::<Utc>::from(cert.not_before))
        .bind(DateTime::<Utc>::from(cert.not_after))
        .execute(&mut *tx)
        .await?;
        audit::record(
            &mut *tx,
            Event {
                actor_kind: "peer",
                actor_id: &uri,
                action: "peer.renew",
                target: Some((role.as_str(), id)),
                details: serde_json::json!({
                    "serial": cert.serial.to_string(),
                    "previous_serial": presented.serial.to_string(),
                    "not_after": DateTime::<Utc>::from(cert.not_after),
                    "via_relay": via_relay,
                }),
            },
        )
        .await?;
        tx.commit().await?;
        tracing::info!(%peer, serial = %cert.serial, previous = %presented.serial, ?via_relay, "renewed a certificate");
        Ok(Renewed {
            identity: peer.clone(),
            cert,
        })
    }
}

/// A revoked inventory row refuses the renewal, as it refuses the stream
/// (ADR-0008). The row is locked until commit, so a revocation cannot slip
/// in between. An agent may have no row yet if it never said Hello: its
/// recorded certificate proves the enrollment. A relay always has one.
async fn check_inventory(
    tx: &mut Transaction<'_, Postgres>,
    peer: &PeerIdentity,
) -> Result<(), RenewError> {
    let sql = match peer.role() {
        PeerRole::Relay => "SELECT revoked_at IS NOT NULL FROM relays WHERE peer_id = $1 FOR SHARE",
        _ => "SELECT revoked_at IS NOT NULL FROM servers WHERE peer_id = $1 FOR SHARE",
    };
    let revoked: Option<bool> = sqlx::query_scalar(sql)
        .bind(peer.id())
        .fetch_optional(&mut **tx)
        .await?;
    match (peer.role(), revoked) {
        (role, Some(true)) => Err(RenewError::Refused(format!("this {role} is revoked"))),
        (PeerRole::Relay, None) => Err(RenewError::Refused("this relay is not enrolled".into())),
        _ => Ok(()),
    }
}

#[tonic::async_trait]
impl CertificateService for Renewer {
    async fn renew(
        &self,
        request: Request<RenewRequest>,
    ) -> Result<Response<RenewResponse>, Status> {
        let presented = PresentedCert::from_request(&request)?;
        let peer = presented.identity.clone();
        let result = Renewer::renew(self, &presented, &request.into_inner().csr_pem).await;
        self.respond(&peer, result)
    }

    async fn renew_forwarded(
        &self,
        request: Request<ForwardedRenewRequest>,
    ) -> Result<Response<RenewResponse>, Status> {
        let relay = monitorflow_common::tls::peer_identity(&request)?;
        let request = request.into_inner();
        let cert = CertificateDer::from(request.peer_cert_der);
        let result = Renewer::renew_forwarded(self, &relay, &cert, &request.csr_pem).await;
        self.respond(&relay, result)
    }
}

impl Renewer {
    /// The response for `result`, logged with the calling `peer`.
    #[allow(clippy::result_large_err)]
    fn respond(
        &self,
        peer: &PeerIdentity,
        result: Result<Renewed, RenewError>,
    ) -> Result<Response<RenewResponse>, Status> {
        let renewed = match result {
            Ok(renewed) => renewed,
            Err(error) => {
                match &error {
                    RenewError::Internal(why) => {
                        tracing::error!(%peer, error = %why, "renewal failed")
                    }
                    RenewError::Refused(_) => tracing::warn!(%peer, %error, "renewal refused"),
                    _ => tracing::info!(%peer, %error, "renewal rejected"),
                }
                return Err(error.into());
            }
        };
        let not_after_unix = renewed
            .cert
            .not_after
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        Ok(Response::new(RenewResponse {
            cert_pem: renewed.cert.cert_pem,
            ca_cert_pem: self.ca.ca_cert_pem().to_owned(),
            peer_uri: renewed.identity.uri(),
            not_after_unix,
        }))
    }
}

#[cfg(test)]
mod tests {
    use tonic::Code;

    use super::*;

    #[test]
    fn errors_map_to_grpc_codes() {
        let code = |e: RenewError| Status::from(e).code();
        assert_eq!(
            code(RenewError::Refused("revoked".into())),
            Code::PermissionDenied
        );
        assert_eq!(
            code(RenewError::TooSoon(DEFAULT_MIN_INTERVAL)),
            Code::ResourceExhausted
        );
        assert_eq!(code(RenewError::InvalidCsr), Code::InvalidArgument);
        let internal = Status::from(RenewError::from(sqlx::Error::PoolClosed));
        assert_eq!(internal.code(), Code::Internal);
        assert!(!internal.message().contains("pool"), "{internal}");
    }

    #[test]
    fn presented_cert_reads_identity_serial_and_expiry() {
        let dir = tempfile::TempDir::new().unwrap();
        let ca = CertificateAuthority::open_or_create(dir.path(), Default::default()).unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let csr = rcgen::CertificateParams::default()
            .serialize_request(&key)
            .unwrap()
            .pem()
            .unwrap();
        let id = PeerIdentity::new(PeerRole::Relay, "r1").unwrap();
        let issued = ca.issue(&csr, &id, None, &[]).unwrap();
        let der = CertificateDer::from(pem::parse(&issued.cert_pem).unwrap().into_contents());
        let presented = PresentedCert::from_der(&der).unwrap();
        assert_eq!(presented.identity, id);
        assert_eq!(presented.serial, issued.serial);
        assert_eq!(presented.not_after, issued.not_after);
        assert!(PresentedCert::from_der(&CertificateDer::from(vec![1, 2, 3])).is_err());
    }
}
