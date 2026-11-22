//! Running AI remediation again on an alert that is still open
//! (ADR-0165): whether it can start now and why not, and starting it.
//!
//! Each run is a new remediation incident of the alert, whose first event,
//! a `note` with code `attempt`, names the person who asked; the
//! `audit_log` row `remediation.run` says it too. The loop drives it like
//! any other on its next poll. Asking needs an approver role on the
//! alert's site, the role that lets commands run there (ADR-0045).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use super::access::authorize;
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::roles::Role;
use super::sensors::{parse_id, UNIQUE_VIOLATION};
use super::{ApiError, ApiState};
use crate::commands::acceptance::{self, Refusal};
use crate::remediation::audit::{AuditError, IncidentState};
use crate::remediation::runner::retry::{
    self, AlertAttempts, Attempt, Blocked, RetryError, Trigger,
};
use crate::remediation::switch;

/// Why AI remediation cannot run on an alert now, for the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Why {
    Alert(Blocked),
    AiOff,
    Refused(Refusal),
    Role,
}

impl Why {
    fn code(self) -> &'static str {
        match self {
            Self::Alert(blocked) => blocked.code(),
            Self::AiOff => "ai_off",
            Self::Refused(_) => "commands_refused",
            Self::Role => "role",
        }
    }

    fn message(self) -> &'static str {
        match self {
            Self::Alert(blocked) => blocked.message(),
            Self::AiOff => {
                "AI remediation is off; a global administrator can turn it on under Settings, AI"
            }
            Self::Refused(refusal) => refusal.message(),
            Self::Role => "starting AI remediation needs an approver role on the server's site",
        }
    }
}

/// Body of `GET /api/remediation/alerts/{id}`. Field names and types are
/// part of the API contract: add fields, never rename or remove them.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = RemediationRunStatus)]
pub struct RunStatus {
    /// The alert, see `/api/incidents`.
    pub alert_incident_id: Uuid,
    /// Remediation incidents of the alert so far, one per attempt.
    pub attempts: i64,
    /// The latest of them; `null` when the alert never had one.
    pub latest_id: Option<Uuid>,
    pub latest_state: Option<IncidentState>,
    /// Whether the caller may start an attempt now: `blocked` is `null`.
    pub can_run: bool,
    /// Why not: `alert_resolved`, `no_agent` (a device, or the server is
    /// offline), `in_progress` (an attempt is running or waits for
    /// approval), `ai_off`, `commands_refused` (the server reports refusing
    /// AI commands, see `refusal`), or `role` (the caller is no approver on
    /// the site). `null` when it can run.
    pub blocked: Option<&'static str>,
    /// `blocked` as an English sentence.
    pub reason: Option<&'static str>,
    /// With `commands_refused`, why the server refuses: `commands_off`,
    /// `commands_off_windows`, `system_unsupported` or `agent_too_old`,
    /// the codes of remediation events (ADR-0076).
    pub refusal: Option<&'static str>,
}

/// Body of `POST /api/remediation/alerts/{id}/run`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = RemediationRunStarted)]
pub struct RunStarted {
    /// The new remediation incident, see `/api/remediation/incidents/{id}`.
    pub id: Uuid,
    pub alert_incident_id: Uuid,
    /// Its number among the alert's remediations, from 1.
    pub attempt: i64,
    /// The remediation before it, if any.
    pub previous_id: Option<Uuid>,
    /// `open`: the loop takes it on its next poll.
    pub state: IncidentState,
}

/// The alert `id` once the user reads its site.
async fn alert(
    state: &ApiState,
    user: &CurrentUser,
    id: &str,
) -> Result<(Uuid, AlertAttempts), ApiError> {
    let id = parse_id(id)?;
    let found = retry::alert(&state.pool, id).await?;
    authorize(user, found.as_ref().map(|a| a.site_id), false)?;
    Ok((id, found.ok_or(ApiError::NotFound)?))
}

/// Whether AI remediation runs: from `[ai]`, or the web interface's
/// settings with a key that opens (ADR-0064).
async fn ai_on(state: &ApiState) -> Result<bool, ApiError> {
    Ok(state.settings.ai.config.enabled || switch::wanted(&state.pool).await?.is_some())
}

/// The first reason the user cannot start an attempt on `found`, in the
/// order a person can act on them.
async fn why_not(
    state: &ApiState,
    user: &CurrentUser,
    found: &AlertAttempts,
) -> Result<Option<Why>, ApiError> {
    if let Some(blocked) = found.blocked() {
        return Ok(Some(Why::Alert(blocked)));
    }
    if !ai_on(state).await? {
        return Ok(Some(Why::AiOff));
    }
    // Said before it starts: the first command would come back refused.
    if let Some(refusal) = acceptance::shell_refusal(&state.pool, found.server_id).await? {
        return Ok(Some(Why::Refused(refusal)));
    }
    if !user.sites_with(Role::Approver).contains(found.site_id) {
        return Ok(Some(Why::Role));
    }
    Ok(None)
}

/// `GET /api/remediation/alerts/{id}`: where the alert's remediations
/// stand and whether the user may start one now.
pub async fn status(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<RunStatus>, ApiError> {
    let (id, found) = alert(&state, &user, &id).await?;
    let why = why_not(&state, &user, &found).await?;
    Ok(Json(RunStatus {
        alert_incident_id: id,
        attempts: found.attempts,
        latest_id: found.latest_id,
        latest_state: found.latest_state,
        can_run: why.is_none(),
        blocked: why.map(Why::code),
        reason: why.map(Why::message),
        refusal: match why {
            Some(Why::Refused(refusal)) => Some(refusal.code()),
            _ => None,
        },
    }))
}

/// `POST /api/remediation/alerts/{id}/run`: opens a new remediation of
/// the alert, 201. 409 with the reason when it cannot start; 403 for a
/// user who is no approver on the site.
pub async fn run(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<RunStarted>), ApiError> {
    let (id, found) = alert(&state, &user, &id).await?;
    match why_not(&state, &user, &found).await? {
        None => {}
        Some(Why::Role) => return Err(ApiError::Forbidden),
        Some(why) => return Err(ApiError::Conflict(why.message().into())),
    }
    let limits = &state.settings.remediation;
    let attempt = Attempt {
        trigger: Trigger::Person(user.id),
        after: None,
        max_commands: limits.max_commands,
        max_duration: std::time::Duration::from_secs(limits.max_duration),
    };
    let mut tx = state.pool.begin().await?;
    let opened = retry::open(&mut tx, id, attempt)
        .await
        .map_err(|e| match e {
            RetryError::NotFound => ApiError::NotFound,
            RetryError::Blocked(blocked) => ApiError::Conflict(blocked.message().into()),
            RetryError::Database(sqlx::Error::Database(e))
            | RetryError::Audit(AuditError::Database(sqlx::Error::Database(e)))
                if e.code().as_deref() == Some(UNIQUE_VIOLATION) =>
            {
                ApiError::Conflict(Blocked::InProgress.message().into())
            }
            RetryError::Database(e) | RetryError::Audit(AuditError::Database(e)) => {
                ApiError::Database(e)
            }
            RetryError::Audit(other) => ApiError::Internal(other.to_string()),
        })?;
    let actor = user.id.to_string();
    let incident = opened.id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: "remediation.run",
            target: Some(("remediation_incident", &incident)),
            details: json!({
                "alert_incident_id": id,
                "attempt": opened.attempt,
                "previous_id": opened.previous,
            }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(RunStarted {
            id: opened.id,
            alert_incident_id: id,
            attempt: opened.attempt,
            previous_id: opened.previous,
            state: IncidentState::Open,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reason_has_a_code_and_a_sentence() {
        let reasons = [
            Why::Alert(Blocked::AlertResolved),
            Why::Alert(Blocked::NoAgent),
            Why::Alert(Blocked::InProgress),
            Why::AiOff,
            Why::Refused(Refusal::Off { windows: false }),
            Why::Role,
        ];
        let codes: Vec<_> = reasons.iter().map(|w| w.code()).collect();
        assert_eq!(
            codes,
            [
                "alert_resolved",
                "no_agent",
                "in_progress",
                "ai_off",
                "commands_refused",
                "role"
            ]
        );
        assert!(Why::Refused(Refusal::Off { windows: false })
            .message()
            .contains("MF_ALLOW_COMMANDS=1"));
    }
}
