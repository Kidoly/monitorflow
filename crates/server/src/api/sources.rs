//! Package source configuration (ADR-0035): point a server at mirror
//! snapshots with a `configure_sources` command, or undo it with
//! `revert_sources`. The rule of updates applies: administrators of the
//! server's customer. Mirrors are global, so any of them can be named.

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use monitorflow_common::command;
use monitorflow_common::proto::v1::configure_sources::Mode;
use monitorflow_common::proto::v1::RevertSources;
use prost::Message;
use serde::Deserialize;

use super::auth::CurrentUser;
use super::commands::{queue, ttl, CommandDetail, Target};
use super::sensors::body;
use super::{ApiError, ApiState};
use crate::commands::sources::{self, KeyLookupError, CURRENT, MAX_MIRRORS};
use crate::mirror::relays;

/// Body of `POST /api/servers/{id}/sources`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigureRequest {
    /// Mirrors of the server's family, at least one.
    pub mirrors: Vec<MirrorChoice>,
    pub mode: ModeChoice,
    /// As for other commands; see [`super::commands::DEFAULT_TTL_S`].
    pub ttl_s: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MirrorChoice {
    pub name: String,
    /// A published snapshot, or `current` (the default).
    #[serde(default = "current")]
    pub snapshot: String,
}

fn current() -> String {
    CURRENT.into()
}

/// `replace` sets the host's other sources aside, `add` keeps them
/// (ADR-0032).
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModeChoice {
    Replace,
    Add,
}

impl From<ModeChoice> for Mode {
    fn from(mode: ModeChoice) -> Self {
        match mode {
            ModeChoice::Replace => Mode::Replace,
            ModeChoice::Add => Mode::Add,
        }
    }
}

/// `POST /api/servers/{id}/sources`: 201 with the queued command.
pub async fn configure(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<ConfigureRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<CommandDetail>), ApiError> {
    let target = Target::find(&state, &user, &id).await?;
    target.check_sources()?;
    let request = body(request)?;
    let ttl = ttl(request.ttl_s)?;
    if request.mirrors.is_empty() || request.mirrors.len() > MAX_MIRRORS {
        return Err(ApiError::Invalid(format!(
            "mirrors must list 1 to {MAX_MIRRORS} mirrors"
        )));
    }
    let mirrors = state
        .mirrors
        .as_ref()
        .ok_or_else(|| ApiError::Conflict("mirrors are not enabled on this server".into()))?;
    let mut selected = Vec::with_capacity(request.mirrors.len());
    for choice in &request.mirrors {
        let found = sources::select(&state.pool, &choice.name, &choice.snapshot).await?;
        selected.push(found.map_err(ApiError::Invalid)?);
    }
    let relay = relays::for_server(&mut *state.pool.acquire().await?, target.id).await?;
    let base = sources::base_url(
        state.settings.mirror_public_url.as_deref(),
        relay.as_ref(),
        &selected,
    )
    .ok_or_else(|| ApiError::Conflict("mirror_public_url is not configured".into()))?;
    let key = sources::archive_key(mirrors.runner(), mirrors.layout())
        .await
        .map_err(|error| match error {
            KeyLookupError::Key(key) => ApiError::Conflict(key.to_string()),
            other => ApiError::Internal(other.to_string()),
        })?;
    let spec = sources::build(
        &base,
        request.mode.into(),
        &target.os_family,
        &selected,
        &key,
    )
    .map_err(ApiError::Invalid)?;
    let payload = spec.encode_to_vec();
    queue(
        &state,
        &user,
        &target,
        command::CONFIGURE_SOURCES,
        payload,
        ttl,
    )
    .await
}

/// `DELETE /api/servers/{id}/sources`: 202 with the queued
/// `revert_sources` command.
pub async fn revert(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<CommandDetail>), ApiError> {
    let target = Target::find(&state, &user, &id).await?;
    target.check_sources()?;
    let payload = RevertSources {}.encode_to_vec();
    let (_, command) = queue(
        &state,
        &user,
        &target,
        command::REVERT_SOURCES,
        payload,
        ttl(None)?,
    )
    .await?;
    Ok((StatusCode::ACCEPTED, command))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn requests_default_to_the_current_snapshot() {
        let request: ConfigureRequest = serde_json::from_value(json!({
            "mirrors": [{"name": "debian-12"}, {"name": "debian-12-security", "snapshot": "s1"}],
            "mode": "add"
        }))
        .unwrap();
        assert_eq!(request.mirrors[0].snapshot, CURRENT);
        assert_eq!(request.mirrors[1].snapshot, "s1");
        assert_eq!(Mode::from(request.mode), Mode::Add);
        for bad in [
            json!({"mirrors": [], "mode": "merge"}),
            json!({"mirrors": [{"name": "x"}]}),
            json!({"mirrors": [{"name": "x", "suite": "y"}], "mode": "add"}),
        ] {
            assert!(serde_json::from_value::<ConfigureRequest>(bad).is_err());
        }
    }
}
