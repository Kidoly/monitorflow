//! AI commands turned on and off from the web interface (ADR-0098): the
//! signed `ai_commands` command for one server or every server of a site,
//! and the last one asked for, which the server's page shows.
//!
//! The agent decides: it always obeys "off", and "on" only where its host
//! allowed it with `commands.from_web`, which it reports as `ai_commands`
//! accepted (ADR-0076). These routes refuse what the report says the agent
//! would refuse, so nobody waits for a refusal.

use std::time::Duration;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use monitorflow_common::command;
use monitorflow_common::proto::v1::AiCommandsSwitch;
use prost::Message;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgConnection;
use uuid::Uuid;

use super::access::authorize;
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::commands::{parameters, CommandDetail};
use super::package_sources::{SiteChange, Skipped};
use super::sensors::{body, parse_id};
use super::{ApiError, ApiState, Role};
use crate::commands::{CommandStore, NewCommand, State as CommandState};

/// How long an "on" may still start: a server back days later must not
/// turn on by surprise.
pub const ON_TTL: Duration = Duration::from_secs(60 * 60);
/// How long an "off" waits for an offline server.
pub const OFF_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Body of both routes.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SwitchRequest {
    pub on: bool,
}

/// The last `ai_commands` command of a server, in `GET /api/servers/{id}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct AiCommandsChange {
    pub command_id: Uuid,
    pub on: bool,
    pub state: CommandState,
    /// The username, `null` when the account is gone.
    pub requested_by: Option<String>,
    pub issued_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

/// A server as these routes need it.
#[derive(Debug, sqlx::FromRow)]
struct Host {
    id: Uuid,
    site_id: Uuid,
    peer_id: String,
    hostname: String,
    os_family: String,
    revoked: bool,
    /// `command_kinds.ai_commands`: `accepted` where the host allowed
    /// "on", `off` where only "off" works, `NULL` for an older agent.
    switch: Option<String>,
    /// `command_kinds.shell`.
    shell: Option<String>,
}

const HOST_COLUMNS: &str =
    "id, site_id, peer_id, hostname, os_family, revoked_at IS NOT NULL AS revoked,
     command_kinds->>'ai_commands' AS switch, command_kinds->>'shell' AS shell";

impl Host {
    /// Why the agent would refuse `on`, if it would.
    fn refusal(&self, on: bool) -> Option<String> {
        if self.revoked {
            return Some("the server is removed".into());
        }
        match self.switch.as_deref() {
            None => Some(
                "this server's agent cannot be switched from the web interface yet; \
                 it updates by itself"
                    .into(),
            ),
            Some("accepted") => None,
            Some(_) if on => Some(not_allowed(&self.os_family).into()),
            Some(_) => None,
        }
    }

    /// Whether AI commands are already as asked, by the agent's report.
    fn already(&self, on: bool) -> bool {
        (self.shell.as_deref() == Some("accepted")) == on
    }
}

/// Why "on" is refused where the host did not allow it, with the line
/// that allows it there.
pub fn not_allowed(os_family: &str) -> &'static str {
    if os_family == "windows" {
        "this server only lets AI commands be turned on on the server itself; to allow it here, \
         run `monitorflow-agent.exe ai-commands from-web on` there as an administrator, then \
         restart the MonitorFlowAgent service"
    } else {
        "this server only lets AI commands be turned on on the server itself; to allow it here, \
         run the installer there with MF_AI_COMMANDS_FROM_WEB=1"
    }
}

/// The site `site` once `user` administers it: its own administrators,
/// its customer's, or everything's, who approve high-impact commands there
/// (ADR-0043). 404 for a site the user cannot see, 403 for one it reads.
fn authorize_site_admin(user: &CurrentUser, site: Option<Uuid>) -> Result<Uuid, ApiError> {
    let site = authorize(user, site, false)?;
    if !user.sites_with(Role::Administrator).contains(site) {
        return Err(ApiError::Forbidden);
    }
    Ok(site)
}

/// `POST /api/servers/{id}/ai-commands`: 201 with the queued command.
pub async fn server(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<SwitchRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<CommandDetail>), ApiError> {
    let id = parse_id(&id)?;
    // SQL-safe: interpolates only the constant HOST_COLUMNS.
    let host: Option<Host> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {HOST_COLUMNS} FROM servers WHERE id = $1"
    )))
    .bind(id)
    .fetch_optional(&state.pool)
    .await?;
    authorize_site_admin(&user, host.as_ref().map(|h| h.site_id))?;
    let host = host.ok_or(ApiError::NotFound)?;
    let SwitchRequest { on } = body(request)?;
    if let Some(refusal) = host.refusal(on) {
        return Err(ApiError::Conflict(refusal));
    }
    let mut tx = state.pool.begin().await?;
    let detail = queue(&mut tx, &user, &host, on).await?;
    tx.commit().await?;
    if let Some(commands) = &state.commands {
        commands.dispatch(&host.peer_id);
    }
    Ok((StatusCode::CREATED, Json(detail)))
}

/// `POST /api/sites/{id}/ai-commands`: the same for every server of the
/// site whose AI commands are not as asked; each is queued or skipped
/// alone, with why.
pub async fn site(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<SwitchRequest>, JsonRejection>,
) -> Result<Json<SiteChange>, ApiError> {
    let id = parse_id(&id)?;
    let site: Option<Uuid> = sqlx::query_scalar("SELECT id FROM sites WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?;
    let site = authorize_site_admin(&user, site)?;
    let SwitchRequest { on } = body(request)?;
    // SQL-safe: interpolates only the constant HOST_COLUMNS.
    let hosts: Vec<Host> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {HOST_COLUMNS} FROM servers
         WHERE site_id = $1 AND revoked_at IS NULL ORDER BY hostname, peer_id"
    )))
    .bind(site)
    .fetch_all(&state.pool)
    .await?;
    let mut change = SiteChange {
        queued: Vec::new(),
        skipped: Vec::new(),
    };
    let mut tx = state.pool.begin().await?;
    for host in hosts.iter().filter(|h| !h.already(on)) {
        match host.refusal(on) {
            Some(reason) => change.skipped.push(Skipped {
                id: host.id,
                hostname: host.hostname.clone(),
                reason,
            }),
            None => {
                queue(&mut tx, &user, host, on).await?;
                change.queued.push(host.id);
            }
        }
    }
    tx.commit().await?;
    if let Some(commands) = &state.commands {
        for host in hosts.iter().filter(|h| change.queued.contains(&h.id)) {
            commands.dispatch(&host.peer_id);
        }
    }
    tracing::info!(user = %user.id, %site, on, queued = change.queued.len(), skipped = change.skipped.len(), "switched the AI commands of a site");
    Ok(Json(change))
}

/// Queues the switch of `host`, cancelling an older one still queued so
/// the last request wins, then audits and notifies, all in `tx`.
async fn queue(
    tx: &mut PgConnection,
    user: &CurrentUser,
    host: &Host,
    on: bool,
) -> Result<CommandDetail, ApiError> {
    sqlx::query(
        "UPDATE commands SET state = 'cancelled', finished_at = now(),
                error = 'replaced by a newer request'
         WHERE server_id = $1 AND kind = $2 AND state = 'queued'",
    )
    .bind(host.id)
    .bind(command::AI_COMMANDS)
    .execute(&mut *tx)
    .await?;
    let new = NewCommand {
        server_id: host.id,
        kind: command::AI_COMMANDS.to_owned(),
        payload: AiCommandsSwitch { on }.encode_to_vec(),
        ttl: if on { ON_TTL } else { OFF_TTL },
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
            action: "server.ai_commands",
            target: Some(("server", &host.id.to_string())),
            details: json!({
                "hostname": host.hostname,
                "peer_id": host.peer_id,
                "on": on,
                "command_id": row.id,
                "parameters": parameters(&row.kind, &row.payload),
            }),
        },
    )
    .await?;
    sqlx::query(
        "INSERT INTO notification_outbox (event, customer_id, details)
         SELECT 'ai_commands.changed', p.customer_id, jsonb_build_object(
             'occurred_at', now(),
             'customer', p.customer,
             'site', p.site,
             'server', notification_ref(s.id, s.hostname),
             'ai_commands', jsonb_build_object('on', $2::boolean, 'requested_by', $3::text,
                                               'command_id', $4::uuid))
         FROM servers s, notification_place(s.id, NULL) p
         WHERE s.id = $1",
    )
    .bind(host.id)
    .bind(on)
    .bind(&user.username)
    .bind(row.id)
    .execute(&mut *tx)
    .await?;
    tracing::info!(user = %user.id, command_id = %row.id, peer_id = %host.peer_id, on, "queued an AI commands switch");
    Ok(row.into())
}

/// A row of `commands` for [`last_change`].
#[derive(sqlx::FromRow)]
struct ChangeRow {
    id: Uuid,
    payload: Vec<u8>,
    state: CommandState,
    username: Option<String>,
    issued_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    error: Option<String>,
}

/// The last `ai_commands` command of `server_id`, if any.
pub(super) async fn last_change(
    pool: &sqlx::PgPool,
    server_id: Uuid,
) -> Result<Option<AiCommandsChange>, sqlx::Error> {
    let row: Option<ChangeRow> = sqlx::query_as(
        "SELECT c.id, c.payload, c.state, u.username, c.issued_at, c.finished_at, c.error
         FROM commands c LEFT JOIN users u ON u.id = c.requested_by
         WHERE c.server_id = $1 AND c.kind = $2
         ORDER BY c.issued_at DESC, c.id DESC
         LIMIT 1",
    )
    .bind(server_id)
    .bind(command::AI_COMMANDS)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|row| AiCommandsChange {
        command_id: row.id,
        on: AiCommandsSwitch::decode(row.payload.as_slice()).is_ok_and(|p| p.on),
        state: row.state,
        requested_by: row.username,
        issued_at: row.issued_at,
        finished_at: row.finished_at,
        error: row.error,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(switch: Option<&str>, shell: Option<&str>) -> Host {
        Host {
            id: Uuid::nil(),
            site_id: Uuid::nil(),
            peer_id: "agent-1".into(),
            hostname: "web-1".into(),
            os_family: "debian".into(),
            revoked: false,
            switch: switch.map(Into::into),
            shell: shell.map(Into::into),
        }
    }

    #[test]
    fn off_works_wherever_the_agent_knows_it_and_on_only_with_consent() {
        let consent = host(Some("accepted"), Some("off"));
        assert_eq!(consent.refusal(true), None);
        assert_eq!(consent.refusal(false), None);
        let local = host(Some("off"), Some("accepted"));
        assert_eq!(local.refusal(false), None);
        assert!(local
            .refusal(true)
            .unwrap()
            .contains("MF_AI_COMMANDS_FROM_WEB=1"));
        let old = host(None, Some("accepted"));
        assert!(old.refusal(false).unwrap().contains("updates by itself"));
        let windows = Host {
            os_family: "windows".into(),
            ..host(Some("off"), None)
        };
        assert!(windows.refusal(true).unwrap().contains("from-web on"));
        let removed = Host {
            revoked: true,
            ..host(Some("accepted"), None)
        };
        assert!(removed.refusal(false).is_some());
    }

    #[test]
    fn a_site_skips_servers_already_as_asked() {
        assert!(host(None, Some("accepted")).already(true));
        assert!(!host(None, Some("accepted")).already(false));
        // Unknown counts as not on: "off" is still sent.
        assert!(!host(None, None).already(true));
        assert!(host(None, None).already(false));
    }
}
