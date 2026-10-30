//! Login, logout, the current user and the CSRF guard.

use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::header::SET_COOKIE;
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use monitorflow_common::redact::Secret;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use super::access::Access;
use super::audit::{self, Event};
use super::roles::RoleGrant;
use super::session::{self, SessionToken};
use super::{password, ApiError, ApiState};

/// Header the web interface sends on every state-changing request. A
/// cross-origin page cannot set it without a CORS preflight, which the API
/// never grants.
pub const CSRF_HEADER: &str = "x-monitorflow-csrf";

/// The user behind a valid session. Extracting it rejects the request with
/// 401 when there is no valid session cookie.
#[derive(Debug, Clone)]
pub struct CurrentUser {
    pub id: Uuid,
    pub username: String,
    pub roles: Vec<RoleGrant>,
    /// `roles` resolved to sites; see [`super::access`].
    pub(super) access: Access,
    session: SessionToken,
}

impl CurrentUser {
    #[cfg(test)]
    pub(super) fn for_tests(access: Access) -> Self {
        Self {
            id: Uuid::nil(),
            username: "u".into(),
            roles: Vec::new(),
            access,
            session: SessionToken::for_tests(),
        }
    }
}

impl FromRequestParts<ApiState> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &ApiState) -> Result<Self, ApiError> {
        let token =
            session::from_headers(&parts.headers, &state.settings).ok_or(ApiError::Unauthorized)?;
        let user_id = session::resolve(&state.pool, &token, &state.settings)
            .await?
            .ok_or(ApiError::Unauthorized)?;
        load_user(&state.pool, user_id, token).await
    }
}

pub(super) async fn load_user(
    pool: &PgPool,
    id: Uuid,
    session: SessionToken,
) -> Result<CurrentUser, ApiError> {
    let username: String = sqlx::query_scalar("SELECT username FROM users WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    let rows: Vec<(String, Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
        "SELECT role, customer_id, site_id FROM user_roles
         WHERE user_id = $1 ORDER BY role, customer_id, site_id",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;
    let roles: Vec<RoleGrant> = rows
        .into_iter()
        .filter_map(|(role, customer_id, site_id)| match role.parse() {
            Ok(role) => Some(RoleGrant {
                role,
                customer_id,
                site_id,
            }),
            Err(error) => {
                tracing::warn!(user = %id, %error, "ignoring unknown role");
                None
            }
        })
        .collect();
    let access = Access::load(pool, &roles).await?;
    Ok(CurrentUser {
        id,
        username,
        roles,
        access,
        session,
    })
}

/// Rejects state-changing requests that lack [`CSRF_HEADER`] or that the
/// browser marks as coming from another site (`Sec-Fetch-Site`).
pub async fn csrf_guard(request: Request, next: Next) -> Response {
    if !request.method().is_safe() {
        let headers = request.headers();
        let has_header = headers.get(CSRF_HEADER).is_some_and(|v| v == "1");
        let cross_site = headers
            .get("sec-fetch-site")
            .is_some_and(|v| v != "same-origin" && v != "none");
        if !has_header || cross_site {
            return ApiError::CrossSite.into_response();
        }
    }
    next.run(request).await
}

#[derive(Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: Secret,
}

/// Body of `GET /api/auth/me` and of a successful login.
#[derive(Debug, Serialize)]
pub struct Me {
    pub id: Uuid,
    pub username: String,
    pub roles: Vec<RoleGrant>,
}

impl From<&CurrentUser> for Me {
    fn from(user: &CurrentUser) -> Self {
        Self {
            id: user.id,
            username: user.username.clone(),
            roles: user.roles.clone(),
        }
    }
}

struct Account {
    id: Uuid,
    /// `None` for an invited user without a password yet (ADR-0059).
    password_hash: Option<String>,
    disabled: bool,
}

/// `POST /api/auth/login`.
pub async fn login(
    State(state): State<ApiState>,
    request: Result<Json<LoginRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = request.map_err(|_| ApiError::BadRequest)?;
    let account = sqlx::query_as::<_, (Uuid, Option<String>, bool)>(
        "SELECT id, password_hash, disabled_at IS NOT NULL
         FROM users WHERE lower(username) = lower($1)",
    )
    .bind(&request.username)
    .fetch_optional(&state.pool)
    .await?
    .map(|(id, password_hash, disabled)| Account {
        id,
        password_hash,
        disabled,
    });

    // Always run one argon2 verification, account or not.
    let stored = account.as_ref().and_then(|a| a.password_hash.clone());
    let password = request.password;
    let matches = tokio::task::spawn_blocking(move || {
        password::verify_or_dummy(stored.as_deref(), password.expose())
    })
    .await
    .map_err(|e| ApiError::Internal(format!("password verification task: {e}")))?;

    let account = match account {
        Some(account) if matches && !account.disabled => account,
        other => {
            let (actor_id, reason) = match &other {
                None => ("unknown".to_owned(), "unknown_user"),
                Some(a) if a.disabled => (a.id.to_string(), "disabled"),
                Some(a) if a.password_hash.is_none() => (a.id.to_string(), "no_password"),
                Some(a) => (a.id.to_string(), "bad_password"),
            };
            // The submitted username is not recorded: users sometimes type
            // their password into that field.
            audit::record(
                &state.pool,
                Event {
                    actor_kind: "user",
                    actor_id: &actor_id,
                    action: "auth.login_failed",
                    target: None,
                    details: json!({ "reason": reason }),
                },
            )
            .await?;
            tracing::info!(user = %actor_id, reason, "login failed");
            return Err(ApiError::InvalidCredentials);
        }
    };

    let mut tx = state.pool.begin().await?;
    session::prune(&mut *tx, &state.settings).await?;
    let token = session::create(&mut *tx, account.id, &state.settings).await?;
    let actor_id = account.id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor_id,
            action: "auth.login",
            target: None,
            details: json!({}),
        },
    )
    .await?;
    tx.commit().await?;
    tracing::info!(user = %account.id, "login succeeded");

    let cookie = session::set_cookie(&state.settings, &token);
    let user = load_user(&state.pool, account.id, token).await?;
    Ok(([(SET_COOKIE, cookie)], Json(Me::from(&user))).into_response())
}

/// `POST /api/auth/logout`: ends the current session.
pub async fn logout(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Response, ApiError> {
    let mut tx = state.pool.begin().await?;
    session::revoke(&mut *tx, &user.session).await?;
    let actor_id = user.id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor_id,
            action: "auth.logout",
            target: None,
            details: json!({}),
        },
    )
    .await?;
    tx.commit().await?;
    let cookie = session::clear_cookie(&state.settings);
    Ok((StatusCode::NO_CONTENT, [(SET_COOKIE, cookie)]).into_response())
}

/// `GET /api/auth/me`.
pub async fn me(user: CurrentUser) -> Json<Me> {
    Json(Me::from(&user))
}
