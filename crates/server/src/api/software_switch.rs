//! Software installs turned off from the server's page (ADR-0188): the
//! signed `software_off` command, and the last one asked for, which the
//! server's page shows.
//!
//! The central server can only take the power away. Turning software
//! installs on again is the host administrator's alone, on the host:
//! `{"on": true}` is refused with the line to run there, and nothing the
//! server sends could do it, since the agent's only answer to
//! `software_off` is to stop.

use std::time::Duration;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use monitorflow_common::command;
use monitorflow_common::proto::v1::SoftwareOff;
use prost::Message;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::access::authorize;
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::commands::{parameters, CommandDetail};
use super::sensors::{body, parse_id};
use super::{ApiError, ApiState, Role};
use crate::commands::{CommandStore, NewCommand, State as CommandState};

/// How long an "off" waits for an offline server.
pub const OFF_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Body of `POST /api/servers/{id}/software`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[schema(as = SoftwareSwitchRequest)]
#[serde(deny_unknown_fields)]
pub struct SwitchRequest {
    /// `false` turns software installs off. `true` is always refused:
    /// only the server's administrator turns them on, on the server.
    pub on: bool,
}

/// The last `software_off` command of a server, in `GET
/// /api/servers/{id}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct SoftwareOffChange {
    /// The `software_off` command.
    pub command_id: Uuid,
    pub state: CommandState,
    /// The username, `null` when the account is gone.
    pub requested_by: Option<String>,
    /// When it was queued.
    pub issued_at: DateTime<Utc>,
    /// `null` until it ends.
    pub finished_at: Option<DateTime<Utc>>,
    /// Why it failed, in plain words; `null` otherwise.
    pub error: Option<String>,
}

/// A server as the route needs it.
#[derive(Debug, sqlx::FromRow)]
struct Host {
    id: Uuid,
    site_id: Uuid,
    peer_id: String,
    hostname: String,
    os_family: String,
    revoked: bool,
    /// `command_kinds.software`.
    software: Option<String>,
    /// `command_kinds.software_off`: `accepted` from agents that know it.
    off: Option<String>,
}

/// The line that turns software installs on, run on the server.
pub fn turn_on_line(os_family: &str) -> &'static str {
    if os_family == "windows" {
        "`monitorflow-agent.exe software on`, as an administrator"
    } else {
        "`sudo monitorflow-agent software on`, or the installer with MF_SOFTWARE_FROM_WEB=1"
    }
}

impl Host {
    /// Why the request is refused, if it is.
    fn refusal(&self, on: bool) -> Option<String> {
        if self.revoked {
            return Some("the server is removed".into());
        }
        if on {
            return Some(format!(
                "software installs can only be turned on on the server itself: run {} there",
                turn_on_line(&self.os_family)
            ));
        }
        if self.off.is_none() {
            return Some(
                "this server's agent cannot turn software installs off from the web interface \
                 yet; it updates by itself"
                    .into(),
            );
        }
        if self.software.as_deref() != Some("accepted") {
            return Some("software installs are already off on this server".into());
        }
        None
    }
}

/// `POST /api/servers/{id}/software`: 201 with the queued command. For
/// administrators of the server's site, its customer or everything.
pub async fn server(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<SwitchRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<CommandDetail>), ApiError> {
    let id = parse_id(&id)?;
    let host: Option<Host> = sqlx::query_as(
        "SELECT id, site_id, peer_id, hostname, os_family, revoked_at IS NOT NULL AS revoked,
             command_kinds->>'software' AS software, command_kinds->>'software_off' AS off
         FROM servers WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?;
    let site = authorize(&user, host.as_ref().map(|h| h.site_id), false)?;
    if !user.sites_with(Role::Administrator).contains(site) {
        return Err(ApiError::Forbidden);
    }
    let host = host.ok_or(ApiError::NotFound)?;
    let SwitchRequest { on } = body(request)?;
    if let Some(refusal) = host.refusal(on) {
        return Err(ApiError::Conflict(refusal));
    }
    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "UPDATE commands SET state = 'cancelled', finished_at = now(),
                error = 'replaced by a newer request'
         WHERE server_id = $1 AND kind = $2 AND state = 'queued'",
    )
    .bind(host.id)
    .bind(command::SOFTWARE_OFF)
    .execute(&mut *tx)
    .await?;
    let new = NewCommand {
        server_id: host.id,
        kind: command::SOFTWARE_OFF.to_owned(),
        payload: SoftwareOff {}.encode_to_vec(),
        ttl: OFF_TTL,
        requested_by: Some(user.id),
        campaign_id: None,
    };
    let row = CommandStore::enqueue_in(&mut *tx, &new).await?;
    let actor = user.id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: "server.software_off",
            target: Some(("server", &host.id.to_string())),
            details: json!({
                "hostname": host.hostname,
                "peer_id": host.peer_id,
                "command_id": row.id,
                "parameters": parameters(&row.kind, &row.payload),
            }),
        },
    )
    .await?;
    tx.commit().await?;
    tracing::info!(user = %user.id, command_id = %row.id, peer_id = %host.peer_id, "queued turning software installs off");
    if let Some(commands) = &state.commands {
        commands.dispatch(&host.peer_id);
    }
    Ok((StatusCode::CREATED, Json(row.into())))
}

/// The last `software_off` command of `server_id`, if any.
pub(super) async fn last_change(
    pool: &sqlx::PgPool,
    server_id: Uuid,
) -> Result<Option<SoftwareOffChange>, sqlx::Error> {
    #[allow(clippy::type_complexity)]
    let row: Option<(
        Uuid,
        CommandState,
        Option<String>,
        DateTime<Utc>,
        Option<DateTime<Utc>>,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT c.id, c.state, u.username, c.issued_at, c.finished_at, c.error
         FROM commands c LEFT JOIN users u ON u.id = c.requested_by
         WHERE c.server_id = $1 AND c.kind = $2
         ORDER BY c.issued_at DESC, c.id DESC
         LIMIT 1",
    )
    .bind(server_id)
    .bind(command::SOFTWARE_OFF)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(
        |(command_id, state, requested_by, issued_at, finished_at, error)| SoftwareOffChange {
            command_id,
            state,
            requested_by,
            issued_at,
            finished_at,
            error,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(software: Option<&str>, off: Option<&str>, os: &str) -> Host {
        Host {
            id: Uuid::nil(),
            site_id: Uuid::nil(),
            peer_id: "agent-1".into(),
            hostname: "web-1".into(),
            os_family: os.into(),
            revoked: false,
            software: software.map(Into::into),
            off: off.map(Into::into),
        }
    }

    #[test]
    fn the_web_interface_only_turns_software_off() {
        let allowed = host(Some("accepted"), Some("accepted"), "debian");
        assert_eq!(allowed.refusal(false), None);
        // On is never the central server's to give.
        assert!(allowed
            .refusal(true)
            .unwrap()
            .contains("MF_SOFTWARE_FROM_WEB=1"));
        assert!(host(Some("accepted"), Some("accepted"), "windows")
            .refusal(true)
            .unwrap()
            .contains("monitorflow-agent.exe software on"));
        assert!(host(Some("off"), Some("accepted"), "rhel")
            .refusal(false)
            .unwrap()
            .contains("already off"));
        assert!(host(Some("accepted"), None, "debian")
            .refusal(false)
            .unwrap()
            .contains("updates by itself"));
        let removed = Host {
            revoked: true,
            ..host(Some("accepted"), Some("accepted"), "debian")
        };
        assert!(removed.refusal(false).is_some());
    }
}
