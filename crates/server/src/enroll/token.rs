//! One-time enrollment tokens: generation, hashing and creation in
//! `enrollment_tokens` (ADR-0007). Only the SHA-256 of the secret is stored.

use std::time::Duration;

use chrono::{DateTime, Utc};
use monitorflow_common::tls::{validate_id, EnrollmentToken, Fingerprint, PeerRole, SECRET_LEN};
use ring::rand::{SecureRandom, SystemRandom};
use sqlx::PgPool;
use uuid::Uuid;

pub const DEFAULT_TTL: Duration = Duration::from_secs(60 * 60);
pub const MIN_TTL: Duration = Duration::from_secs(60);
pub const MAX_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error("{0}")]
    Invalid(String),
    #[error("system random generator failed")]
    Random,
    #[error("database query failed: {0}")]
    Db(#[from] sqlx::Error),
}

/// Who creates a token: `created_by` and the audit log actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Creator {
    User(Uuid),
    /// `monitorflow-server token create`, run by someone with shell access
    /// to the server.
    Cli,
}

impl Creator {
    fn user_id(self) -> Option<Uuid> {
        match self {
            Self::User(id) => Some(id),
            Self::Cli => None,
        }
    }

    /// `(actor_kind, actor_id)` of the audit log.
    fn actor(self) -> (&'static str, String) {
        match self {
            Self::User(id) => ("user", id.to_string()),
            Self::Cli => ("system", "cli".into()),
        }
    }
}

/// What a new token enrolls.
#[derive(Debug, Clone)]
pub struct TokenRequest {
    /// `Agent` or `Relay`.
    pub role: PeerRole,
    /// ID written in the certificate, see [`validate_id`].
    pub peer_id: String,
    pub site_id: Uuid,
    pub ttl: Duration,
    pub creator: Creator,
}

impl TokenRequest {
    fn validate(&self) -> Result<(), TokenError> {
        let invalid = |message: String| Err(TokenError::Invalid(message));
        if self.role == PeerRole::Server {
            return invalid("tokens enroll agents and relays, not servers".into());
        }
        validate_id(&self.peer_id).or_else(|e| invalid(format!("invalid peer ID: {e}")))?;
        if !(MIN_TTL..=MAX_TTL).contains(&self.ttl) {
            return invalid(format!(
                "TTL must be between {} and {} seconds",
                MIN_TTL.as_secs(),
                MAX_TTL.as_secs()
            ));
        }
        Ok(())
    }
}

/// A stored token. The string in `token` is shown once and never again.
#[derive(Debug)]
pub struct CreatedToken {
    pub id: Uuid,
    pub token: EnrollmentToken,
    pub expires_at: DateTime<Utc>,
}

/// A new token with a random secret, pinned to the CA `ca_fingerprint`.
pub fn generate(ca_fingerprint: Fingerprint) -> Result<EnrollmentToken, TokenError> {
    let mut secret = [0u8; SECRET_LEN];
    SystemRandom::new()
        .fill(&mut secret)
        .map_err(|_| TokenError::Random)?;
    Ok(EnrollmentToken::new(secret, ca_fingerprint))
}

/// The value stored in `enrollment_tokens.token_hash`: SHA-256 of the secret.
pub fn hash(token: &EnrollmentToken) -> [u8; 32] {
    let digest = ring::digest::digest(&ring::digest::SHA256, token.secret());
    digest
        .as_ref()
        .try_into()
        .expect("SHA-256 digests are 32 bytes")
}

/// Parses a TTL such as `90s`, `15m`, `1h` or `7d`.
pub fn parse_ttl(s: &str) -> Result<Duration, String> {
    let invalid = || format!("invalid TTL `{s}`: expected a number and s, m, h or d, like 1h");
    let unit = match s.chars().last().ok_or_else(invalid)? {
        's' => 1,
        'm' => 60,
        'h' => 60 * 60,
        'd' => 24 * 60 * 60,
        _ => return Err(invalid()),
    };
    let count: u64 = s[..s.len() - 1].parse().map_err(|_| invalid())?;
    count
        .checked_mul(unit)
        .map(Duration::from_secs)
        .ok_or_else(invalid)
}

/// Stores a new token for `request`, with an `audit_log` row, in one
/// transaction.
pub async fn create(
    pool: &PgPool,
    ca_fingerprint: Fingerprint,
    request: &TokenRequest,
) -> Result<CreatedToken, TokenError> {
    request.validate()?;
    let token = generate(ca_fingerprint)?;
    let mut tx = pool.begin().await?;
    let (id, expires_at): (Uuid, DateTime<Utc>) = sqlx::query_as(
        "INSERT INTO enrollment_tokens (token_hash, role, peer_id, site_id, created_by, expires_at)
         VALUES ($1, $2, $3, $4, $5, now() + make_interval(secs => $6))
         RETURNING id, expires_at",
    )
    .bind(hash(&token).as_slice())
    .bind(request.role.as_str())
    .bind(&request.peer_id)
    .bind(request.site_id)
    .bind(request.creator.user_id())
    .bind(request.ttl.as_secs_f64())
    .fetch_one(&mut *tx)
    .await?;
    let (actor_kind, actor_id) = request.creator.actor();
    let details = serde_json::json!({
        "token_id": id,
        "site_id": request.site_id,
        "expires_at": expires_at,
    });
    sqlx::query(
        "INSERT INTO audit_log (actor_kind, actor_id, action, target_kind, target_id, details)
         VALUES ($1, $2, 'enrollment_token.create', $3, $4, $5::jsonb)",
    )
    .bind(actor_kind)
    .bind(actor_id)
    .bind(request.role.as_str())
    .bind(&request.peer_id)
    .bind(details.to_string())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    tracing::info!(token_id = %id, role = %request.role, peer_id = %request.peer_id, %expires_at, "created an enrollment token");
    Ok(CreatedToken {
        id,
        token,
        expires_at,
    })
}

/// ID of the site `name`; creates it when missing if `create_missing`.
pub async fn site_id(
    pool: &PgPool,
    name: &str,
    create_missing: bool,
) -> Result<Option<Uuid>, sqlx::Error> {
    let existing = sqlx::query_scalar("SELECT id FROM sites WHERE name = $1")
        .bind(name)
        .fetch_optional(pool)
        .await?;
    if existing.is_some() || !create_missing {
        return Ok(existing);
    }
    // A concurrent creation of the same name is fine: keep whichever won.
    sqlx::query_scalar(
        "INSERT INTO sites (name) VALUES ($1)
         ON CONFLICT (name) DO UPDATE SET name = EXCLUDED.name
         RETURNING id",
    )
    .bind(name)
    .fetch_one(pool)
    .await
    .map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_are_random_and_pinned() {
        let a = generate([1; 32]).unwrap();
        let b = generate([1; 32]).unwrap();
        assert_ne!(a.secret(), b.secret());
        assert_eq!(a.ca_fingerprint(), &[1; 32]);
        // 256 bits from the system generator are never all zero in practice.
        assert_ne!(a.secret(), &[0; SECRET_LEN]);
    }

    #[test]
    fn hash_is_sha256_of_the_secret_only() {
        let token = EnrollmentToken::new([0; SECRET_LEN], [1; 32]);
        let other_ca = EnrollmentToken::new([0; SECRET_LEN], [2; 32]);
        let hex: String = hash(&token).iter().map(|b| format!("{b:02x}")).collect();
        // SHA-256 of 32 zero bytes.
        assert_eq!(
            hex,
            "66687aadf862bd776c8fc18b8e9f8e20089714856ee233b3902a591d0d5f2925"
        );
        assert_eq!(hash(&token), hash(&other_ca));
    }

    #[test]
    fn parses_ttls() {
        assert_eq!(parse_ttl("90s"), Ok(Duration::from_secs(90)));
        assert_eq!(parse_ttl("15m"), Ok(Duration::from_secs(900)));
        assert_eq!(parse_ttl("1h"), Ok(DEFAULT_TTL));
        assert_eq!(parse_ttl("7d"), Ok(MAX_TTL));
        for bad in ["", "h", "1", "1w", "-1h", "1.5h", "99999999999999999999d"] {
            assert!(parse_ttl(bad).is_err(), "{bad}");
        }
    }

    fn request(role: PeerRole, peer_id: &str, ttl: Duration) -> TokenRequest {
        TokenRequest {
            role,
            peer_id: peer_id.into(),
            site_id: Uuid::nil(),
            ttl,
            creator: Creator::Cli,
        }
    }

    #[test]
    fn requests_are_validated() {
        assert!(request(PeerRole::Agent, "web-01", DEFAULT_TTL)
            .validate()
            .is_ok());
        assert!(request(PeerRole::Relay, "relay-1", MIN_TTL)
            .validate()
            .is_ok());
        for bad in [
            request(PeerRole::Server, "central", DEFAULT_TTL),
            request(PeerRole::Agent, "web/01", DEFAULT_TTL),
            request(PeerRole::Agent, "", DEFAULT_TTL),
            request(PeerRole::Agent, "web-01", Duration::from_secs(59)),
            request(PeerRole::Agent, "web-01", MAX_TTL + Duration::from_secs(1)),
        ] {
            assert!(
                matches!(bad.validate(), Err(TokenError::Invalid(_))),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn cli_tokens_have_no_user() {
        assert_eq!(Creator::Cli.user_id(), None);
        assert_eq!(Creator::Cli.actor(), ("system", "cli".into()));
        let id = Uuid::new_v4();
        assert_eq!(Creator::User(id).actor(), ("user", id.to_string()));
    }
}
