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
use super::{ca_fingerprint, install_key, server_cert, token};
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
    /// An install key needs the host name an agent sends (ADR-0058).
    #[error("no host name with an install key")]
    NoHostname,
    /// The install key's per-minute allowance is spent; not a failure of
    /// the caller.
    #[error("install key busy")]
    Busy,
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
            EnrollError::NoHostname => Status::invalid_argument(
                "install keys enroll agents that send their host name: use a single-use token for relays and older agents",
            ),
            EnrollError::Busy => {
                Status::resource_exhausted("too many enrollments with this install key; retry later")
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

    /// [`Self::enroll_host`] without a host name: single-use tokens only.
    pub async fn enroll(
        &self,
        token: &str,
        csr_pem: &str,
        source: Option<IpAddr>,
    ) -> Result<Enrolled, EnrollError> {
        self.enroll_host(token, csr_pem, "", source).await
    }

    /// Consumes `token` and signs the key of `csr_pem` for the identity the
    /// token names. A single-use token is tried first; otherwise `token`
    /// may be an install key, which enrolls an agent under an ID made from
    /// `hostname` (ADR-0058). Token use, certificate record and audit row
    /// commit together; on any error the token stays usable. `source` goes
    /// to the audit log.
    pub async fn enroll_host(
        &self,
        token: &str,
        csr_pem: &str,
        hostname: &str,
        source: Option<IpAddr>,
    ) -> Result<Enrolled, EnrollError> {
        // Checks that need no database come first, so their answers say
        // nothing about the token's state.
        let token = EnrollmentToken::parse(token).map_err(|_| EnrollError::Refused)?;
        if token.ca_fingerprint() != &self.ca_fingerprint {
            return Err(EnrollError::Refused);
        }
        CertificateSigningRequestParams::from_pem(csr_pem).map_err(|_| EnrollError::InvalidCsr)?;

        let hash = token::hash(&token);
        let mut tx = self.pool.begin().await?;
        // Concurrent uses wait on the row lock, then see used_at set.
        let row: Option<TokenRow> = sqlx::query_as(
            "UPDATE enrollment_tokens SET used_at = now(), used_by_peer_id = peer_id
             WHERE token_hash = $1 AND used_at IS NULL AND expires_at > now()
             RETURNING id, role, peer_id, site_id, server_names, parent_relay_id",
        )
        .bind(hash.as_slice())
        .fetch_optional(&mut *tx)
        .await?;
        let (token_id, role, peer_id, site_id, server_names, parent, key_id) = match row {
            Some((token_id, role, peer_id, site_id, names, parent)) => {
                (token_id, role, peer_id, site_id, names, parent, None)
            }
            None => match install_key::consume(&mut tx, &hash, hostname, source).await? {
                Some(used) => (
                    used.token_id,
                    "agent".to_owned(),
                    used.peer_id,
                    used.site_id,
                    Vec::new(),
                    None,
                    Some(used.key_id),
                ),
                None => return Err(EnrollError::Refused),
            },
        };
        let role = match role.as_str() {
            "agent" => PeerRole::Agent,
            "relay" => PeerRole::Relay,
            other => return Err(EnrollError::Internal(format!("token role `{other}`"))),
        };
        let identity =
            PeerIdentity::new(role, &peer_id).map_err(|e| EnrollError::Internal(e.to_string()))?;
        let new_relay = prepare_inventory(&mut tx, role, &peer_id, site_id, parent).await?;

        // Names come from the token, like the identity (ADR-0025).
        let names = server_cert::parse_names(&server_names);
        let (ca, csr, id) = (self.ca.clone(), csr_pem.to_owned(), identity.clone());
        let cert = tokio::task::spawn_blocking(move || ca.issue(&csr, &id, None, &names))
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
            "install_key_id": key_id,
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
                    details: serde_json::json!({
                        "relay_id": relay_id,
                        "site_id": site_id,
                        "parent_relay_id": parent,
                    }),
                },
            )
            .await?;
        }
        tx.commit().await?;
        tracing::info!(peer = %identity, serial = %cert.serial, ?source, "enrolled a peer");
        Ok(Enrolled { identity, cert })
    }
}

/// A consumed token: ID, role, peer ID, site, server names, parent relay.
type TokenRow = (Uuid, String, String, Uuid, Vec<String>, Option<Uuid>);

/// Checks the peer's inventory row before its certificate is issued
/// (ADR-0008): a revoked row, or a row in another site than the token's or
/// under another parent relay (ADR-0028), refuses the enrollment. A relay
/// without a row gets one in the token's site, under the token's parent;
/// its ID is returned. Agents get theirs at their first Hello.
async fn prepare_inventory(
    tx: &mut Transaction<'_, Postgres>,
    role: PeerRole,
    peer_id: &str,
    site_id: Uuid,
    parent: Option<Uuid>,
) -> Result<Option<Uuid>, EnrollError> {
    let sql = match role {
        PeerRole::Relay => {
            "SELECT site_id, revoked_at IS NOT NULL, parent_relay_id FROM relays
             WHERE peer_id = $1 FOR UPDATE"
        }
        _ => {
            "SELECT site_id, revoked_at IS NOT NULL, NULL::uuid FROM servers
             WHERE peer_id = $1 FOR UPDATE"
        }
    };
    let existing: Option<(Uuid, bool, Option<Uuid>)> = sqlx::query_as(sql)
        .bind(peer_id)
        .fetch_optional(&mut **tx)
        .await?;
    match existing {
        Some((existing_site, revoked, existing_parent))
            if revoked || existing_site != site_id || existing_parent != parent =>
        {
            tracing::warn!(
                %role,
                peer_id,
                revoked,
                %existing_site,
                token_site = %site_id,
                ?existing_parent,
                token_parent = ?parent,
                "enrollment refused: the peer's inventory row is revoked, in another site or under another parent relay"
            );
            Err(EnrollError::Refused)
        }
        None if role == PeerRole::Relay => {
            let relay_id = sqlx::query_scalar(
                "INSERT INTO relays (peer_id, site_id, name, parent_relay_id)
                 VALUES ($1, $2, $1, $3) RETURNING id",
            )
            .bind(peer_id)
            .bind(site_id)
            .bind(parent)
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
        let enrolled = match self
            .enroll_host(&request.token, &request.csr_pem, &request.hostname, source)
            .await
        {
            Ok(enrolled) => enrolled,
            Err(error) => {
                match &error {
                    EnrollError::Internal(why) => {
                        tracing::error!(?source, error = %why, "enrollment failed")
                    }
                    // Holders of a valid key: not an attempt to count.
                    EnrollError::Busy | EnrollError::NoHostname => {
                        tracing::info!(?source, %error, "enrollment postponed or refused")
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
