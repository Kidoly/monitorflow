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
    session: SessionToken,
}

/// Sites whose data a user may read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SiteScope {
    /// No role at all.
    None,
    /// At least one global role.
    All,
    /// Only site-scoped roles, on these sites.
    Sites(Vec<Uuid>),
}

impl CurrentUser {
    /// Every role includes read access to the sites it covers.
    pub fn can_read(&self) -> bool {
        !self.roles.is_empty()
    }

    pub fn readable_sites(&self) -> SiteScope {
        if self.roles.is_empty() {
            return SiteScope::None;
        }
        if self.roles.iter().any(|grant| grant.site_id.is_none()) {
            return SiteScope::All;
        }
        let mut sites: Vec<Uuid> = self.roles.iter().filter_map(|g| g.site_id).collect();
        sites.sort();
        sites.dedup();
        SiteScope::Sites(sites)
    }

    /// Whether any role covers `site`.
    pub fn can_read_site(&self, site: Uuid) -> bool {
        self.roles.iter().any(|g| g.covers(site))
    }

    /// Whether a role that may change configuration (operator or above)
    /// covers `site`.
    pub fn can_modify_site(&self, site: Uuid) -> bool {
        self.roles
            .iter()
            .any(|g| g.role.can_modify() && g.covers(site))
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

async fn load_user(
    pool: &PgPool,
    id: Uuid,
    session: SessionToken,
) -> Result<CurrentUser, ApiError> {
    let username: String = sqlx::query_scalar("SELECT username FROM users WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    let rows: Vec<(String, Option<Uuid>)> = sqlx::query_as(
        "SELECT role, site_id FROM user_roles WHERE user_id = $1 ORDER BY role, site_id",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;
    let roles = rows
        .into_iter()
        .filter_map(|(role, site_id)| match role.parse() {
            Ok(role) => Some(RoleGrant { role, site_id }),
            Err(error) => {
                tracing::warn!(user = %id, %error, "ignoring unknown role");
                None
            }
        })
        .collect();
    Ok(CurrentUser {
        id,
        username,
        roles,
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
    password_hash: String,
    disabled: bool,
}

/// `POST /api/auth/login`.
pub async fn login(
    State(state): State<ApiState>,
    request: Result<Json<LoginRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = request.map_err(|_| ApiError::BadRequest)?;
    let account = sqlx::query_as::<_, (Uuid, String, bool)>(
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
    let stored = account.as_ref().map(|a| a.password_hash.clone());
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Role;

    fn user(roles: &[(Role, Option<Uuid>)]) -> CurrentUser {
        CurrentUser {
            id: Uuid::nil(),
            username: "u".into(),
            roles: roles
                .iter()
                .map(|&(role, site_id)| RoleGrant { role, site_id })
                .collect(),
            session: SessionToken::for_tests(),
        }
    }

    #[test]
    fn readable_sites_follow_role_grants() {
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        assert_eq!(user(&[]).readable_sites(), SiteScope::None);
        assert!(!user(&[]).can_read());
        assert_eq!(
            user(&[(Role::Viewer, Some(b)), (Role::Operator, None)]).readable_sites(),
            SiteScope::All
        );
        assert_eq!(
            user(&[
                (Role::Operator, Some(b)),
                (Role::Viewer, Some(a)),
                (Role::Viewer, Some(b))
            ])
            .readable_sites(),
            SiteScope::Sites(vec![a, b])
        );
    }

    #[test]
    fn site_checks_follow_role_grants() {
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let viewer = user(&[(Role::Viewer, Some(a))]);
        assert!(viewer.can_read_site(a) && !viewer.can_read_site(b));
        assert!(!viewer.can_modify_site(a));
        let operator = user(&[(Role::Viewer, None), (Role::Operator, Some(b))]);
        assert!(operator.can_read_site(a) && !operator.can_modify_site(a));
        assert!(operator.can_modify_site(b));
        let admin = user(&[(Role::Administrator, None)]);
        assert!(admin.can_modify_site(a) && admin.can_modify_site(b));
        assert!(!user(&[]).can_read_site(a));
    }
}
