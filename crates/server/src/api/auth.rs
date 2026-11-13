//! Login, logout, the current user and the CSRF guard.

use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::header::{AUTHORIZATION, SET_COOKIE};
use axum::http::request::Parts;
use axum::http::HeaderMap;
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
use super::client_addr::Peer;
use super::roles::RoleGrant;
use super::session::{self, SessionToken};
use super::{api_keys, password, v1, ApiError, ApiState};

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
    /// `None` when acting for someone without a session: the person who
    /// made a display link (ADR-0099) or an API key (ADR-0101).
    session: Option<SessionToken>,
    /// The API key the request came with, if any (ADR-0101).
    pub api_key: Option<Uuid>,
}

impl CurrentUser {
    #[cfg(test)]
    pub(super) fn for_tests(access: Access) -> Self {
        Self {
            id: Uuid::nil(),
            username: "u".into(),
            roles: Vec::new(),
            access,
            session: Some(SessionToken::for_tests()),
            api_key: None,
        }
    }

    /// The same user acting through `key` with only `roles`, already
    /// narrowed to the key (ADR-0101).
    pub(super) fn through_key(self, key: Uuid, roles: Vec<RoleGrant>, access: Access) -> Self {
        Self {
            roles,
            access,
            session: None,
            api_key: Some(key),
            ..self
        }
    }

    /// 403 for a request made with an API key. For what belongs to the
    /// person rather than to what the key reaches: their own contact, and
    /// credentials that act as them or outlive the key. v1 leaves these
    /// routes out already ([`v1::left_out`]); this holds if one is ever
    /// reached another way.
    pub(super) fn in_person(&self) -> Result<(), ApiError> {
        match self.api_key {
            Some(_) => Err(ApiError::Forbidden),
            None => Ok(()),
        }
    }
}

impl FromRequestParts<ApiState> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &ApiState) -> Result<Self, ApiError> {
        // Under `/api/v1`, a request with an `Authorization` header is a
        // key's and never falls back to the cookie (ADR-0101).
        if v1::is_v1(&parts.extensions) && parts.headers.contains_key(AUTHORIZATION) {
            return api_keys::authenticate(parts, state).await;
        }
        let token =
            session::from_headers(&parts.headers, &state.settings).ok_or(ApiError::Unauthorized)?;
        let user_id = session::resolve(&state.pool, &token, &state.settings)
            .await?
            .ok_or(ApiError::Unauthorized)?;
        load_user(&state.pool, user_id, Some(token)).await
    }
}

pub(super) async fn load_user(
    pool: &PgPool,
    id: Uuid,
    session: Option<SessionToken>,
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
        api_key: None,
    })
}

/// Rejects state-changing requests that lack [`CSRF_HEADER`] or that the
/// browser marks as coming from another site (`Sec-Fetch-Site`). A key's
/// request under `/api/v1` needs neither: it never uses the cookie, and no
/// browser adds an `Authorization` header by itself (ADR-0101).
pub async fn csrf_guard(request: Request, next: Next) -> Response {
    let with_key = v1::is_v1(request.extensions()) && request.headers().contains_key(AUTHORIZATION);
    if !request.method().is_safe() && !with_key {
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
    /// Whether the user sees more than one customer: what shows the
    /// customer picker and the customer wording in the web interface
    /// (ADR-0104). False for a single company and for a user limited to
    /// one customer.
    pub multi_customer: bool,
}

impl Me {
    pub(super) async fn of(pool: &PgPool, user: &CurrentUser) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: user.id,
            username: user.username.clone(),
            roles: user.roles.clone(),
            multi_customer: super::customers::several_visible(pool, user).await?,
        })
    }
}

struct Account {
    id: Uuid,
    /// `None` for an invited user without a password yet (ADR-0059).
    password_hash: Option<String>,
    disabled: bool,
}

/// `POST /api/auth/login`.
///
/// Attempts are limited per source address and per account before
/// anything is looked up or verified (ADR-0106). A refused attempt costs
/// no argon2 run and writes no audit row: the wrong attempt that used a
/// limit up says so in its own row.
pub async fn login(
    State(state): State<ApiState>,
    peer: Peer,
    headers: HeaderMap,
    request: Result<Json<LoginRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = request.map_err(|_| ApiError::BadRequest)?;
    let source = state.settings.trusted_proxies.source(&peer, &headers).await;
    let Some(attempt) = state.login_limits.begin(source, &request.username) else {
        tracing::debug!(%source, "login refused: too many attempts");
        return Err(ApiError::TooManyRequests(
            "Too many sign-in attempts. Wait a few minutes and try again.".into(),
        ));
    };
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
            let mut details = json!({ "reason": reason });
            if let Some(limit) = state.login_limits.failed(&attempt) {
                // The last row of this source or account until the window
                // ends: what follows is refused unrecorded.
                details["limit_reached"] = json!(limit.as_str());
                details["source"] = json!(source.to_string());
                tracing::warn!(%source, limit = limit.as_str(), "too many failed logins");
            }
            // The submitted username is not recorded: users sometimes type
            // their password into that field.
            audit::record(
                &state.pool,
                Event {
                    actor_kind: "user",
                    actor_id: &actor_id,
                    action: "auth.login_failed",
                    target: None,
                    details,
                },
            )
            .await?;
            tracing::info!(user = %actor_id, reason, "login failed");
            return Err(ApiError::InvalidCredentials);
        }
    };

    state.login_limits.succeeded(attempt);

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
    let user = load_user(&state.pool, account.id, Some(token)).await?;
    let me = Me::of(&state.pool, &user).await?;
    Ok(([(SET_COOKIE, cookie)], Json(me)).into_response())
}

/// `POST /api/auth/logout`: ends the current session.
pub async fn logout(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Response, ApiError> {
    let mut tx = state.pool.begin().await?;
    if let Some(token) = &user.session {
        session::revoke(&mut *tx, token).await?;
    }
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
pub async fn me(State(state): State<ApiState>, user: CurrentUser) -> Result<Json<Me>, ApiError> {
    Ok(Json(Me::of(&state.pool, &user).await?))
}
