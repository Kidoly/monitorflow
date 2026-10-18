//! PostgreSQL access: connection pool and migrations in `crates/server/migrations`.

use std::str::FromStr;
use std::time::Duration;

use monitorflow_common::config::DatabaseUrl;
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

/// Whether the relay with certificate ID `relay_peer_id` may speak for the
/// agent `agent_peer_id` (ADR-0004): both exist, neither is revoked, and they
/// belong to the same site. Both lookups use the unique `peer_id` indexes.
pub async fn relay_may_speak_for(
    pool: &PgPool,
    relay_peer_id: &str,
    agent_peer_id: &str,
) -> Result<bool, DbError> {
    let allowed = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
             FROM relays r
             JOIN servers s ON s.site_id = r.site_id
             WHERE r.peer_id = $1 AND r.revoked_at IS NULL
               AND s.peer_id = $2 AND s.revoked_at IS NULL
         )",
    )
    .bind(relay_peer_id)
    .bind(agent_peer_id)
    .fetch_one(pool)
    .await?;
    Ok(allowed)
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
