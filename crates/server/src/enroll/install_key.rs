//! Reusable install keys (ADR-0058): one key per site enrolls many agents,
//! each under its own host name, until it expires, reaches its host limit
//! or is revoked. A key has the token format of ADR-0007, so installers
//! take it where they take a token; only the SHA-256 of its secret is
//! stored, in `install_keys`.

use std::net::IpAddr;
use std::time::Duration;

use chrono::{DateTime, Utc};
use monitorflow_common::tls::{EnrollmentToken, Fingerprint};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::token::{self, Creator, TokenError};
use super::EnrollError;

pub const DEFAULT_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
pub const MIN_TTL: Duration = Duration::from_secs(60 * 60);
pub const MAX_TTL: Duration = Duration::from_secs(365 * 24 * 60 * 60);
/// Largest host limit; no limit is `None`.
pub const MAX_HOSTS: u32 = 100_000;
/// Enrollments one key accepts per minute; more wait with
/// `RESOURCE_EXHAUSTED`, which agents retry.
pub const MAX_PER_MINUTE: i64 = 30;

/// Longest peer ID (ADR-0004).
const MAX_ID_LEN: usize = 64;
/// Suffixes tried after the host name itself: `web-01-2` to `web-01-1000`.
const MAX_SUFFIX: u32 = 1000;
/// Serialises the choice of new agent IDs between concurrent enrollments.
const ID_LOCK: i64 = 0x6d66_6964; // "mfid"

/// What a new key allows.
#[derive(Debug, Clone)]
pub struct KeyRequest {
    pub site_id: Uuid,
    pub ttl: Duration,
    /// Hosts the key may enroll; `None` for no limit.
    pub max_hosts: Option<u32>,
    /// New hosts wait for an administrator before they appear.
    pub require_approval: bool,
    pub creator: Creator,
}

impl KeyRequest {
    fn validate(&self) -> Result<(), TokenError> {
        if !(MIN_TTL..=MAX_TTL).contains(&self.ttl) {
            return Err(TokenError::Invalid(format!(
                "validity must be between {} hour and {} days",
                MIN_TTL.as_secs() / 3600,
                MAX_TTL.as_secs() / 86_400
            )));
        }
        if self
            .max_hosts
            .is_some_and(|n| !(1..=MAX_HOSTS).contains(&n))
        {
            return Err(TokenError::Invalid(format!(
                "the host limit must be between 1 and {MAX_HOSTS}"
            )));
        }
        Ok(())
    }
}

/// A stored key. The string in `key` is shown once and never again.
#[derive(Debug)]
pub struct CreatedKey {
    pub id: Uuid,
    pub key: EnrollmentToken,
    pub expires_at: DateTime<Utc>,
}

/// Stores a new key for `request`, with an `audit_log` row, in one
/// transaction.
pub async fn create(
    pool: &PgPool,
    ca_fingerprint: Fingerprint,
    request: &KeyRequest,
) -> Result<CreatedKey, TokenError> {
    request.validate()?;
    let key = token::generate(ca_fingerprint)?;
    let mut tx = pool.begin().await?;
    let (id, expires_at): (Uuid, DateTime<Utc>) = sqlx::query_as(
        "INSERT INTO install_keys (key_hash, site_id, created_by, expires_at, max_hosts,
                                   require_approval)
         VALUES ($1, $2, $3, now() + make_interval(secs => $4), $5, $6)
         RETURNING id, expires_at",
    )
    .bind(token::hash(&key).as_slice())
    .bind(request.site_id)
    .bind(request.creator.user_id())
    .bind(request.ttl.as_secs_f64())
    .bind(request.max_hosts.map(|n| n as i32))
    .bind(request.require_approval)
    .fetch_one(&mut *tx)
    .await?;
    let (actor_kind, actor_id) = request.creator.actor();
    crate::api::audit::record(
        &mut *tx,
        crate::api::audit::Event {
            actor_kind,
            actor_id: &actor_id,
            action: "install_key.create",
            target: Some(("install_key", &id.to_string())),
            details: serde_json::json!({
                "site_id": request.site_id,
                "expires_at": expires_at,
                "max_hosts": request.max_hosts,
                "require_approval": request.require_approval,
            }),
        },
    )
    .await?;
    tx.commit().await?;
    tracing::info!(key_id = %id, site_id = %request.site_id, %expires_at, "created an install key");
    Ok(CreatedKey {
        id,
        key,
        expires_at,
    })
}

/// An enrollment through a key, recorded as a used token.
#[derive(Debug)]
pub(super) struct Consumed {
    pub key_id: Uuid,
    pub token_id: Uuid,
    pub peer_id: String,
    pub site_id: Uuid,
}

/// Uses the key with hash `key_hash` for the agent `hostname`, in `tx`:
/// takes back the ID of the same host if it was removed from the key's
/// site (ADR-0062), otherwise picks a new unique ID from the host name;
/// records the enrollment as a used token of the key's site, counts it,
/// and puts the host in `pending_hosts` when the key asks for approval.
/// `None` when no usable key has that hash.
pub(super) async fn consume(
    tx: &mut Transaction<'_, Postgres>,
    key_hash: &[u8; 32],
    hostname: &str,
    source: Option<IpAddr>,
) -> Result<Option<Consumed>, EnrollError> {
    // Concurrent uses wait on the row lock, then see the new host_count.
    let key: Option<(Uuid, Uuid, Option<Uuid>, bool)> = sqlx::query_as(
        "SELECT id, site_id, created_by, require_approval FROM install_keys
         WHERE key_hash = $1 AND revoked_at IS NULL AND expires_at > now()
           AND (max_hosts IS NULL OR host_count < max_hosts)
         FOR UPDATE",
    )
    .bind(key_hash.as_slice())
    .fetch_optional(&mut **tx)
    .await?;
    let Some((key_id, site_id, created_by, require_approval)) = key else {
        return Ok(None);
    };
    if hostname.trim().is_empty() {
        return Err(EnrollError::NoHostname);
    }
    let recent: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM enrollment_tokens
         WHERE install_key_id = $1 AND used_at > now() - interval '1 minute'",
    )
    .bind(key_id)
    .fetch_one(&mut **tx)
    .await?;
    if recent >= MAX_PER_MINUTE {
        return Err(EnrollError::Busy);
    }

    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(ID_LOCK)
        .execute(&mut **tx)
        .await?;
    let base = peer_id_from_hostname(hostname);
    let peer_id = match removed_id(tx, site_id, hostname, &base).await? {
        Some(id) => id,
        None => new_id(tx, &base).await?,
    };

    // A hash of a secret nobody holds: the row exists only as a record.
    let unusable = token::generate([0; 32]).map_err(|e| EnrollError::Internal(e.to_string()))?;
    let token_id: Uuid = sqlx::query_scalar(
        "INSERT INTO enrollment_tokens (token_hash, role, peer_id, site_id, created_by,
                                        expires_at, used_at, used_by_peer_id, install_key_id)
         VALUES ($1, 'agent', $2, $3, $4, now() + interval '1 second', now(), $2, $5)
         RETURNING id",
    )
    .bind(token::hash(&unusable).as_slice())
    .bind(&peer_id)
    .bind(site_id)
    .bind(created_by)
    .bind(key_id)
    .fetch_one(&mut **tx)
    .await?;
    sqlx::query("UPDATE install_keys SET host_count = host_count + 1 WHERE id = $1")
        .bind(key_id)
        .execute(&mut **tx)
        .await?;
    if require_approval {
        sqlx::query(
            "INSERT INTO pending_hosts (peer_id, site_id, install_key_id, hostname, source)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(&peer_id)
        .bind(site_id)
        .bind(key_id)
        .bind(hostname.trim())
        .bind(source.map(|ip| ip.to_string()))
        .execute(&mut **tx)
        .await?;
    }
    Ok(Some(Consumed {
        key_id,
        token_id,
        peer_id,
        site_id,
    }))
}

/// The ID of the server last removed from `site_id` with this host name
/// or ID: enrolling the host again brings that row back instead of adding
/// a second one. Not a host already waiting for approval.
async fn removed_id(
    tx: &mut Transaction<'_, Postgres>,
    site_id: Uuid,
    hostname: &str,
    base: &str,
) -> Result<Option<String>, EnrollError> {
    Ok(sqlx::query_scalar(
        "SELECT s.peer_id FROM servers s
         WHERE s.site_id = $1 AND s.revoked_at IS NOT NULL
           AND (lower(s.hostname) = lower($2) OR lower(s.peer_id) = $3)
           AND NOT EXISTS (SELECT 1 FROM pending_hosts p WHERE p.peer_id = s.peer_id)
         ORDER BY s.revoked_at DESC
         LIMIT 1",
    )
    .bind(site_id)
    .bind(hostname.trim())
    .bind(base)
    .fetch_optional(&mut **tx)
    .await?)
}

/// `base`, or `base` with the first free suffix.
async fn new_id(tx: &mut Transaction<'_, Postgres>, base: &str) -> Result<String, EnrollError> {
    for n in 1..=MAX_SUFFIX {
        let candidate = with_suffix(base, n);
        // Any trace of the ID counts, revoked or pending, in any case:
        // a key never gives a new host an ID that was ever given out.
        let taken: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM servers WHERE lower(peer_id) = $1)
                 OR EXISTS (SELECT 1 FROM certificates
                            WHERE peer_role = 'agent' AND lower(peer_id) = $1)
                 OR EXISTS (SELECT 1 FROM enrollment_tokens
                            WHERE role = 'agent' AND lower(peer_id) = $1)",
        )
        .bind(&candidate)
        .fetch_one(&mut **tx)
        .await?;
        if !taken {
            return Ok(candidate);
        }
    }
    Err(EnrollError::Internal(format!(
        "no free ID left for host `{base}`"
    )))
}

/// A peer ID from a host name: lowercase, every run of other characters
/// replaced by `-`, at most 64 characters; `host` if nothing is left.
pub fn peer_id_from_hostname(hostname: &str) -> String {
    let mut id = String::new();
    for c in hostname.trim().chars().map(|c| c.to_ascii_lowercase()) {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            id.push(c);
        } else if !id.ends_with('-') {
            id.push('-');
        }
    }
    let id = trim(&id[..id.len().min(MAX_ID_LEN)]);
    if id.is_empty() {
        "host".into()
    } else {
        id.into()
    }
}

/// `base` for 1, else `base-n`, shortened to fit the ID length.
fn with_suffix(base: &str, n: u32) -> String {
    if n == 1 {
        return base.into();
    }
    let suffix = format!("-{n}");
    let base = trim(&base[..base.len().min(MAX_ID_LEN - suffix.len())]);
    format!("{base}{suffix}")
}

fn trim(id: &str) -> &str {
    id.trim_matches(|c| matches!(c, '-' | '.'))
}

#[cfg(test)]
mod tests {
    use monitorflow_common::tls::validate_id;

    use super::*;

    #[test]
    fn host_names_become_valid_ids() {
        for (hostname, id) in [
            ("web-01", "web-01"),
            ("WEB-01.Example.COM\n", "web-01.example.com"),
            ("DESKTOP 7 (old)", "desktop-7-old"),
            ("db_primary", "db_primary"),
            ("élise", "lise"),
            ("..--", "host"),
            ("", "host"),
        ] {
            assert_eq!(peer_id_from_hostname(hostname), id, "{hostname:?}");
            validate_id(id).unwrap();
        }
        let long = peer_id_from_hostname(&"a".repeat(100));
        assert_eq!(long.len(), MAX_ID_LEN);
        validate_id(&long).unwrap();
    }

    #[test]
    fn suffixes_fit_the_id_length() {
        assert_eq!(with_suffix("web-01", 1), "web-01");
        assert_eq!(with_suffix("web-01", 2), "web-01-2");
        let long = with_suffix(&"a".repeat(MAX_ID_LEN), MAX_SUFFIX);
        assert_eq!(long.len(), MAX_ID_LEN);
        assert!(long.ends_with("-1000"));
        validate_id(&long).unwrap();
        // A cut never leaves a separator before the suffix.
        let base = format!("{}-b", "a".repeat(MAX_ID_LEN - 3));
        assert_eq!(
            with_suffix(&base, 2),
            format!("{}-2", "a".repeat(MAX_ID_LEN - 3))
        );
    }

    #[test]
    fn requests_are_validated() {
        let request = |ttl, max_hosts| KeyRequest {
            site_id: Uuid::nil(),
            ttl,
            max_hosts,
            require_approval: false,
            creator: Creator::Cli,
        };
        assert!(request(DEFAULT_TTL, None).validate().is_ok());
        assert!(request(MIN_TTL, Some(1)).validate().is_ok());
        assert!(request(MAX_TTL, Some(MAX_HOSTS)).validate().is_ok());
        for bad in [
            request(MIN_TTL - Duration::from_secs(1), None),
            request(MAX_TTL + Duration::from_secs(1), None),
            request(DEFAULT_TTL, Some(0)),
            request(DEFAULT_TTL, Some(MAX_HOSTS + 1)),
        ] {
            assert!(
                matches!(bad.validate(), Err(TokenError::Invalid(_))),
                "{bad:?}"
            );
        }
    }
}
