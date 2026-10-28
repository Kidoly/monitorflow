//! User accounts and role grants, managed from the server CLI (`user add`,
//! `user grant`) and, for the first administrator, the setup link
//! ([`super::setup`]).

use monitorflow_common::redact::Secret;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
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

/// Where a grant applies (ADR-0026).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantScope {
    Global,
    /// One customer and all its sites.
    Customer(Uuid),
    Site(Uuid),
}

impl GrantScope {
    /// `(customer_id, site_id)` of `user_roles`.
    fn columns(self) -> (Option<Uuid>, Option<Uuid>) {
        match self {
            Self::Global => (None, None),
            Self::Customer(id) => (Some(id), None),
            Self::Site(id) => (None, Some(id)),
        }
    }

    /// Audit details of a grant: `role`, plus the scope unless global.
    fn details(self, role: Role) -> serde_json::Value {
        let mut details = json!({ "role": role });
        match self {
            Self::Global => {}
            Self::Customer(id) => details["customer_id"] = json!(id),
            Self::Site(id) => details["site_id"] = json!(id),
        }
        details
    }
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
    create_user_with(pool, username, password, role, GrantScope::Global, actor).await
}

/// [`create_user`] with the first role on `scope`.
pub async fn create_user_with(
    pool: &PgPool,
    username: &str,
    password: &Secret,
    role: Role,
    scope: GrantScope,
    actor: &str,
) -> Result<Uuid, CreateUserError> {
    let hash = hash_new_password(username, password).await?;
    let mut tx = pool.begin().await?;
    let id = insert_user(&mut tx, username, &hash, role, scope, ("system", actor)).await?;
    tx.commit().await?;
    Ok(id)
}

/// Validates a new account's username and password, then hashes the
/// password off the runtime.
pub(super) async fn hash_new_password(
    username: &str,
    password: &Secret,
) -> Result<String, CreateUserError> {
    validate_username(username)?;
    if password.expose().chars().count() < MIN_PASSWORD_LEN {
        return Err(CreateUserError::PasswordTooShort);
    }
    let password = password.clone();
    Ok(tokio::task::spawn_blocking(move || password::hash(password.expose())).await??)
}

/// Inserts a user with the argon2 `hash` and its first role, audited as
/// done by `actor` (`(actor_kind, actor_id)`), in the caller's transaction.
/// The setup link dies with it (ADR-0057).
pub(super) async fn insert_user(
    tx: &mut PgConnection,
    username: &str,
    hash: &str,
    role: Role,
    scope: GrantScope,
    actor: (&str, &str),
) -> Result<Uuid, CreateUserError> {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO users (username, password_hash) VALUES ($1, $2) RETURNING id",
    )
    .bind(username)
    .bind(hash)
    .fetch_one(&mut *tx)
    .await?;
    let (customer_id, site_id) = scope.columns();
    sqlx::query(
        "INSERT INTO user_roles (user_id, role, customer_id, site_id) VALUES ($1, $2, $3, $4)",
    )
    .bind(id)
    .bind(role.as_str())
    .bind(customer_id)
    .bind(site_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM setup_tokens")
        .execute(&mut *tx)
        .await?;
    let mut details = scope.details(role);
    details["username"] = json!(username);
    let target = id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: actor.0,
            actor_id: actor.1,
            action: "user.create",
            target: Some(("user", &target)),
            details,
        },
    )
    .await?;
    Ok(id)
}

/// ID of the user `username`, case-insensitively.
pub async fn user_id(pool: &PgPool, username: &str) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar("SELECT id FROM users WHERE lower(username) = lower($1)")
        .bind(username)
        .fetch_optional(pool)
        .await
}

/// Grants `role` on `scope` to `user` and audits it; `false` when the user
/// already had that exact grant.
pub async fn grant(
    pool: &PgPool,
    user: Uuid,
    role: Role,
    scope: GrantScope,
    actor: &str,
) -> Result<bool, sqlx::Error> {
    let (customer_id, site_id) = scope.columns();
    let mut tx = pool.begin().await?;
    let added = sqlx::query(
        "INSERT INTO user_roles (user_id, role, customer_id, site_id) VALUES ($1, $2, $3, $4)
         ON CONFLICT ON CONSTRAINT user_roles_key DO NOTHING",
    )
    .bind(user)
    .bind(role.as_str())
    .bind(customer_id)
    .bind(site_id)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    if added {
        let target = user.to_string();
        audit::record(
            &mut *tx,
            Event {
                actor_kind: "system",
                actor_id: actor,
                action: "user.grant",
                target: Some(("user", &target)),
                details: scope.details(role),
            },
        )
        .await?;
    }
    tx.commit().await?;
    Ok(added)
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
