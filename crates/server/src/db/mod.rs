//! PostgreSQL access: connection pool and migrations in `crates/server/migrations`.

use std::str::FromStr;
use std::time::Duration;

use monitorflow_common::config::DatabaseUrl;
use monitorflow_common::tls::{PeerIdentity, PeerRole};
use sqlx::migrate::{MigrateError, Migrator};
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};

/// Migrations embedded at compile time from `crates/server/migrations`.
pub static MIGRATOR: Migrator = sqlx::migrate!();

const MAX_CONNECTIONS: u32 = 16;
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);
const IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const MAX_LIFETIME: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// The source error comes from URL parsing and never contains the URL.
    #[error("invalid database URL")]
    InvalidUrl(#[source] sqlx::Error),
    #[error("cannot connect to PostgreSQL: {0}")]
    Connect(#[source] sqlx::Error),
    #[error("database migration failed: {0}")]
    Migrate(#[from] MigrateError),
    #[error("database query failed: {0}")]
    Query(#[from] sqlx::Error),
}

/// Parses `url` into connection options without echoing it in errors.
/// Never log the result: its `Debug` output includes the password.
fn connect_options(url: &DatabaseUrl) -> Result<PgConnectOptions, DbError> {
    PgConnectOptions::from_str(url.expose()).map_err(DbError::InvalidUrl)
}

/// Opens a connection pool and checks that one connection succeeds.
pub async fn connect(url: &DatabaseUrl) -> Result<PgPool, DbError> {
    let options = connect_options(url)?;
    let pool = PgPoolOptions::new()
        .max_connections(MAX_CONNECTIONS)
        .acquire_timeout(ACQUIRE_TIMEOUT)
        .idle_timeout(IDLE_TIMEOUT)
        .max_lifetime(MAX_LIFETIME)
        .connect_with(options)
        .await
        .map_err(DbError::Connect)?;
    // `DatabaseUrl`'s Debug output redacts the password.
    tracing::info!(url = ?url, "connected to PostgreSQL");
    Ok(pool)
}

/// Applies pending migrations. Safe to call on every start.
pub async fn migrate(pool: &PgPool) -> Result<(), DbError> {
    MIGRATOR.run(pool).await?;
    tracing::info!("database migrations applied");
    Ok(())
}

/// Site of the latest enrollment token consumed by the agent `$1`: where
/// its `servers` row is created at its first Hello (ADR-0008).
pub const ENROLLED_AGENT_SITE: &str = "SELECT site_id FROM enrollment_tokens
     WHERE peer_id = $1 AND role = 'agent' AND used_at IS NOT NULL
     ORDER BY used_at DESC LIMIT 1";

/// Whether the relay with certificate ID `relay_peer_id` may speak for the
/// agent `agent_peer_id` (ADR-0004): the relay exists and is not revoked,
/// the agent's row is not revoked, and the agent belongs to the relay's
/// site. An agent without a row yet belongs to the site of its enrollment
/// token, so its first Hello can come through a relay (ADR-0008).
pub async fn relay_may_speak_for(
    pool: &PgPool,
    relay_peer_id: &str,
    agent_peer_id: &str,
) -> Result<bool, DbError> {
    let allowed = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
             FROM relays r
             WHERE r.peer_id = $1 AND r.revoked_at IS NULL
               -- Until an administrator confirms it, a relay speaks for itself only.
               AND r.pending_names IS NULL
               AND r.site_id = COALESCE(
                   (SELECT s.site_id FROM servers s WHERE s.peer_id = $2),
                   (SELECT t.site_id FROM enrollment_tokens t
                    WHERE t.peer_id = $2 AND t.role = 'agent' AND t.used_at IS NOT NULL
                    ORDER BY t.used_at DESC LIMIT 1))
               AND NOT EXISTS (
                   SELECT 1 FROM servers s WHERE s.peer_id = $2 AND s.revoked_at IS NOT NULL
                   -- A removed server enrolled again, waiting for approval.
                   AND NOT EXISTS (SELECT 1 FROM pending_hosts p WHERE p.peer_id = $2))
         )",
    )
    .bind(relay_peer_id)
    .bind(agent_peer_id)
    .fetch_one(pool)
    .await?;
    Ok(allowed)
}

/// Whether a call forwarded along `relays` may speak for `origin`
/// (ADR-0028). `relays` starts with the connecting relay and ends with the
/// one `origin` is attached to; each must be the parent of the next, and
/// none revoked. An agent must then belong to the site of the relay it is
/// attached to ([`relay_may_speak_for`]); a relay must be the child of it.
/// So a relay speaks only for its own subtree.
pub async fn relay_route_allowed(
    pool: &PgPool,
    relays: &[&str],
    origin: &PeerIdentity,
) -> Result<bool, DbError> {
    let Some(attached) = relays.last() else {
        return Ok(false);
    };
    let links_ok: bool = sqlx::query_scalar(
        "SELECT count(*) = cardinality($1::text[]) - 1
         FROM unnest($1::text[]) WITH ORDINALITY AS hop (peer_id, n)
         JOIN relays child ON child.peer_id = hop.peer_id AND child.revoked_at IS NULL
         JOIN relays parent ON parent.id = child.parent_relay_id AND parent.revoked_at IS NULL
              AND parent.pending_names IS NULL
         WHERE hop.n > 1 AND parent.peer_id = ($1::text[])[hop.n - 1]",
    )
    .bind(relays)
    .fetch_one(pool)
    .await?;
    if !links_ok {
        return Ok(false);
    }
    match origin.role() {
        PeerRole::Agent => relay_may_speak_for(pool, attached, origin.id()).await,
        PeerRole::Relay => Ok(sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1 FROM relays child
                 JOIN relays parent ON parent.id = child.parent_relay_id
                 WHERE child.peer_id = $1 AND parent.peer_id = $2 AND parent.revoked_at IS NULL
                   AND parent.pending_names IS NULL)",
        )
        .bind(origin.id())
        .bind(attached)
        .fetch_one(pool)
        .await?),
        PeerRole::Server => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_url_error_does_not_echo_password() {
        let url: DatabaseUrl = "postgres://mf:hunter2@db:notaport/mf".parse().unwrap();
        let err = connect_options(&url).unwrap_err();
        assert!(matches!(err, DbError::InvalidUrl(_)));
        let chain = format!("{err} {:?}", std::error::Error::source(&err));
        assert!(!chain.contains("hunter2"), "{chain}");
    }

    #[test]
    fn valid_url_parses() {
        let url: DatabaseUrl = "postgres://mf:pw@localhost:5432/mf".parse().unwrap();
        let options = connect_options(&url).unwrap();
        assert_eq!(options.get_database(), Some("mf"));
    }

    #[test]
    fn migrations_are_embedded() {
        assert!(MIGRATOR.iter().any(|m| m.description == "initial schema"));
    }
}
