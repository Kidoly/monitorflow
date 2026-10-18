//! User accounts. For now only creation, used by the `user add` command to
//! bootstrap the first administrator.

use monitorflow_common::redact::Secret;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use super::audit::{self, Event};
use super::password;
use super::roles::Role;

pub const MIN_PASSWORD_LEN: usize = 12;
pub const MAX_USERNAME_LEN: usize = 64;

const UNIQUE_VIOLATION: &str = "23505";

#[derive(Debug, thiserror::Error)]
pub enum CreateUserError {
    #[error(
        "username must be 1 to {MAX_USERNAME_LEN} characters without spaces or control characters"
    )]
    InvalidUsername,
    #[error("password must be at least {MIN_PASSWORD_LEN} characters")]
    PasswordTooShort,
    #[error("a user with this name already exists (names are case-insensitive)")]
    AlreadyExists,
    #[error(transparent)]
    Hash(#[from] password::HashError),
    #[error("password hashing task failed")]
    Task(#[from] tokio::task::JoinError),
    #[error("database query failed: {0}")]
    Database(#[source] sqlx::Error),
}

impl From<sqlx::Error> for CreateUserError {
    fn from(error: sqlx::Error) -> Self {
        let unique = error
            .as_database_error()
            .and_then(|e| e.code())
            .is_some_and(|code| code == UNIQUE_VIOLATION);
        if unique {
            Self::AlreadyExists
        } else {
            Self::Database(error)
        }
    }
}

pub fn validate_username(username: &str) -> Result<(), CreateUserError> {
    let len = username.chars().count();
    let valid = (1..=MAX_USERNAME_LEN).contains(&len)
        && !username
            .chars()
            .any(|c| c.is_whitespace() || c.is_control());
    valid.then_some(()).ok_or(CreateUserError::InvalidUsername)
}

/// Creates a user with one global role and records it in the audit log as
/// done by `actor` (`system` kind), in one transaction.
pub async fn create_user(
    pool: &PgPool,
    username: &str,
    password: &Secret,
    role: Role,
    actor: &str,
) -> Result<Uuid, CreateUserError> {
    validate_username(username)?;
    if password.expose().chars().count() < MIN_PASSWORD_LEN {
        return Err(CreateUserError::PasswordTooShort);
    }
    let password = password.clone();
    let hash = tokio::task::spawn_blocking(move || password::hash(password.expose())).await??;

    let mut tx = pool.begin().await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO users (username, password_hash) VALUES ($1, $2) RETURNING id",
    )
    .bind(username)
    .bind(hash)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO user_roles (user_id, role) VALUES ($1, $2)")
        .bind(id)
        .bind(role.as_str())
        .execute(&mut *tx)
        .await?;
    let target = id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "system",
            actor_id: actor,
            action: "user.create",
            target: Some(("user", &target)),
            details: json!({ "username": username, "role": role }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usernames_are_validated() {
        for ok in ["admin", "Alice.Martin", "é", &"a".repeat(64)] {
            assert!(validate_username(ok).is_ok(), "{ok}");
        }
        for bad in ["", "a b", "a\tb", "a\u{7}", &"a".repeat(65)] {
            assert!(validate_username(bad).is_err(), "{bad:?}");
        }
    }
}
