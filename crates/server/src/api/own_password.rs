//! The signed-in person changes their own password (ADR-0141).
//!
//! Only from a browser session: v1 leaves `/api/me/` out
//! ([`super::v1::left_out`]) and an API key gets 403. The current password
//! is required and counts against the sign-in limits of ADR-0106, so a
//! stolen session cannot guess it faster than the login page could. A
//! change ends the person's other sessions and keeps the one it came from.

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use monitorflow_common::redact::Secret;
use serde::Deserialize;
use serde_json::json;

use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::client_addr::Peer;
use super::users::{self, CreateUserError};
use super::{password, session, ApiError, ApiState};

/// Answer to a wrong current password.
pub const WRONG_CURRENT: &str = "The current password is not right.";
/// Answer to a new password equal to the current one.
pub const UNCHANGED: &str = "The new password is the same as the current one.";

/// Body of `POST /api/me/password`.
#[derive(Deserialize)]
pub struct ChangeRequest {
    pub current_password: Secret,
    pub new_password: Secret,
}

fn invalid(error: CreateUserError) -> ApiError {
    match error {
        CreateUserError::PasswordTooShort => ApiError::Invalid(error.to_string()),
        other => ApiError::Internal(other.to_string()),
    }
}

/// `POST /api/me/password`: replaces the signed-in person's password. 204
/// on success; 400 for a short or unchanged new password or a wrong
/// current one; 429 past the sign-in limits. Audited either way.
pub async fn change(
    State(state): State<ApiState>,
    user: CurrentUser,
    peer: Peer,
    headers: HeaderMap,
    request: Result<Json<ChangeRequest>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    user.in_person()?;
    // Display links act for their maker without a session: they change nothing here.
    let current_session = user.session().ok_or(ApiError::Forbidden)?.clone();
    let Json(request) = request.map_err(|_| ApiError::BadRequest)?;
    // Cheap checks first: neither costs an argon2 run nor an attempt.
    users::validate_password(&request.new_password).map_err(invalid)?;
    if request.new_password.expose() == request.current_password.expose() {
        return Err(ApiError::Invalid(UNCHANGED.into()));
    }

    let source = state.settings.trusted_proxies.source(&peer, &headers).await;
    let Some(attempt) = state.login_limits.begin(source, &user.username) else {
        tracing::debug!(%source, "password change refused: too many attempts");
        return Err(ApiError::TooManyRequests(
            "Too many wrong passwords. Wait a few minutes and try again.".into(),
        ));
    };
    let stored: Option<String> =
        sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1")
            .bind(user.id)
            .fetch_optional(&state.pool)
            .await?
            .flatten();
    let current = request.current_password;
    let matches = tokio::task::spawn_blocking(move || {
        password::verify_or_dummy(stored.as_deref(), current.expose())
    })
    .await
    .map_err(|e| ApiError::Internal(format!("password verification task: {e}")))?;

    let actor_id = user.id.to_string();
    if !matches {
        let mut details = json!({ "reason": "bad_password" });
        if let Some(limit) = state.login_limits.failed(&attempt) {
            details["limit_reached"] = json!(limit.as_str());
            details["source"] = json!(source.to_string());
            tracing::warn!(%source, limit = limit.as_str(), "too many wrong current passwords");
        }
        audit::record(
            &state.pool,
            Event {
                actor_kind: "user",
                actor_id: &actor_id,
                action: "user.password_change_failed",
                target: None,
                details,
            },
        )
        .await?;
        tracing::info!(user = %user.id, "password change refused: wrong current password");
        return Err(ApiError::Invalid(WRONG_CURRENT.into()));
    }
    state.login_limits.succeeded(attempt);

    let hash = users::hash_password(&request.new_password)
        .await
        .map_err(invalid)?;
    let mut tx = state.pool.begin().await?;
    sqlx::query("UPDATE users SET password_hash = $2 WHERE id = $1")
        .bind(user.id)
        .bind(&hash)
        .execute(&mut *tx)
        .await?;
    let ended = session::revoke_others(&mut *tx, user.id, &current_session).await?;
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor_id,
            action: "user.password_set",
            target: None,
            details: json!({ "via": "self", "sessions_ended": ended }),
        },
    )
    .await?;
    tx.commit().await?;
    tracing::info!(user = %user.id, sessions_ended = ended, "password changed by its owner");
    Ok(StatusCode::NO_CONTENT)
}
