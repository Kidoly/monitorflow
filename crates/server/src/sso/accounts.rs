//! Providers as stored, and the accounts they sign in (ADR-0145):
//! created at the first sign-in, roles replaced from the groups at each
//! one and when the mappings change, refused without a mapped group.

use chrono::{DateTime, Utc};
use monitorflow_common::redact::Secret;
use serde_json::json;
use sqlx::{PgConnection, PgExecutor, PgPool};
use uuid::Uuid;

use super::config::Settings;
use super::{grants, Mapping};
use crate::api::audit::{self, Event};
use crate::api::users::{validate_username, GrantScope};
use crate::api::Role;
use crate::secrets::Column;

/// A provider as stored.
#[derive(Debug, Clone)]
pub struct Provider {
    pub id: Uuid,
    pub name: String,
    pub enabled: bool,
    pub settings: Settings,
    /// Sealed (ADR-0125).
    pub secret: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

type Row = (
    Uuid,
    String,
    bool,
    serde_json::Value,
    Option<String>,
    DateTime<Utc>,
    DateTime<Utc>,
);

const COLUMNS: &str = "id, name, enabled, config, secret, created_at, updated_at";

fn provider(row: Row) -> Option<Provider> {
    let (id, name, enabled, config, secret, created_at, updated_at) = row;
    match serde_json::from_value(config) {
        Ok(settings) => Some(Provider {
            id,
            name,
            enabled,
            settings,
            secret,
            created_at,
            updated_at,
        }),
        Err(error) => {
            tracing::warn!(provider = %id, %error, "ignoring a sign-in provider whose settings do not read");
            None
        }
    }
}

impl Provider {
    /// The secret, opened; `Err` says why it does not open.
    pub fn secret(&self) -> Result<Option<Secret>, String> {
        crate::secrets::open_stored(Column::IdentityProviders, self.secret.clone()).transpose()
    }

    /// Every provider, oldest first: the order directories are tried in.
    pub async fn all(db: impl PgExecutor<'_>) -> Result<Vec<Self>, sqlx::Error> {
        let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM identity_providers ORDER BY created_at, id"
        )))
        .fetch_all(db)
        .await?;
        Ok(rows.into_iter().filter_map(provider).collect())
    }

    pub async fn get(db: impl PgExecutor<'_>, id: Uuid) -> Result<Option<Self>, sqlx::Error> {
        let row: Option<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM identity_providers WHERE id = $1"
        )))
        .bind(id)
        .fetch_optional(db)
        .await?;
        Ok(row.and_then(provider))
    }

    /// The provider locked for a change.
    pub async fn lock(db: &mut PgConnection, id: Uuid) -> Result<Option<Self>, sqlx::Error> {
        let row: Option<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM identity_providers WHERE id = $1 FOR UPDATE"
        )))
        .bind(id)
        .fetch_optional(&mut *db)
        .await?;
        Ok(row.and_then(provider))
    }
}

/// The mappings of a provider, in the order they were added.
pub async fn mappings(
    db: impl PgExecutor<'_>,
    provider: Uuid,
) -> Result<Vec<Mapping>, sqlx::Error> {
    let rows: Vec<(String, String, Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
        "SELECT group_name, role, customer_id, site_id FROM identity_provider_groups
         WHERE provider_id = $1 ORDER BY ctid",
    )
    .bind(provider)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(group, role, customer, site)| {
            Some(Mapping {
                group,
                role: role.parse().ok()?,
                scope: GrantScope::from_columns(customer, site)?,
            })
        })
        .collect())
}

/// Who a provider vouched for.
#[derive(Debug, Clone)]
pub struct Identity {
    pub subject: String,
    pub username: String,
    pub groups: Vec<String>,
}

/// Why a sign-in through a provider is refused, as audited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// In none of the mapped groups.
    NoGroup,
    /// A new account's name is another account's.
    UsernameTaken,
    /// The account is disabled.
    Disabled,
}

impl Refusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoGroup => "no_group",
            Self::UsernameTaken => "username_taken",
            Self::Disabled => "disabled",
        }
    }
}

/// The account a sign-in leads to, or why not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    SignedIn {
        user: Uuid,
        created: bool,
    },
    /// `user` is the account when there is one.
    Refused {
        reason: Refusal,
        user: Option<Uuid>,
    },
}

/// Who the audit log says made a change: the provider, as a `system`
/// actor.
fn actor(provider: Uuid) -> String {
    format!("identity_provider:{provider}")
}

async fn roles_of(
    db: &mut PgConnection,
    user: Uuid,
) -> Result<Vec<(Role, GrantScope)>, sqlx::Error> {
    let rows: Vec<(String, Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
        "SELECT role, customer_id, site_id FROM user_roles WHERE user_id = $1
         ORDER BY role, customer_id NULLS FIRST, site_id NULLS FIRST",
    )
    .bind(user)
    .fetch_all(&mut *db)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(r, c, s)| Some((r.parse().ok()?, GrantScope::from_columns(c, s)?)))
        .collect())
}

fn describe(grants: &[(Role, GrantScope)]) -> serde_json::Value {
    json!(grants
        .iter()
        .map(|(role, scope)| scope.details(*role))
        .collect::<Vec<_>>())
}

/// Replaces the roles of `user` with `wanted`, audited as `user.roles`
/// when they change. Returns whether they changed.
async fn set_roles(
    db: &mut PgConnection,
    provider: Uuid,
    user: Uuid,
    wanted: &[(Role, GrantScope)],
) -> Result<bool, sqlx::Error> {
    let mut old = roles_of(db, user).await?;
    let mut new = wanted.to_vec();
    let key = |g: &(Role, GrantScope)| (g.0, g.1.columns());
    old.sort_by_key(key);
    new.sort_by_key(key);
    if old == new {
        return Ok(false);
    }
    sqlx::query("DELETE FROM user_roles WHERE user_id = $1")
        .bind(user)
        .execute(&mut *db)
        .await?;
    for (role, scope) in &new {
        let (customer, site) = scope.columns();
        sqlx::query(
            "INSERT INTO user_roles (user_id, role, customer_id, site_id) VALUES ($1, $2, $3, $4)",
        )
        .bind(user)
        .bind(role.as_str())
        .bind(customer)
        .bind(site)
        .execute(&mut *db)
        .await?;
    }
    let actor = actor(provider);
    let target = user.to_string();
    audit::record(
        &mut *db,
        Event {
            actor_kind: "system",
            actor_id: &actor,
            action: "user.roles",
            target: Some(("user", &target)),
            details: json!({ "old": describe(&old), "new": describe(&new) }),
        },
    )
    .await?;
    Ok(true)
}

/// Signs `identity` in through `provider`: finds or creates its account,
/// replaces its roles from its groups, or says why not. Sessions are the
/// caller's.
pub async fn sign_in(
    pool: &PgPool,
    provider: &Provider,
    identity: &Identity,
) -> Result<Outcome, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let mappings = mappings(&mut *tx, provider.id).await?;
    let wanted = grants(&identity.groups, &mappings);
    let existing: Option<(Uuid, bool)> = sqlx::query_as(
        "SELECT id, disabled_at IS NOT NULL FROM users
         WHERE identity_provider_id = $1 AND external_subject = $2 FOR UPDATE",
    )
    .bind(provider.id)
    .bind(&identity.subject)
    .fetch_optional(&mut *tx)
    .await?;
    let outcome = match existing {
        Some((user, disabled)) => {
            sqlx::query("UPDATE users SET external_groups = $2 WHERE id = $1")
                .bind(user)
                .bind(&identity.groups)
                .execute(&mut *tx)
                .await?;
            set_roles(&mut tx, provider.id, user, &wanted).await?;
            if wanted.is_empty() {
                end_sessions(&mut tx, user).await?;
                Outcome::Refused {
                    reason: Refusal::NoGroup,
                    user: Some(user),
                }
            } else if disabled {
                Outcome::Refused {
                    reason: Refusal::Disabled,
                    user: Some(user),
                }
            } else {
                Outcome::SignedIn {
                    user,
                    created: false,
                }
            }
        }
        None if wanted.is_empty() => Outcome::Refused {
            reason: Refusal::NoGroup,
            user: None,
        },
        None => create(&mut tx, provider, identity, &wanted).await?,
    };
    tx.commit().await?;
    Ok(outcome)
}

async fn create(
    tx: &mut PgConnection,
    provider: &Provider,
    identity: &Identity,
    wanted: &[(Role, GrantScope)],
) -> Result<Outcome, sqlx::Error> {
    if validate_username(&identity.username).is_err() {
        return Ok(Outcome::Refused {
            reason: Refusal::UsernameTaken,
            user: None,
        });
    }
    // Never joined to an existing account by its name (ADR-0145).
    let user: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO users (username, identity_provider_id, external_subject, external_groups)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT DO NOTHING RETURNING id",
    )
    .bind(&identity.username)
    .bind(provider.id)
    .bind(&identity.subject)
    .bind(&identity.groups)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(user) = user else {
        return Ok(Outcome::Refused {
            reason: Refusal::UsernameTaken,
            user: None,
        });
    };
    let actor = actor(provider.id);
    let target = user.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "system",
            actor_id: &actor,
            action: "user.create",
            target: Some(("user", &target)),
            details: json!({
                "username": identity.username,
                "via": provider.settings.kind().as_str(),
                "provider": provider.name,
            }),
        },
    )
    .await?;
    set_roles(tx, provider.id, user, wanted).await?;
    Ok(Outcome::SignedIn {
        user,
        created: true,
    })
}

async fn end_sessions(db: &mut PgConnection, user: Uuid) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query("DELETE FROM sessions WHERE user_id = $1")
        .bind(user)
        .execute(&mut *db)
        .await?
        .rows_affected())
}

/// After a change of `provider`'s mappings, in the same transaction:
/// replaces the roles of each of its accounts from the groups it had at
/// its last sign-in, and ends the sessions of those left without any.
/// Returns how many accounts changed.
pub async fn resync(db: &mut PgConnection, provider: Uuid) -> Result<usize, sqlx::Error> {
    let mappings = mappings(&mut *db, provider).await?;
    let accounts: Vec<(Uuid, Option<Vec<String>>)> = sqlx::query_as(
        "SELECT id, external_groups FROM users WHERE identity_provider_id = $1 ORDER BY id FOR UPDATE",
    )
    .bind(provider)
    .fetch_all(&mut *db)
    .await?;
    let mut changed = 0;
    for (user, groups) in accounts {
        let wanted = grants(&groups.unwrap_or_default(), &mappings);
        if set_roles(db, provider, user, &wanted).await? {
            changed += 1;
        }
        if wanted.is_empty() {
            end_sessions(db, user).await?;
        }
    }
    Ok(changed)
}

/// Ends every session of `provider`'s accounts, when it is turned off.
pub async fn end_provider_sessions(
    db: &mut PgConnection,
    provider: Uuid,
) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query(
        "DELETE FROM sessions WHERE user_id IN (SELECT id FROM users WHERE identity_provider_id = $1)",
    )
    .bind(provider)
    .execute(&mut *db)
    .await?
    .rows_affected())
}

/// Before `provider` is removed: its accounts lose their sessions, roles
/// and identity, and keep their name. Returns how many there were.
pub async fn release(db: &mut PgConnection, provider: Uuid) -> Result<u64, sqlx::Error> {
    end_provider_sessions(db, provider).await?;
    sqlx::query(
        "DELETE FROM user_roles WHERE user_id IN (SELECT id FROM users WHERE identity_provider_id = $1)",
    )
    .bind(provider)
    .execute(&mut *db)
    .await?;
    Ok(sqlx::query(
        "UPDATE users SET identity_provider_id = NULL, external_subject = NULL, external_groups = NULL
         WHERE identity_provider_id = $1",
    )
    .bind(provider)
    .execute(&mut *db)
    .await?
    .rows_affected())
}

/// How many accounts sign in through each provider.
pub async fn account_counts(db: impl PgExecutor<'_>) -> Result<Vec<(Uuid, i64)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT identity_provider_id, count(*) FROM users
         WHERE identity_provider_id IS NOT NULL GROUP BY identity_provider_id",
    )
    .fetch_all(db)
    .await
}
