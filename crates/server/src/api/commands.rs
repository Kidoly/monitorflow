//! Commands for agents (ADR-0031): queue one for a server, list a server's
//! commands, read one with its output.
//!
//! Only `apply_updates` and `self_update` (ADR-0053) can be queued here, by administrators of the
//! server's customer, the rule of ADR-0029 for decisions that change a
//! host; source configuration has its own routes in [`super::sources`]
//! under the same rule. Every role that reads the site reads its commands.

use std::time::Duration;

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use monitorflow_common::command::{self, valid_package_name};
use monitorflow_common::proto::v1::{ApplyUpdates, ConfigureSources, SelfUpdate, ShellCommand};
use monitorflow_common::release::Version;
use prost::Message;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use super::access::{authorize, authorize_admin};
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::sensors::{body, parse_id};
use super::servers::{decode_cursor, encode_cursor, page_limit};
use super::{ApiError, ApiState};
use crate::commands::{CommandRow, CommandStore, NewCommand, State as CommandState};

pub const DEFAULT_TTL_S: u64 = 3600;
pub const MIN_TTL_S: u64 = 60;
pub const MAX_TTL_S: u64 = 7 * 24 * 3600;
/// Longest package list in one command.
pub const MAX_PACKAGES: usize = 1000;
pub const DEFAULT_LIMIT: u32 = 100;
pub const MAX_LIMIT: u32 = 500;

/// Body of `POST /api/servers/{id}/commands`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRequest {
    /// `apply_updates` or `self_update`.
    pub kind: String,
    /// `self_update`: the agent version to install.
    pub version: Option<String>,
    /// `self_update`: install even an older version.
    #[serde(default)]
    pub allow_downgrade: bool,
    /// `self_update`: seconds the new agent has to connect before the
    /// old one comes back; 30 to 3600, 300 by default.
    pub health_timeout_s: Option<u32>,
    /// Packages to upgrade; empty or absent for all.
    #[serde(default)]
    pub packages: Vec<String>,
    #[serde(default)]
    pub security_only: bool,
    #[serde(default)]
    pub allow_reboot: bool,
    /// Seconds the agent may still start it; [`MIN_TTL_S`] to
    /// [`MAX_TTL_S`], default [`DEFAULT_TTL_S`].
    pub ttl_s: Option<u64>,
}

/// One command, without its output.
#[derive(Debug, Serialize)]
pub struct CommandView {
    pub id: Uuid,
    pub server_id: Uuid,
    pub kind: String,
    /// The payload as JSON, for kinds the API knows.
    pub parameters: Value,
    pub state: CommandState,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub sent_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    pub requested_by: Option<Uuid>,
    pub campaign_id: Option<Uuid>,
}

/// `GET /api/commands/{id}`: the command and its output, each at most
/// the last 64 KiB.
#[derive(Debug, Serialize)]
pub struct CommandDetail {
    #[serde(flatten)]
    pub command: CommandView,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CommandList {
    /// Newest first.
    pub commands: Vec<CommandView>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListParams {
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

/// The payload as JSON. Keys appear by fingerprint, not armored.
pub(crate) fn parameters(kind: &str, payload: &[u8]) -> Value {
    match kind {
        command::APPLY_UPDATES => match ApplyUpdates::decode(payload) {
            Ok(p) => json!({
                "packages": p.packages,
                "security_only": p.security_only,
                "allow_reboot": p.allow_reboot,
            }),
            Err(_) => Value::Null,
        },
        command::CONFIGURE_SOURCES => match ConfigureSources::decode(payload) {
            Ok(p) => {
                let repositories: Vec<Value> = p
                    .apt
                    .iter()
                    .map(|r| json!({"mirror": r.mirror, "snapshot": r.snapshot}))
                    .chain(
                        p.dnf
                            .iter()
                            .map(|r| json!({"mirror": r.mirror, "snapshot": r.snapshot})),
                    )
                    .collect();
                json!({
                    "mirror_base_url": p.mirror_base_url,
                    "family": p.family().as_str_name(),
                    "mode": p.mode().as_str_name(),
                    "repositories": repositories,
                    "key_fingerprints": p.keys.iter().map(|k| &k.fingerprint).collect::<Vec<_>>(),
                })
            }
            Err(_) => Value::Null,
        },
        command::REVERT_SOURCES => json!({}),
        command::SELF_UPDATE => match SelfUpdate::decode(payload) {
            Ok(p) => json!({
                "version": p.version,
                "base_urls": p.base_urls,
                "allow_downgrade": p.allow_downgrade,
                "health_timeout_seconds": p.health_timeout_seconds,
            }),
            Err(_) => Value::Null,
        },
        // Queued by the remediation loop only (ADR-0043).
        command::SHELL => match ShellCommand::decode(payload) {
            Ok(p) => json!({"command": p.command, "timeout_seconds": p.timeout_seconds}),
            Err(_) => Value::Null,
        },
        _ => Value::Null,
    }
}

impl From<CommandRow> for CommandDetail {
    fn from(row: CommandRow) -> Self {
        let command = CommandView {
            parameters: parameters(&row.kind, &row.payload),
            id: row.id,
            server_id: row.server_id,
            kind: row.kind,
            state: row.state,
            issued_at: row.issued_at,
            expires_at: row.expires_at,
            sent_at: row.sent_at,
            started_at: row.started_at,
            finished_at: row.finished_at,
            exit_code: row.exit_code,
            error: row.error,
            requested_by: row.requested_by,
            campaign_id: row.campaign_id,
        };
        Self {
            command,
            stdout: row.stdout,
            stderr: row.stderr,
        }
    }
}

/// The payload of a valid `apply_updates` request, and its time to live.
fn check(request: &CreateRequest) -> Result<(Vec<u8>, Duration), ApiError> {
    if request.kind != command::APPLY_UPDATES {
        return Err(ApiError::Invalid(format!(
            "kind must be `{}` or `{}`",
            command::APPLY_UPDATES,
            command::SELF_UPDATE
        )));
    }
    if request.version.is_some() || request.allow_downgrade || request.health_timeout_s.is_some() {
        return Err(ApiError::Invalid(
            "version, allow_downgrade and health_timeout_s are for self_update".into(),
        ));
    }
    if request.packages.len() > MAX_PACKAGES {
        return Err(ApiError::Invalid(format!(
            "at most {MAX_PACKAGES} packages"
        )));
    }
    if let Some(bad) = request.packages.iter().find(|p| !valid_package_name(p)) {
        return Err(ApiError::Invalid(format!(
            "invalid package name `{}`",
            bad.escape_debug()
        )));
    }
    let ttl = ttl(request.ttl_s)?;
    let payload = ApplyUpdates {
        packages: request.packages.clone(),
        security_only: request.security_only,
        allow_reboot: request.allow_reboot,
    }
    .encode_to_vec();
    Ok((payload, ttl))
}

/// Why `name` cannot go to a server of `os_family` (ADR-0049).
pub(crate) fn unfit_message(os_family: &str, name: &str) -> String {
    let name = name.escape_debug();
    if os_family == "windows" {
        format!("`{name}` is not a KB number or update ID, which windows servers need")
    } else {
        format!("`{name}` is a windows update name, which {os_family} servers cannot take")
    }
}

/// `ttl_s` checked, [`DEFAULT_TTL_S`] when absent.
pub(super) fn ttl(ttl_s: Option<u64>) -> Result<Duration, ApiError> {
    let ttl = ttl_s.unwrap_or(DEFAULT_TTL_S);
    if !(MIN_TTL_S..=MAX_TTL_S).contains(&ttl) {
        return Err(ApiError::Invalid(format!(
            "ttl_s must be between {MIN_TTL_S} and {MAX_TTL_S}"
        )));
    }
    Ok(Duration::from_secs(ttl))
}

/// `POST /api/servers/{id}/commands`.
pub async fn create(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<CreateRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<CommandDetail>), ApiError> {
    let target = Target::find(&state, &user, &id).await?;
    let request = body(request)?;
    match request.kind.as_str() {
        command::APPLY_UPDATES => {}
        command::SELF_UPDATE => {
            let mut conn = state.pool.acquire().await?;
            let relay = crate::mirror::relays::url_for_server(&mut conn, target.id).await?;
            drop(conn);
            let (payload, ttl) = check_self_update(&state, &request, relay.as_deref())?;
            return queue(&state, &user, &target, &request.kind, payload, ttl).await;
        }
        _ => {}
    }
    let (payload, ttl) = check(&request)?;
    target.check_package_updates()?;
    if let Some(bad) = command::unfit_name(&target.os_family, &request.packages) {
        return Err(ApiError::Invalid(unfit_message(&target.os_family, bad)));
    }
    queue(&state, &user, &target, &request.kind, payload, ttl).await
}

/// The payload of a valid `self_update` request (ADR-0053): the version
/// and the sources the agent tries: its relay's mirror listener, the
/// server's cache, then the download URL.
fn check_self_update(
    state: &ApiState,
    request: &CreateRequest,
    relay_url: Option<&str>,
) -> Result<(Vec<u8>, Duration), ApiError> {
    if !request.packages.is_empty() || request.security_only || request.allow_reboot {
        return Err(ApiError::Invalid(
            "packages, security_only and allow_reboot are for apply_updates".into(),
        ));
    }
    let version: Version = request
        .version
        .as_deref()
        .ok_or_else(|| ApiError::Invalid("version is required".into()))?
        .parse()
        .map_err(|_| ApiError::Invalid("version must be like 0.2.0".into()))?;
    let health = request.health_timeout_s.unwrap_or(0);
    if health != 0 && !(MIN_HEALTH_TIMEOUT_S..=MAX_HEALTH_TIMEOUT_S).contains(&health) {
        return Err(ApiError::Invalid(format!(
            "health_timeout_s must be between {MIN_HEALTH_TIMEOUT_S} and {MAX_HEALTH_TIMEOUT_S}"
        )));
    }
    let ttl = ttl(request.ttl_s)?;
    let payload =
        state
            .settings
            .agent_releases
            .payload(&version, relay_url, request.allow_downgrade, health);
    Ok((payload, ttl))
}

/// The bounds of `health_timeout_s`, as the agent applies them.
pub const MIN_HEALTH_TIMEOUT_S: u32 = 30;
pub const MAX_HEALTH_TIMEOUT_S: u32 = 3600;

/// `GET /api/agent-releases`.
#[derive(Debug, Serialize)]
pub struct AgentReleases {
    /// Cached versions, newest first.
    pub versions: Vec<String>,
    /// The newest version without a pre-release suffix, else the newest.
    pub latest: Option<String>,
}

/// `GET /api/agent-releases`: what the server can serve to agents.
pub async fn agent_releases(
    State(state): State<ApiState>,
    _user: CurrentUser,
) -> Result<Json<AgentReleases>, ApiError> {
    let cache = state.settings.agent_releases.cache.clone();
    let versions = tokio::task::spawn_blocking(move || cache.versions())
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(AgentReleases {
        latest: crate::agent_releases::latest(&versions).map(ToString::to_string),
        versions: versions.iter().map(ToString::to_string).collect(),
    }))
}

/// Why a host without a package manager takes no update or source command.
const UNAVAILABLE: &str = "package updates aren't available on this system";

/// A server a user may queue commands for.
pub(super) struct Target {
    pub id: Uuid,
    pub peer_id: String,
    pub os_family: String,
    pub package_updates: bool,
    pub revoked: bool,
}

impl Target {
    /// The server `id` once `user` administers its customer.
    pub(super) async fn find(
        state: &ApiState,
        user: &CurrentUser,
        id: &str,
    ) -> Result<Self, ApiError> {
        let id = parse_id(id)?;
        let row: Option<(Uuid, String, String, bool, bool)> = sqlx::query_as(
            "SELECT site_id, peer_id, os_family, package_updates, revoked_at IS NOT NULL
             FROM servers WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&state.pool)
        .await?;
        authorize_admin(user, row.as_ref().map(|r| r.0))?;
        let (_, peer_id, os_family, package_updates, revoked) = row.ok_or(ApiError::NotFound)?;
        Ok(Self {
            id,
            peer_id,
            os_family,
            package_updates,
            revoked,
        })
    }

    /// Update commands need a package manager on the host (ADR-0063).
    fn check_package_updates(&self) -> Result<(), ApiError> {
        if self.package_updates {
            return Ok(());
        }
        Err(ApiError::Conflict(UNAVAILABLE.into()))
    }

    /// Source commands need apt or dnf; Windows servers take none
    /// (ADR-0049).
    pub(super) fn check_sources(&self) -> Result<(), ApiError> {
        self.check_package_updates()?;
        match self.os_family.as_str() {
            "windows" => Err(ApiError::Invalid(
                "package sources are not supported on windows servers".into(),
            )),
            _ => Ok(()),
        }
    }
}

/// Queues a checked command for `target`, audits it and wakes the
/// dispatcher: 201 with the command.
pub(super) async fn queue(
    state: &ApiState,
    user: &CurrentUser,
    target: &Target,
    kind: &str,
    payload: Vec<u8>,
    ttl: Duration,
) -> Result<(StatusCode, Json<CommandDetail>), ApiError> {
    if target.revoked {
        return Err(ApiError::Conflict("the server is revoked".into()));
    }
    let (id, peer_id) = (target.id, &target.peer_id);
    let new = NewCommand {
        server_id: id,
        kind: kind.to_owned(),
        payload,
        ttl,
        requested_by: Some(user.id),
        campaign_id: None,
    };
    let mut tx = state.pool.begin().await?;
    let row = CommandStore::enqueue_in(&mut *tx, &new).await?;
    let actor = user.id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: "command.create",
            target: Some(("command", &row.id.to_string())),
            details: json!({
                "server_id": id,
                "peer_id": peer_id,
                "kind": row.kind,
                "parameters": parameters(&row.kind, &row.payload),
                "ttl_s": ttl.as_secs(),
            }),
        },
    )
    .await?;
    tx.commit().await?;
    if let Some(commands) = &state.commands {
        commands.dispatch(peer_id);
    }
    tracing::info!(user = %user.id, command_id = %row.id, %peer_id, kind = %row.kind, "queued a command");
    Ok((StatusCode::CREATED, Json(row.into())))
}

/// `GET /api/servers/{id}/commands`.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    params: Result<Query<ListParams>, QueryRejection>,
) -> Result<Json<CommandList>, ApiError> {
    let id = parse_id(&id)?;
    let site: Option<Uuid> = sqlx::query_scalar("SELECT site_id FROM servers WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?;
    authorize(&user, site, false)?;
    let Query(params) = params.map_err(|e| ApiError::Invalid(e.body_text()))?;
    let limit = page_limit(params.limit, DEFAULT_LIMIT, MAX_LIMIT)?;
    let before: Option<(DateTime<Utc>, Uuid)> =
        params.cursor.as_deref().map(decode_cursor).transpose()?;
    let (before_at, before_id) = before.unzip();
    let mut rows = CommandStore::new(state.pool.clone())
        .list(id, before_at.zip(before_id), i64::from(limit) + 1)
        .await?;
    let next_cursor = if rows.len() > limit as usize {
        rows.truncate(limit as usize);
        rows.last().map(|r| encode_cursor(&(r.issued_at, r.id)))
    } else {
        None
    };
    let commands = rows
        .into_iter()
        .map(|row| CommandDetail::from(row).command)
        .collect();
    Ok(Json(CommandList {
        commands,
        next_cursor,
    }))
}

/// `GET /api/commands/{id}`.
pub async fn detail(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<CommandDetail>, ApiError> {
    let id = parse_id(&id)?;
    let store = CommandStore::new(state.pool.clone());
    let row = store.get(id).await?;
    let site = match &row {
        Some(row) => {
            sqlx::query_scalar("SELECT site_id FROM servers WHERE id = $1")
                .bind(row.server_id)
                .fetch_optional(&state.pool)
                .await?
        }
        None => None,
    };
    authorize(&user, site, false)?;
    Ok(Json(row.ok_or(ApiError::NotFound)?.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(value: Value) -> CreateRequest {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn requests_are_checked_and_encoded() {
        let (payload, ttl) = check(&request(json!({
            "kind": "apply_updates", "packages": ["libc6", "g++"], "security_only": true
        })))
        .unwrap();
        assert_eq!(ttl, Duration::from_secs(DEFAULT_TTL_S));
        let decoded = ApplyUpdates::decode(payload.as_slice()).unwrap();
        assert_eq!(decoded.packages, ["libc6", "g++"]);
        assert!(decoded.security_only && !decoded.allow_reboot);
        assert_eq!(
            parameters("apply_updates", &payload),
            json!({"packages": ["libc6", "g++"], "security_only": true, "allow_reboot": false})
        );
        assert_eq!(parameters("reboot", &payload), Value::Null);
        let shell = ShellCommand {
            command: "df -h".into(),
            timeout_seconds: 120,
        };
        assert_eq!(
            parameters("shell", &shell.encode_to_vec()),
            json!({"command": "df -h", "timeout_seconds": 120})
        );

        for bad in [
            json!({"kind": "shell"}),
            json!({"kind": "apply_updates", "packages": ["-o"]}),
            json!({"kind": "apply_updates", "ttl_s": 10}),
            json!({"kind": "apply_updates", "ttl_s": MAX_TTL_S + 1}),
        ] {
            assert!(matches!(check(&request(bad)), Err(ApiError::Invalid(_))));
        }
        assert!(
            serde_json::from_value::<CreateRequest>(json!({"kind": "apply_updates", "x": 1}))
                .is_err()
        );
    }
}
