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
use monitorflow_common::via::Via;
use rcgen::CertificateSigningRequestParams;
use rustls::pki_types::CertificateDer;
use sqlx::{PgPool, Postgres, Transaction};
use tonic::{Request, Response, Status};
use x509_parser::extensions::GeneralName;
use x509_parser::prelude::{FromDer, X509Certificate};

use crate::api::audit::{self, Event};
use crate::ca::{CertificateAuthority, IssuedCert, Serial, ServerName, Trust};
use crate::enroll::server_cert::parse_names;

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
    /// A relay asked for names nobody confirmed yet (ADR-0066).
    #[error("waiting for an administrator to confirm this relay's server names")]
    NoNewNames,
    /// A peer asked whether a change of certificate authority needs a new
    /// certificate, and none does (ADR-0088).
    #[error("no change of certificate authority for this peer")]
    NoRotation,
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
            RenewError::NoNewNames | RenewError::NoRotation => Status::failed_precondition(message),
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
    /// certificate unless an administrator confirmed others (ADR-0066).
    /// The CA wrote them from its enrollment token or that confirmation.
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
    /// The CAs to send with it, read when it was issued.
    pub trust: Trust,
    /// The server's signatures of those CAs for this certificate, which
    /// let the peer take a new one (ADR-0107).
    pub ca_bundle_signatures: Vec<Vec<u8>>,
}

/// What a `Renew` asks for, beyond a CSR.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ask {
    /// Only names an administrator confirmed (ADR-0066).
    pub names_only: bool,
    /// The peer stores a CA bundle (ADR-0088).
    pub accepts_ca_bundle: bool,
    /// Only what a change of certificate authority needs (ADR-0088).
    pub rotation_only: bool,
}

impl Ask {
    /// A plain renewal by a peer that takes CA bundles.
    pub const RENEWAL: Self = Self {
        names_only: false,
        accepts_ca_bundle: true,
        rotation_only: false,
    };
}

/// Lowercase hexadecimal, as `certificates.issuer_ca` stores it.
pub fn hex(fingerprint: &monitorflow_common::tls::Fingerprint) -> String {
    monitorflow_common::tls::fingerprint_hex(fingerprint)
}

/// Parses [`hex`].
pub(crate) fn from_hex(hex: &str) -> Option<monitorflow_common::tls::Fingerprint> {
    if hex.len() != 64 || !hex.is_ascii() {
        return None;
    }
    let mut fingerprint = [0; 32];
    for (i, byte) in fingerprint.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(fingerprint)
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
        self.issue_renewal(presented, csr_pem, None, Ask::RENEWAL)
            .await
    }

    /// [`Self::renew`] for `ask`.
    pub async fn renew_for(
        &self,
        presented: &PresentedCert,
        csr_pem: &str,
        ask: Ask,
    ) -> Result<Renewed, RenewError> {
        self.issue_renewal(presented, csr_pem, None, ask).await
    }

    /// [`Self::renew`] only for the names an administrator confirmed and
    /// `presented` does not carry yet, whatever the renewal interval:
    /// [`RenewError::NoNewNames`] otherwise (ADR-0066).
    pub async fn renew_names(
        &self,
        presented: &PresentedCert,
        csr_pem: &str,
    ) -> Result<Renewed, RenewError> {
        let ask = Ask {
            names_only: true,
            ..Ask::RENEWAL
        };
        self.issue_renewal(presented, csr_pem, None, ask).await
    }

    /// [`Self::renew`] for the peer whose certificate `relay` forwards
    /// (ADR-0028), along `via` when it came through chained relays. The
    /// certificate must be one this CA issued, valid now, and the origin of
    /// `via`; the relay policy must allow the path to speak for its peer
    /// ([`crate::db::relay_route_allowed`]): agents of the attached relay's
    /// site, or a child relay. Everything else, rate limit included,
    /// applies to the peer as if it had called directly.
    pub async fn renew_forwarded(
        &self,
        relay: &PeerIdentity,
        via: Option<&Via>,
        peer_cert: &CertificateDer<'_>,
        csr_pem: &str,
    ) -> Result<Renewed, RenewError> {
        self.forwarded(relay, via, peer_cert, csr_pem, Ask::RENEWAL)
            .await
    }

    /// [`Self::renew_forwarded`] for `ask`.
    pub async fn forwarded(
        &self,
        relay: &PeerIdentity,
        via: Option<&Via>,
        peer_cert: &CertificateDer<'_>,
        csr_pem: &str,
        ask: Ask,
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
        if via.is_some_and(|via| via.origin_peer() != peer) {
            return refused(format!(
                "the forwarded certificate is not {peer}'s path origin"
            ));
        }
        let relays: Vec<&str> = std::iter::once(relay.id())
            .chain(
                via.into_iter()
                    .flat_map(|via| via.relays().rev().map(PeerIdentity::id)),
            )
            .collect();
        let allowed = crate::db::relay_route_allowed(&self.pool, &relays, peer)
            .await
            .map_err(|e| RenewError::Internal(e.to_string()))?;
        if !allowed {
            return refused(format!("{relay} may not renew for {peer}"));
        }
        let attached = relays.last().copied();
        self.issue_renewal(&presented, csr_pem, attached, ask).await
    }

    /// The checks of ADR-0010 and the issuance, for `presented` as seen by
    /// the server or, when `via_relay` is set, by that relay.
    async fn issue_renewal(
        &self,
        presented: &PresentedCert,
        csr_pem: &str,
        via_relay: Option<&str>,
        ask: Ask,
    ) -> Result<Renewed, RenewError> {
        let names_only = ask.names_only;
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
        let recorded: Option<Recorded> = sqlx::query_as(
            "SELECT peer_role, peer_id, revoked_at IS NOT NULL, not_after <= now(),
                    issuer_ca, trusted_cas
             FROM certificates WHERE serial = $1",
        )
        .bind(presented.serial.as_bytes())
        .fetch_optional(&mut *tx)
        .await?;
        let (issuer, trusted) = match recorded {
            None => return refused("the presented certificate was not issued by this server"),
            Some((r, i, ..)) if r != role.as_str() || i != id => {
                return refused("the presented certificate is recorded for another peer")
            }
            Some((_, _, true, ..)) => return refused("the presented certificate is revoked"),
            Some((_, _, _, true, ..)) => return refused("the presented certificate has expired"),
            Some((_, _, _, _, issuer, trusted)) => (issuer, trusted),
        };
        // Rows from before ADR-0088 get their issuer at start; until then
        // they come from the first CA, the oldest one still trusted.
        let trust = self.ca.trust();
        let oldest = trust.bundle.last().copied().unwrap_or(trust.issuer);
        let issuer = issuer.as_deref().and_then(from_hex).unwrap_or(oldest);
        let trusted: Vec<_> = match trusted {
            Some(trusted) => trusted.iter().filter_map(|fp| from_hex(fp)).collect(),
            None => vec![issuer],
        };
        let behind = ask.accepts_ca_bundle && trust.is_behind(&issuer, &trusted);
        if ask.rotation_only && !behind {
            return Err(RenewError::NoRotation);
        }
        check_inventory(&mut tx, peer).await?;
        let names = confirmed_names(&mut tx, peer)
            .await?
            .unwrap_or_else(|| presented.server_names.clone());
        let new_names = !same_names(&names, &presented.server_names);
        if names_only && !new_names {
            return Err(RenewError::NoNewNames);
        }
        let too_soon: Option<bool> = sqlx::query_scalar(
            "SELECT max(issued_at) > now() - make_interval(secs => $3)
             FROM certificates WHERE peer_role = $1 AND peer_id = $2",
        )
        .bind(role.as_str())
        .bind(id)
        .bind(self.min_interval.as_secs_f64())
        .fetch_one(&mut *tx)
        .await?;
        // A change of certificate authority is not limited: the server
        // decides when it happens, and each step needs one certificate.
        if too_soon == Some(true) && !new_names && !behind {
            return Err(RenewError::TooSoon(self.min_interval));
        }

        let (cert, trust) = issue(&self.ca, csr_pem, peer, names).await?;
        let ca_bundle_signatures = sign_bundle(&self.ca, &cert, &trust)?;
        record_certificate(&mut tx, &cert, peer, &trust, ask.accepts_ca_bundle).await?;
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
                    "issuer_ca": hex(&cert.issuer),
                    "ca_change": behind,
                }),
            },
        )
        .await?;
        tx.commit().await?;
        tracing::info!(%peer, serial = %cert.serial, previous = %presented.serial, ?via_relay, ca_change = behind, "renewed a certificate");
        Ok(Renewed {
            identity: peer.clone(),
            cert,
            trust,
            ca_bundle_signatures,
        })
    }
}

/// `peer_role`, `peer_id`, revoked, expired, `issuer_ca`, `trusted_cas`.
type Recorded = (
    String,
    String,
    bool,
    bool,
    Option<String>,
    Option<Vec<String>>,
);

/// Issues a certificate for `peer` (in `spawn_blocking`) with the CAs it
/// goes with. A change of certificate authority in between fails the call,
/// which the peer retries.
pub(crate) async fn issue(
    ca: &Arc<CertificateAuthority>,
    csr_pem: &str,
    peer: &PeerIdentity,
    names: Vec<ServerName>,
) -> Result<(IssuedCert, Trust), RenewError> {
    let (ca, csr, identity) = (ca.clone(), csr_pem.to_owned(), peer.clone());
    tokio::task::spawn_blocking(move || {
        let cert = ca.issue(&csr, &identity, None, &names)?;
        Ok((cert, ca.trust()))
    })
    .await
    .map_err(|e| RenewError::Internal(e.to_string()))?
    .map_err(|e: crate::ca::CaError| RenewError::Internal(e.to_string()))
    .and_then(|(cert, trust)| match cert.issuer == trust.issuer {
        true => Ok((cert, trust)),
        false => Err(RenewError::Internal(
            "the certificate authority changed while issuing".into(),
        )),
    })
}

/// The signatures that let the peer of `cert` take the CAs of `trust`
/// (ADR-0107). Two ECDSA signatures at most: not worth a blocking task.
fn sign_bundle(
    ca: &CertificateAuthority,
    cert: &IssuedCert,
    trust: &Trust,
) -> Result<Vec<Vec<u8>>, RenewError> {
    let leaf = monitorflow_common::ca_bundle::leaf_fingerprint(&cert.cert_pem)
        .ok_or_else(|| RenewError::Internal("the issued certificate does not parse".into()))?;
    ca.sign_ca_bundle(&leaf, trust)
        .map_err(|e| RenewError::Internal(e.to_string()))
}

/// Records `cert` in `certificates`, with its issuer and, for a peer that
/// stores CA bundles, the CAs it trusts with it.
pub(crate) async fn record_certificate(
    tx: &mut Transaction<'_, Postgres>,
    cert: &IssuedCert,
    peer: &PeerIdentity,
    trust: &Trust,
    accepts_ca_bundle: bool,
) -> Result<(), sqlx::Error> {
    let trusted: Option<Vec<String>> =
        accepts_ca_bundle.then(|| trust.bundle.iter().map(hex).collect());
    sqlx::query(
        "INSERT INTO certificates
             (serial, peer_role, peer_id, not_before, not_after, issuer_ca, trusted_cas)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(cert.serial.as_bytes())
    .bind(peer.role().as_str())
    .bind(peer.id())
    .bind(DateTime::<Utc>::from(cert.not_before))
    .bind(DateTime::<Utc>::from(cert.not_after))
    .bind(hex(&cert.issuer))
    .bind(trusted)
    .execute(&mut **tx)
    .await
    .map(drop)
}

/// The names an administrator confirmed for a relay (ADR-0066); `None` for
/// other peers and for relays that have their names from a token.
async fn confirmed_names(
    tx: &mut Transaction<'_, Postgres>,
    peer: &PeerIdentity,
) -> Result<Option<Vec<ServerName>>, RenewError> {
    if peer.role() != PeerRole::Relay {
        return Ok(None);
    }
    let names: Option<Option<Vec<String>>> =
        sqlx::query_scalar("SELECT confirmed_names FROM relays WHERE peer_id = $1")
            .bind(peer.id())
            .fetch_optional(&mut **tx)
            .await?;
    Ok(names.flatten().map(|names| parse_names(&names)))
}

/// Whether two lists hold the same names, in any order.
fn same_names(a: &[ServerName], b: &[ServerName]) -> bool {
    a.iter().all(|name| b.contains(name)) && b.iter().all(|name| a.contains(name))
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
        let request = request.into_inner();
        let ask = Ask {
            names_only: request.names_only,
            accepts_ca_bundle: request.accepts_ca_bundle,
            rotation_only: request.rotation_only,
        };
        let result = self.renew_for(&presented, &request.csr_pem, ask).await;
        self.respond(&peer, ask, result)
    }

    async fn renew_forwarded(
        &self,
        request: Request<ForwardedRenewRequest>,
    ) -> Result<Response<RenewResponse>, Status> {
        let relay = monitorflow_common::tls::peer_identity(&request)?;
        let via = Via::from_metadata(request.metadata())?;
        let request = request.into_inner();
        let cert = CertificateDer::from(request.peer_cert_der);
        let ask = Ask {
            names_only: request.names_only,
            accepts_ca_bundle: request.accepts_ca_bundle,
            rotation_only: request.rotation_only,
        };
        let result = self
            .forwarded(&relay, via.as_ref(), &cert, &request.csr_pem, ask)
            .await;
        self.respond(&relay, ask, result)
    }
}

impl Renewer {
    /// The response for `result`, logged with the calling `peer`.
    fn respond(
        &self,
        peer: &PeerIdentity,
        ask: Ask,
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
                    RenewError::NoRotation => tracing::trace!(%peer, %error, "renewal rejected"),
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
            ca_cert_pem: renewed.trust.issuer_pem,
            peer_uri: renewed.identity.uri(),
            not_after_unix,
            ca_bundle_pem: match ask.accepts_ca_bundle {
                true => renewed.trust.bundle_pem,
                false => String::new(),
            },
            ca_bundle_signatures: match ask.accepts_ca_bundle {
                true => renewed.ca_bundle_signatures,
                false => Vec::new(),
            },
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

    #[test]
    fn fingerprints_round_trip_through_hex() {
        for fingerprint in [[0u8; 32], [0xab; 32], {
            let mut f = [0u8; 32];
            f[31] = 1;
            f
        }] {
            assert_eq!(from_hex(&hex(&fingerprint)), Some(fingerprint));
        }
        assert_eq!(from_hex("ab"), None);
        assert_eq!(from_hex(&"g".repeat(64)), None);
    }
}
