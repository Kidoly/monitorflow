//! Server-side sessions. The cookie carries a random 256-bit token; the
//! `sessions` table stores only its SHA-256, with an absolute expiry and an
//! idle timeout checked on every request.

use std::fmt;

use axum::http::header::COOKIE;
use axum::http::{HeaderMap, HeaderValue};
use ring::digest::{digest, SHA256};
use ring::rand::{SecureRandom, SystemRandom};
use sqlx::{PgExecutor, PgPool};
use uuid::Uuid;

use super::ApiSettings;

/// Cookie name with `Secure`. The `__Host-` prefix makes browsers refuse it
/// unless it is `Secure`, has `Path=/` and no `Domain`.
const COOKIE_SECURE: &str = "__Host-mf_session";
/// Cookie name without `Secure`, for development over plain HTTP.
const COOKIE_PLAIN: &str = "mf_session";

const TOKEN_BYTES: usize = 32;

/// A session token as sent in the cookie. `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionToken(String);

impl fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SessionToken({})", monitorflow_common::redact::REDACTED)
    }
}

impl SessionToken {
    fn generate() -> Self {
        let mut bytes = [0u8; TOKEN_BYTES];
        SystemRandom::new()
            .fill(&mut bytes)
            .expect("the system random generator failed");
        Self(bytes.iter().map(|b| format!("{b:02x}")).collect())
    }

    /// Accepts only the exact format [`SessionToken::generate`] produces.
    fn parse(value: &str) -> Option<Self> {
        let valid = value.len() == 2 * TOKEN_BYTES
            && value
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        valid.then(|| Self(value.to_owned()))
    }

    fn hash(&self) -> Vec<u8> {
        digest(&SHA256, self.0.as_bytes()).as_ref().to_vec()
    }
}

fn cookie_name(settings: &ApiSettings) -> &'static str {
    if settings.cookie_secure {
        COOKIE_SECURE
    } else {
        COOKIE_PLAIN
    }
}

fn cookie(settings: &ApiSettings, value: &str, max_age: u64) -> HeaderValue {
    let secure = if settings.cookie_secure {
        "; Secure"
    } else {
        ""
    };
    let cookie = format!(
        "{}={value}; Path=/; Max-Age={max_age}; HttpOnly; SameSite=Strict{secure}",
        cookie_name(settings)
    );
    HeaderValue::from_str(&cookie).expect("cookie is ASCII")
}

/// `Set-Cookie` value that stores `token` for the session lifetime.
pub fn set_cookie(settings: &ApiSettings, token: &SessionToken) -> HeaderValue {
    cookie(settings, &token.0, settings.session_lifetime.as_secs())
}

/// `Set-Cookie` value that removes the session cookie.
pub fn clear_cookie(settings: &ApiSettings) -> HeaderValue {
    cookie(settings, "", 0)
}

/// The session token in the request's cookies, if well formed.
pub fn from_headers(headers: &HeaderMap, settings: &ApiSettings) -> Option<SessionToken> {
    let name = cookie_name(settings);
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .and_then(|(_, v)| SessionToken::parse(v))
}

/// Starts a session for `user_id` and returns its token.
pub async fn create(
    db: impl PgExecutor<'_>,
    user_id: Uuid,
    settings: &ApiSettings,
) -> Result<SessionToken, sqlx::Error> {
    let token = SessionToken::generate();
    sqlx::query(
        "INSERT INTO sessions (token_hash, user_id, expires_at)
         VALUES ($1, $2, now() + make_interval(secs => $3))",
    )
    .bind(token.hash())
    .bind(user_id)
    .bind(settings.session_lifetime.as_secs_f64())
    .execute(db)
    .await?;
    Ok(token)
}

/// The user of a live session, refreshing its idle timer. Expired or idle
/// sessions, and sessions of disabled users, resolve to `None`.
pub async fn resolve(
    pool: &PgPool,
    token: &SessionToken,
    settings: &ApiSettings,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "UPDATE sessions s SET last_seen_at = now()
         FROM users u
         WHERE s.token_hash = $1 AND u.id = s.user_id AND u.disabled_at IS NULL
           AND s.expires_at > now()
           AND s.last_seen_at > now() - make_interval(secs => $2)
         RETURNING s.user_id",
    )
    .bind(token.hash())
    .bind(settings.session_idle_timeout.as_secs_f64())
    .fetch_optional(pool)
    .await
}

/// Ends the session of `token`. Returns whether it existed.
pub async fn revoke(db: impl PgExecutor<'_>, token: &SessionToken) -> Result<bool, sqlx::Error> {
    let done = sqlx::query("DELETE FROM sessions WHERE token_hash = $1")
        .bind(token.hash())
        .execute(db)
        .await?;
    Ok(done.rows_affected() > 0)
}

/// Deletes sessions that can no longer be used.
pub async fn prune(db: impl PgExecutor<'_>, settings: &ApiSettings) -> Result<u64, sqlx::Error> {
    let done = sqlx::query(
        "DELETE FROM sessions
         WHERE expires_at <= now() OR last_seen_at <= now() - make_interval(secs => $1)",
    )
    .bind(settings.session_idle_timeout.as_secs_f64())
    .execute(db)
    .await?;
    Ok(done.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_random_hex_and_hash_to_sha256() {
        let a = SessionToken::generate();
        let b = SessionToken::generate();
        assert_ne!(a, b);
        assert_eq!(SessionToken::parse(&a.0), Some(a.clone()));
        assert_eq!(a.hash().len(), 32);
        assert!(!format!("{a:?}").contains(&a.0));
    }

    #[test]
    fn malformed_tokens_are_rejected() {
        let upper = "A".repeat(64);
        for bad in [
            "",
            "abc",
            &"a".repeat(63),
            &"a".repeat(65),
            &upper,
            &"g".repeat(64),
        ] {
            assert_eq!(SessionToken::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn cookie_attributes() {
        let token = SessionToken::generate();
        let settings = ApiSettings::default();
        let set = set_cookie(&settings, &token);
        let set = set.to_str().unwrap();
        assert!(
            set.starts_with(&format!("__Host-mf_session={}; Path=/;", token.0)),
            "{set}"
        );
        for attr in ["HttpOnly", "SameSite=Strict", "Secure", "Max-Age=43200"] {
            assert!(set.contains(attr), "{set}");
        }
        assert!(clear_cookie(&settings)
            .to_str()
            .unwrap()
            .contains("Max-Age=0"));

        let plain = ApiSettings {
            cookie_secure: false,
            ..ApiSettings::default()
        };
        let set = set_cookie(&plain, &token);
        assert!(set.to_str().unwrap().starts_with("mf_session="));
        assert!(!set.to_str().unwrap().contains("Secure"));
    }

    #[test]
    fn token_is_read_from_the_matching_cookie_only() {
        let token = SessionToken::generate();
        let settings = ApiSettings::default();
        let mut headers = HeaderMap::new();
        headers.append(COOKIE, HeaderValue::from_static("theme=dark"));
        headers.append(
            COOKIE,
            HeaderValue::from_str(&format!("mf_session=x; __Host-mf_session={}", token.0)).unwrap(),
        );
        assert_eq!(from_headers(&headers, &settings), Some(token));
        // The plain name is not accepted when cookies are Secure.
        let mut headers = HeaderMap::new();
        headers.append(COOKIE, HeaderValue::from_static("mf_session=x"));
        assert_eq!(from_headers(&headers, &settings), None);
    }
}
