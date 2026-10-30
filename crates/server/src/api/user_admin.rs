//! Users managed from the web interface (ADR-0059). Global administrators
//! invite users, grant and revoke roles, disable, enable and remove
//! accounts, and issue password links. A password link,
//! `/welcome/#<secret>`, lets its user choose a password and signs them
//! in: it is both the invitation and the password reset.
//!
//! No change may leave the server without an active global administrator,
//! and an administrator cannot disable or remove their own account.

use std::time::Duration;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::header::SET_COOKIE;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use monitorflow_common::redact::Secret;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgConnection;
use uuid::Uuid;

use super::access::Scope;
use super::audit::{self, Event};
use super::auth::{self, CurrentUser, Me};
use super::one_time::OneTimeToken;
use super::sensors::{body, parse_id};
use super::users::{self, CreateUserError, GrantScope};
use super::{session, ApiError, ApiState, Role, RoleGrant};

/// How long a password link stays valid: long enough for an invitation to
/// be read after a weekend.
pub const LINK_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// What a dead password link answers.
const LINK_GONE: &str = "this link is no longer valid; ask an administrator for a new one";

/// One user of `GET /api/users`.
#[derive(Debug, Serialize)]
pub struct UserView {
    pub id: Uuid,
    pub username: String,
    /// `active`, `invited` (no password yet) or `disabled`.
    pub state: &'static str,
    pub created_at: DateTime<Utc>,
    pub roles: Vec<RoleGrant>,
    /// Expiry of the user's unused password link, if any.
    pub link_expires_at: Option<DateTime<Utc>>,
}

/// Body of `GET /api/users`, sorted by username.
#[derive(Debug, Serialize)]
pub struct UserList {
    pub users: Vec<UserView>,
}

/// A new password link: the only time its secret is returned.
#[derive(Debug, Serialize)]
pub struct LinkView {
    /// Goes in the fragment of `/welcome/#<token>`.
    pub token: String,
    pub expires_at: DateTime<Utc>,
}

/// Body of `POST /api/users`.
#[derive(Debug, Serialize)]
pub struct Invited {
    pub user: UserView,
    pub link: LinkView,
}

/// A role and where it applies; both IDs null is everything.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantRequest {
    pub role: Role,
    #[serde(default)]
    pub customer_id: Option<Uuid>,
    #[serde(default)]
    pub site_id: Option<Uuid>,
}

/// Body of `POST /api/users`: the account and its first role.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InviteRequest {
    pub username: String,
    #[serde(flatten)]
    pub grant: GrantRequest,
}

/// Body of `PUT /api/users/{id}/disabled`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisabledRequest {
    pub disabled: bool,
}

fn require_global_admin(user: &CurrentUser) -> Result<(), ApiError> {
    if user.administered_customers() == Scope::All {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

type Row = (
    Uuid,
    String,
    bool,
    bool,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
);

async fn fetch_users(db: &mut PgConnection, id: Option<Uuid>) -> Result<Vec<UserView>, ApiError> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT u.id, u.username, u.disabled_at IS NOT NULL, u.password_hash IS NULL,
                u.created_at, l.expires_at
         FROM users u LEFT JOIN password_links l ON l.user_id = u.id AND l.expires_at > now()
         WHERE $1::uuid IS NULL OR u.id = $1
         ORDER BY lower(u.username)",
    )
    .bind(id)
    .fetch_all(&mut *db)
    .await?;
    let grants: Vec<(Uuid, String, Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
        "SELECT user_id, role, customer_id, site_id FROM user_roles
         WHERE $1::uuid IS NULL OR user_id = $1
         ORDER BY role DESC, customer_id NULLS FIRST, site_id NULLS FIRST",
    )
    .bind(id)
    .fetch_all(&mut *db)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(id, username, disabled, invited, created_at, link_expires_at)| UserView {
                id,
                username,
                state: if disabled {
                    "disabled"
                } else if invited {
                    "invited"
                } else {
                    "active"
                },
                created_at,
                roles: grants
                    .iter()
                    .filter(|g| g.0 == id)
                    .filter_map(|(_, role, customer_id, site_id)| {
                        Some(RoleGrant {
                            role: role.parse().ok()?,
                            customer_id: *customer_id,
                            site_id: *site_id,
                        })
                    })
                    .collect(),
                link_expires_at,
            },
        )
        .collect())
}

async fn fetch_user(db: &mut PgConnection, id: Uuid) -> Result<UserView, ApiError> {
    fetch_users(db, Some(id))
        .await?
        .pop()
        .ok_or(ApiError::NotFound)
}

/// Active global administrators: enabled, with a password, and an
/// administrator role on everything.
async fn global_admins(db: &mut PgConnection) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(DISTINCT u.id) FROM users u JOIN user_roles r ON r.user_id = u.id
         WHERE r.role = 'administrator' AND r.customer_id IS NULL AND r.site_id IS NULL
           AND u.disabled_at IS NULL AND u.password_hash IS NOT NULL",
    )
    .fetch_one(db)
    .await
}

/// Starts a transaction for a change that could remove the last active
/// global administrator, serialized with every other such change, and
/// returns it with the count before the change.
async fn guarded(
    state: &ApiState,
) -> Result<(sqlx::Transaction<'static, sqlx::Postgres>, i64), ApiError> {
    let mut tx = state.pool.begin().await?;
    sqlx::query("LOCK TABLE user_roles IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await?;
    let before = global_admins(&mut tx).await?;
    Ok((tx, before))
}

/// Refuses the change made in `tx` if it removed the last active global
/// administrator.
async fn check_guard(tx: &mut PgConnection, before: i64) -> Result<(), ApiError> {
    if before > 0 && global_admins(tx).await? == 0 {
        return Err(ApiError::Conflict(
            "this would leave no active administrator of everything; make another user one first"
                .into(),
        ));
    }
    Ok(())
}

async fn record(
    tx: &mut PgConnection,
    actor: &CurrentUser,
    action: &str,
    target: Uuid,
    details: serde_json::Value,
) -> Result<(), sqlx::Error> {
    let actor = actor.id.to_string();
    let target = target.to_string();
    audit::record(
        tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action,
            target: Some(("user", &target)),
            details,
        },
    )
    .await
}

/// The grant's scope, checking that its customer or site exists.
async fn scope_of(db: &mut PgConnection, grant: &GrantRequest) -> Result<GrantScope, ApiError> {
    let scope = GrantScope::from_columns(grant.customer_id, grant.site_id).ok_or_else(|| {
        ApiError::Invalid("a role applies to a customer or a site, not both".into())
    })?;
    let exists: bool = match scope {
        GrantScope::Global => true,
        GrantScope::Customer(id) => {
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM customers WHERE id = $1)")
                .bind(id)
                .fetch_one(&mut *db)
                .await?
        }
        GrantScope::Site(id) => {
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM sites WHERE id = $1)")
                .bind(id)
                .fetch_one(&mut *db)
                .await?
        }
    };
    if !exists {
        return Err(ApiError::Invalid(
            "this customer or site does not exist".into(),
        ));
    }
    Ok(scope)
}

/// Replaces the user's password link with a new one, in `tx`.
async fn issue_link(
    tx: &mut PgConnection,
    actor: &CurrentUser,
    user: Uuid,
) -> Result<LinkView, ApiError> {
    let token = OneTimeToken::generate();
    let expires_at: DateTime<Utc> = sqlx::query_scalar(
        "INSERT INTO password_links (user_id, token_hash, expires_at)
         VALUES ($1, $2, now() + make_interval(secs => $3))
         ON CONFLICT (user_id) DO UPDATE
         SET token_hash = EXCLUDED.token_hash, created_at = now(),
             expires_at = EXCLUDED.expires_at
         RETURNING expires_at",
    )
    .bind(user)
    .bind(token.hash())
    .bind(LINK_TTL.as_secs_f64())
    .fetch_one(&mut *tx)
    .await?;
    record(
        tx,
        actor,
        "user.link_issued",
        user,
        json!({ "ttl_seconds": LINK_TTL.as_secs() }),
    )
    .await?;
    Ok(LinkView {
        token: token.expose().to_owned(),
        expires_at,
    })
}

/// `GET /api/users`: every account, global administrators only.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<UserList>, ApiError> {
    require_global_admin(&user)?;
    let mut db = state.pool.acquire().await?;
    Ok(Json(UserList {
        users: fetch_users(&mut db, None).await?,
    }))
}

/// `POST /api/users`: creates an invited user with one role and returns
/// its password link.
pub async fn invite(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<InviteRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Invited>), ApiError> {
    require_global_admin(&user)?;
    let request = body(request)?;
    let username = request.username.trim();
    users::validate_username(username).map_err(|e| ApiError::Invalid(e.to_string()))?;
    let mut tx = state.pool.begin().await?;
    let scope = scope_of(&mut tx, &request.grant).await?;
    let actor = user.id.to_string();
    let id = users::insert_user(
        &mut tx,
        username,
        None,
        request.grant.role,
        scope,
        ("user", &actor),
    )
    .await
    .map_err(|e| match e {
        CreateUserError::AlreadyExists => ApiError::Conflict(e.to_string()),
        CreateUserError::Database(e) => ApiError::Database(e),
        other => ApiError::Internal(other.to_string()),
    })?;
    let link = issue_link(&mut tx, &user, id).await?;
    let view = fetch_user(&mut tx, id).await?;
    tx.commit().await?;
    tracing::info!(user = %user.id, invited = %id, "user invited");
    Ok((StatusCode::CREATED, Json(Invited { user: view, link })))
}

/// `POST /api/users/{id}/link`: a new password link for the user,
/// replacing any previous one. The current password keeps working until
/// the link is used.
pub async fn link(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<LinkView>), ApiError> {
    require_global_admin(&user)?;
    let id = parse_id(&id)?;
    let mut tx = state.pool.begin().await?;
    let disabled: Option<bool> =
        sqlx::query_scalar("SELECT disabled_at IS NOT NULL FROM users WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    match disabled {
        None => return Err(ApiError::NotFound),
        Some(true) => {
            return Err(ApiError::Conflict(
                "this account is disabled; enable it first".into(),
            ))
        }
        Some(false) => {}
    }
    let link = issue_link(&mut tx, &user, id).await?;
    tx.commit().await?;
    tracing::info!(user = %user.id, target = %id, "password link issued");
    Ok((StatusCode::CREATED, Json(link)))
}

/// `PUT /api/users/{id}/disabled`: disabling ends the user's sessions and
/// voids its password link; enabling lets it sign in again.
pub async fn set_disabled(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<DisabledRequest>, JsonRejection>,
) -> Result<Json<UserView>, ApiError> {
    require_global_admin(&user)?;
    let id = parse_id(&id)?;
    let disabled = body(request)?.disabled;
    if disabled && id == user.id {
        return Err(ApiError::Conflict(
            "you cannot disable your own account".into(),
        ));
    }
    let (mut tx, before) = guarded(&state).await?;
    let changed = sqlx::query(
        "UPDATE users SET disabled_at = CASE WHEN $2 THEN now() END
         WHERE id = $1 AND (disabled_at IS NOT NULL) <> $2",
    )
    .bind(id)
    .bind(disabled)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    if changed {
        if disabled {
            sqlx::query("DELETE FROM sessions WHERE user_id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM password_links WHERE user_id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        check_guard(&mut tx, before).await?;
        let action = if disabled {
            "user.disable"
        } else {
            "user.enable"
        };
        record(&mut tx, &user, action, id, json!({})).await?;
    }
    let view = fetch_user(&mut tx, id).await?;
    tx.commit().await?;
    if changed {
        tracing::info!(user = %user.id, target = %id, disabled, "user state changed");
    }
    Ok(Json(view))
}

/// `DELETE /api/users/{id}`: removes the account, its roles, sessions and
/// link. What it did stays in the audit log and in the history it left.
pub async fn remove(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_global_admin(&user)?;
    let id = parse_id(&id)?;
    if id == user.id {
        return Err(ApiError::Conflict(
            "you cannot remove your own account".into(),
        ));
    }
    let (mut tx, before) = guarded(&state).await?;
    let username: String = sqlx::query_scalar("DELETE FROM users WHERE id = $1 RETURNING username")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ApiError::NotFound)?;
    check_guard(&mut tx, before).await?;
    record(
        &mut tx,
        &user,
        "user.remove",
        id,
        json!({ "username": username }),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(user = %user.id, removed = %id, "user removed");
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/users/{id}/roles`: grants a role; granting one the user has
/// changes nothing.
pub async fn grant(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<GrantRequest>, JsonRejection>,
) -> Result<Json<UserView>, ApiError> {
    require_global_admin(&user)?;
    let id = parse_id(&id)?;
    let request = body(request)?;
    let mut tx = state.pool.begin().await?;
    let view = fetch_user(&mut tx, id).await?;
    let scope = scope_of(&mut tx, &request).await?;
    let (customer_id, site_id) = scope.columns();
    let added = sqlx::query(
        "INSERT INTO user_roles (user_id, role, customer_id, site_id) VALUES ($1, $2, $3, $4)
         ON CONFLICT ON CONSTRAINT user_roles_key DO NOTHING",
    )
    .bind(view.id)
    .bind(request.role.as_str())
    .bind(customer_id)
    .bind(site_id)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    if added {
        record(
            &mut tx,
            &user,
            "user.grant",
            id,
            scope.details(request.role),
        )
        .await?;
    }
    let view = fetch_user(&mut tx, id).await?;
    tx.commit().await?;
    Ok(Json(view))
}

/// `DELETE /api/users/{id}/roles`: revokes one grant, given in the body.
pub async fn revoke(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<GrantRequest>, JsonRejection>,
) -> Result<Json<UserView>, ApiError> {
    require_global_admin(&user)?;
    let id = parse_id(&id)?;
    let request = body(request)?;
    let scope =
        GrantScope::from_columns(request.customer_id, request.site_id).ok_or_else(|| {
            ApiError::Invalid("a role applies to a customer or a site, not both".into())
        })?;
    let (customer_id, site_id) = scope.columns();
    let (mut tx, before) = guarded(&state).await?;
    let removed = sqlx::query(
        "DELETE FROM user_roles WHERE user_id = $1 AND role = $2
           AND customer_id IS NOT DISTINCT FROM $3 AND site_id IS NOT DISTINCT FROM $4",
    )
    .bind(id)
    .bind(request.role.as_str())
    .bind(customer_id)
    .bind(site_id)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    if removed {
        check_guard(&mut tx, before).await?;
        record(
            &mut tx,
            &user,
            "user.revoke",
            id,
            scope.details(request.role),
        )
        .await?;
    }
    let view = fetch_user(&mut tx, id).await?;
    tx.commit().await?;
    Ok(Json(view))
}

/// Body of `POST /api/password-link`.
#[derive(Deserialize)]
pub struct LinkRequest {
    pub token: String,
}

/// Answer of `POST /api/password-link`: whose link it is.
#[derive(Debug, Serialize)]
pub struct LinkOwner {
    pub username: String,
    /// The user never had a password: the link is an invitation.
    pub invited: bool,
}

/// Body of `POST /api/password-link/use`.
#[derive(Deserialize)]
pub struct UseLinkRequest {
    pub token: String,
    pub password: Secret,
}

/// `POST /api/password-link`, without authentication: the user a live link
/// belongs to, so the page can greet them. 410 for any other token.
pub async fn inspect_link(
    State(state): State<ApiState>,
    request: Result<Json<LinkRequest>, JsonRejection>,
) -> Result<Json<LinkOwner>, ApiError> {
    let Json(request) = request.map_err(|_| ApiError::BadRequest)?;
    let token = OneTimeToken::parse(&request.token).ok_or(ApiError::Gone(LINK_GONE.into()))?;
    let owner: Option<(String, bool)> = sqlx::query_as(
        "SELECT u.username, u.password_hash IS NULL
         FROM password_links l JOIN users u ON u.id = l.user_id
         WHERE l.token_hash = $1 AND l.expires_at > now() AND u.disabled_at IS NULL",
    )
    .bind(token.hash())
    .fetch_optional(&state.pool)
    .await?;
    let (username, invited) = owner.ok_or(ApiError::Gone(LINK_GONE.into()))?;
    Ok(Json(LinkOwner { username, invited }))
}

/// `POST /api/password-link/use`, without authentication: consumes the
/// link, sets the password, ends the user's other sessions and signs it in.
pub async fn use_link(
    State(state): State<ApiState>,
    request: Result<Json<UseLinkRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = request.map_err(|_| ApiError::BadRequest)?;
    // Checked before the link is consumed, so a short password keeps it.
    let hash = users::hash_password(&request.password)
        .await
        .map_err(|e| match e {
            CreateUserError::PasswordTooShort => ApiError::Invalid(e.to_string()),
            other => ApiError::Internal(other.to_string()),
        })?;
    let token = OneTimeToken::parse(&request.token).ok_or(ApiError::Gone(LINK_GONE.into()))?;

    let mut tx = state.pool.begin().await?;
    // The DELETE takes the row: of two requests with the same link, the
    // second finds nothing once the first commits.
    let live: Option<(Uuid, bool)> = sqlx::query_as(
        "DELETE FROM password_links l USING users u
         WHERE l.token_hash = $1 AND u.id = l.user_id
         RETURNING l.user_id, l.expires_at > now() AND u.disabled_at IS NULL",
    )
    .bind(token.hash())
    .fetch_optional(&mut *tx)
    .await?;
    let Some((id, true)) = live else {
        // An expired link is deleted all the same.
        tx.commit().await?;
        tracing::info!(expired = live.is_some(), "password link refused");
        return Err(ApiError::Gone(LINK_GONE.into()));
    };
    sqlx::query("UPDATE users SET password_hash = $2 WHERE id = $1")
        .bind(id)
        .bind(&hash)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM sessions WHERE user_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let session = session::create(&mut *tx, id, &state.settings).await?;
    let actor = id.to_string();
    for (action, details) in [
        ("user.password_set", json!({ "via": "link" })),
        ("auth.login", json!({ "via": "link" })),
    ] {
        audit::record(
            &mut *tx,
            Event {
                actor_kind: "user",
                actor_id: &actor,
                action,
                target: None,
                details,
            },
        )
        .await?;
    }
    tx.commit().await?;
    tracing::info!(user = %id, "password set from a link");

    let cookie = session::set_cookie(&state.settings, &session);
    let user = auth::load_user(&state.pool, id, session).await?;
    Ok(([(SET_COOKIE, cookie)], Json(Me::from(&user))).into_response())
}
