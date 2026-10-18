//! `EnrollService::Enroll` and its listener (ADR-0007).

use std::future::Future;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use monitorflow_common::proto::v1::enroll_service_server::{EnrollService, EnrollServiceServer};
use monitorflow_common::proto::v1::{EnrollRequest, EnrollResponse};
use monitorflow_common::tls::{
    self, AcceptOptions, EnrollmentToken, Fingerprint, PeerIdentity, PeerRole,
};
use rcgen::CertificateSigningRequestParams;
use rustls::ServerConfig as TlsServerConfig;
use sqlx::{PgPool, Postgres, Transaction};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tonic::transport::Server;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use super::limiter::{FailureLimiter, UNKNOWN_SOURCE};
use super::{ca_fingerprint, token};
use crate::api::audit::{self, Event};
use crate::ca::{CertificateAuthority, IssuedCert};

/// Largest request accepted: a token and a CSR fit in a few KiB.
const MAX_REQUEST_BYTES: usize = 16 * 1024;
const MAX_HANDSHAKES: usize = 64;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CONCURRENT_PER_CONNECTION: usize = 4;

/// Why an enrollment failed. Callers only ever see the generic statuses of
/// [`Status::from`], so they cannot tell an unknown token from an expired
/// or used one.
#[derive(Debug, thiserror::Error)]
pub enum EnrollError {
    #[error("enrollment refused")]
    Refused,
    #[error("invalid CSR")]
    InvalidCsr,
    #[error("enrollment failed: {0}")]
    Internal(String),
}

impl From<sqlx::Error> for EnrollError {
    fn from(error: sqlx::Error) -> Self {
        Self::Internal(format!("database: {error}"))
    }
}

impl From<EnrollError> for Status {
    fn from(error: EnrollError) -> Self {
        match error {
            EnrollError::Refused => Status::permission_denied("enrollment refused"),
            EnrollError::InvalidCsr => {
                Status::invalid_argument("invalid CSR: expected a signed PKCS#10 request in PEM")
            }
            EnrollError::Internal(_) => Status::internal("enrollment failed"),
        }
    }
}

/// What a successful enrollment returns.
#[derive(Debug)]
pub struct Enrolled {
    pub identity: PeerIdentity,
    pub cert: IssuedCert,
}

/// Exchanges tokens for certificates. Cheap to clone.
#[derive(Clone)]
pub struct Enroller {
    pool: PgPool,
    ca: Arc<CertificateAuthority>,
    ca_fingerprint: Fingerprint,
    limiter: Arc<FailureLimiter>,
}

impl Enroller {
    pub fn new(pool: PgPool, ca: Arc<CertificateAuthority>, limiter: FailureLimiter) -> Self {
        let ca_fingerprint = ca_fingerprint(&ca);
        Self {
            pool,
            ca,
            ca_fingerprint,
            limiter: Arc::new(limiter),
        }
    }

    /// Consumes `token` and signs the key of `csr_pem` for the identity the
    /// token names. Token use, certificate record and audit row commit
    /// together; on any error the token stays usable. `source` goes to the
    /// audit log.
    pub async fn enroll(
        &self,
        token: &str,
        csr_pem: &str,
        source: Option<IpAddr>,
    ) -> Result<Enrolled, EnrollError> {
        // Checks that need no database come first, so their answers say
        // nothing about the token's state.
        let token = EnrollmentToken::parse(token).map_err(|_| EnrollError::Refused)?;
        if token.ca_fingerprint() != &self.ca_fingerprint {
            return Err(EnrollError::Refused);
        }
        CertificateSigningRequestParams::from_pem(csr_pem).map_err(|_| EnrollError::InvalidCsr)?;

        let mut tx = self.pool.begin().await?;
        // Concurrent uses wait on the row lock, then see used_at set.
        let row: Option<(Uuid, String, String, Uuid)> = sqlx::query_as(
            "UPDATE enrollment_tokens SET used_at = now(), used_by_peer_id = peer_id
             WHERE token_hash = $1 AND used_at IS NULL AND expires_at > now()
             RETURNING id, role, peer_id, site_id",
        )
        .bind(token::hash(&token).as_slice())
        .fetch_optional(&mut *tx)
        .await?;
        let Some((token_id, role, peer_id, site_id)) = row else {
            return Err(EnrollError::Refused);
        };
        let role = match role.as_str() {
            "agent" => PeerRole::Agent,
            "relay" => PeerRole::Relay,
            other => return Err(EnrollError::Internal(format!("token role `{other}`"))),
        };
        let identity =
            PeerIdentity::new(role, &peer_id).map_err(|e| EnrollError::Internal(e.to_string()))?;
        let new_relay = prepare_inventory(&mut tx, role, &peer_id, site_id).await?;

        let (ca, csr, id) = (self.ca.clone(), csr_pem.to_owned(), identity.clone());
        let cert = tokio::task::spawn_blocking(move || ca.issue(&csr, &id, None, &[]))
            .await
            .map_err(|e| EnrollError::Internal(e.to_string()))?
            .map_err(|e| EnrollError::Internal(e.to_string()))?;

        sqlx::query(
            "INSERT INTO certificates (serial, peer_role, peer_id, not_before, not_after)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(cert.serial.as_bytes())
        .bind(role.as_str())
        .bind(&peer_id)
        .bind(DateTime::<Utc>::from(cert.not_before))
        .bind(DateTime::<Utc>::from(cert.not_after))
        .execute(&mut *tx)
        .await?;
        let details = serde_json::json!({
            "token_id": token_id,
            "site_id": site_id,
            "serial": cert.serial.to_string(),
            "source": source.map(|ip| ip.to_string()),
        });
        sqlx::query(
            "INSERT INTO audit_log (actor_kind, actor_id, action, target_kind, target_id, details)
             VALUES ('peer', $1, 'peer.enroll', $2, $3, $4::jsonb)",
        )
        .bind(identity.uri())
        .bind(role.as_str())
        .bind(&peer_id)
        .bind(details.to_string())
        .execute(&mut *tx)
        .await?;
        if let Some(relay_id) = new_relay {
            let actor = identity.uri();
            audit::record(
                &mut *tx,
                Event {
                    actor_kind: "peer",
                    actor_id: &actor,
                    action: "relay.create",
                    target: Some(("relay", &peer_id)),
                    details: serde_json::json!({ "relay_id": relay_id, "site_id": site_id }),
                },
            )
            .await?;
        }
        tx.commit().await?;
        tracing::info!(peer = %identity, serial = %cert.serial, ?source, "enrolled a peer");
        Ok(Enrolled { identity, cert })
    }
}

/// Checks the peer's inventory row before its certificate is issued
/// (ADR-0008): a revoked row, or a row in another site than the token's,
/// refuses the enrollment. A relay without a row gets one in the token's
/// site; its ID is returned. Agents get theirs at their first Hello.
async fn prepare_inventory(
    tx: &mut Transaction<'_, Postgres>,
    role: PeerRole,
    peer_id: &str,
    site_id: Uuid,
) -> Result<Option<Uuid>, EnrollError> {
    let sql = match role {
        PeerRole::Relay => {
            "SELECT site_id, revoked_at IS NOT NULL FROM relays WHERE peer_id = $1 FOR UPDATE"
        }
        _ => "SELECT site_id, revoked_at IS NOT NULL FROM servers WHERE peer_id = $1 FOR UPDATE",
    };
    let existing: Option<(Uuid, bool)> = sqlx::query_as(sql)
        .bind(peer_id)
        .fetch_optional(&mut **tx)
        .await?;
    match existing {
        Some((existing_site, revoked)) if revoked || existing_site != site_id => {
            tracing::warn!(
                %role,
                peer_id,
                revoked,
                %existing_site,
                token_site = %site_id,
                "enrollment refused: the peer's inventory row is revoked or in another site"
            );
            Err(EnrollError::Refused)
        }
        None if role == PeerRole::Relay => {
            let relay_id = sqlx::query_scalar(
                "INSERT INTO relays (peer_id, site_id, name) VALUES ($1, $2, $1) RETURNING id",
            )
            .bind(peer_id)
            .bind(site_id)
            .fetch_one(&mut **tx)
            .await?;
            Ok(Some(relay_id))
        }
        _ => Ok(None),
    }
}

#[tonic::async_trait]
impl EnrollService for Enroller {
    async fn enroll(
        &self,
        request: Request<EnrollRequest>,
    ) -> Result<Response<EnrollResponse>, Status> {
        let source = request.remote_addr().map(|addr| addr.ip());
        let key = source.unwrap_or(UNKNOWN_SOURCE);
        if self.limiter.is_blocked(key) {
            return Err(Status::resource_exhausted(
                "too many failed enrollment attempts; retry later",
            ));
        }
        let request = request.into_inner();
        let enrolled = match Enroller::enroll(self, &request.token, &request.csr_pem, source).await
        {
            Ok(enrolled) => enrolled,
            Err(error) => {
                match &error {
                    EnrollError::Internal(why) => {
                        tracing::error!(?source, error = %why, "enrollment failed")
                    }
                    // Routine and cheap to trigger: keep them at debug.
                    _ => {
                        self.limiter.record_failure(key);
                        tracing::debug!(?source, %error, "enrollment rejected");
                    }
                }
                return Err(error.into());
            }
        };
        let not_after_unix = enrolled
            .cert
            .not_after
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        Ok(Response::new(EnrollResponse {
            cert_pem: enrolled.cert.cert_pem,
            ca_cert_pem: self.ca.ca_cert_pem().to_owned(),
            peer_uri: enrolled.identity.uri(),
            not_after_unix,
        }))
    }
}

/// Serves `EnrollService` on `listener` with `tls`, which must not ask for
/// client certificates (`Identity::server_config_without_client_auth`),
/// until `shutdown` resolves.
pub async fn serve(
    listener: TcpListener,
    tls: Arc<TlsServerConfig>,
    enroller: Enroller,
    shutdown: impl Future<Output = ()>,
) -> Result<(), tonic::transport::Error> {
    let (_fixed, tls) = watch::channel(tls);
    let options = AcceptOptions {
        max_handshakes: MAX_HANDSHAKES,
        ..AcceptOptions::default()
    };
    Server::builder()
        .timeout(REQUEST_TIMEOUT)
        .concurrency_limit_per_connection(MAX_CONCURRENT_PER_CONNECTION)
        .add_service(
            EnrollServiceServer::new(enroller).max_decoding_message_size(MAX_REQUEST_BYTES),
        )
        .serve_with_incoming_shutdown(tls::incoming(listener, tls, options), shutdown)
        .await
}
