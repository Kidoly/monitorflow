//! The approval queue (architecture §8 Approval, ADR-0045): commands the
//! risk rules parked for a human, and the approvers' decisions on them.
//!
//! Everyone who reads an incident's site sees its pending command. Low
//! impact commands need an approver or administrator role on the site,
//! high impact ones an administrator role; any approver may reject. A
//! decision is [`approval::decide`], which resumes the loop, and an
//! `audit_log` row, in one transaction.

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::access::authorize;
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::proxmox::SnapshotSupport;
use super::roles::Role;
use super::sensors::{body, parse_id};
use super::{ApiError, ApiState};
use crate::remediation::approval::{self, check_command, check_reason, DecideError, Verdict};
use crate::remediation::audit::AuditError;
use crate::remediation::risk::{self, Tier};
use crate::remediation::snapshot::{Support, VmRef};

/// Most items of one listing. Every awaiting incident has at most one.
pub const MAX_ITEMS: i64 = 500;

/// The least role that may approve a command of `tier` (ADR-0045).
pub fn approver_role(tier: Tier) -> Role {
    match tier {
        Tier::ReadOnly | Tier::LowImpact => Role::Approver,
        Tier::HighImpact | Tier::Forbidden => Role::Administrator,
    }
}

/// Query string of `GET /api/remediation/approvals`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListParams {
    pub customer_id: Option<Uuid>,
    pub site_id: Option<Uuid>,
    pub server_id: Option<Uuid>,
}

#[derive(Debug, sqlx::FromRow)]
struct Row {
    id: Uuid,
    incident_id: Uuid,
    customer_id: Uuid,
    customer_name: String,
    site_id: Uuid,
    site_name: String,
    server_id: Uuid,
    hostname: String,
    alert_incident_id: Option<Uuid>,
    metric: String,
    instance: String,
    command: String,
    diagnosis: String,
    reasoning: String,
    tier: String,
    rule: serde_json::Value,
    reason: String,
    commands_run: i64,
    max_commands: i32,
    opened_at: DateTime<Utc>,
    proposed_at: DateTime<Utc>,
    deadline: DateTime<Utc>,
    cluster: Option<String>,
    node: Option<String>,
    vmid: Option<i32>,
}

/// One command waiting for approval. Field names and types are part of
/// the API contract: add fields, never rename or remove them.
#[derive(Debug, Serialize)]
pub struct PendingView {
    /// The pending command; decisions go to `/approvals/{id}/...`.
    pub id: Uuid,
    pub incident_id: Uuid,
    pub customer_id: Uuid,
    pub customer_name: String,
    pub site_id: Uuid,
    pub site_name: String,
    pub server_id: Uuid,
    pub hostname: String,
    pub alert_incident_id: Option<Uuid>,
    pub metric: String,
    pub instance: String,
    /// The proposed command, exactly as it would run.
    pub command: String,
    /// The AI's text: untrusted, shown as data.
    pub diagnosis: String,
    pub reasoning: String,
    /// `low_impact` or `high_impact`, from the rules, never from the AI.
    pub tier: Tier,
    /// `{"source": "builtin" | "configured", "id"}`.
    pub rule: serde_json::Value,
    pub reason: String,
    /// Commands the incident ran so far, and its limit.
    pub commands_run: i64,
    pub max_commands: i32,
    pub opened_at: DateTime<Utc>,
    pub proposed_at: DateTime<Utc>,
    /// `opened_at + max_duration_s`: the incident escalates then, and a
    /// decision after it is 409.
    pub deadline: DateTime<Utc>,
    /// Whether the user may approve this command as proposed, and reject
    /// it. UX hints: the decision routes check again.
    pub can_approve: bool,
    pub can_reject: bool,
    /// Whether a change on this server is snapshotted first (ADR-0044);
    /// when not, rollback is limited.
    pub snapshots: SnapshotSupport,
}

/// Body of `GET /api/remediation/approvals`.
#[derive(Debug, Serialize)]
pub struct PendingList {
    /// Nearest deadline first.
    pub approvals: Vec<PendingView>,
}

/// Body of `POST .../approve`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApproveRequest {
    /// Runs this instead of the proposal, recorded as an `edit`.
    pub command: Option<String>,
}

/// Body of `POST .../reject`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RejectRequest {
    /// Sent back to the AI with the rejection, and recorded.
    pub reason: String,
}

/// Answer of both decision routes.
#[derive(Debug, Serialize)]
pub struct DecisionView {
    pub id: Uuid,
    pub incident_id: Uuid,
    /// `approved` or `rejected`.
    pub state: &'static str,
    /// What will run, or what was rejected.
    pub command: String,
    pub edited: bool,
}

/// `GET /api/remediation/approvals`: the pending commands of the user's
/// sites.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
    params: Result<Query<ListParams>, QueryRejection>,
) -> Result<Json<PendingList>, ApiError> {
    let Query(params) = params.map_err(|r| ApiError::Invalid(r.body_text()))?;
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let (all_sites, sites) = user.readable_sites().bind();
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT p.id, p.incident_id, i.customer_id, c.name AS customer_name, s.site_id,
                si.name AS site_name, i.server_id, s.hostname, i.alert_incident_id, i.metric,
                i.instance, p.command, p.diagnosis, p.reasoning, p.tier, p.rule, p.reason,
                (SELECT count(*) FROM remediation_incident_events e
                 WHERE e.incident_id = i.id AND e.kind = 'command_queued') AS commands_run,
                i.max_commands, i.opened_at, p.created_at AS proposed_at,
                i.opened_at + make_interval(secs => i.max_duration_s) AS deadline,
                v.cluster, v.node, v.vmid
         FROM remediation_pending_commands p
         JOIN remediation_incidents i ON i.id = p.incident_id
         JOIN servers s ON s.id = i.server_id
         JOIN sites si ON si.id = s.site_id
         JOIN customers c ON c.id = i.customer_id
         LEFT JOIN server_proxmox_vms v ON v.server_id = i.server_id
         WHERE p.state = 'pending' AND i.state = 'awaiting_approval'
           AND ($1 OR s.site_id = ANY($2))
           AND ($3::uuid IS NULL OR i.customer_id = $3)
           AND ($4::uuid IS NULL OR s.site_id = $4)
           AND ($5::uuid IS NULL OR i.server_id = $5)
         ORDER BY deadline, p.id
         LIMIT $6",
    )
    .bind(all_sites)
    .bind(&sites)
    .bind(params.customer_id)
    .bind(params.site_id)
    .bind(params.server_id)
    .bind(MAX_ITEMS)
    .fetch_all(&state.pool)
    .await?;
    let settings = &state.settings;
    let rejecters = user.sites_with(Role::Approver);
    let approvals = rows
        .into_iter()
        .map(|row| {
            let tier = parse_tier(&row.tier)?;
            let mapping = match (row.cluster, row.node, row.vmid) {
                (Some(cluster), Some(node), Some(vmid)) => Some(VmRef {
                    cluster,
                    node,
                    vmid: u32::try_from(vmid).unwrap_or_default(),
                }),
                _ => None,
            };
            let support = Support::of(
                settings.proxmox_enabled,
                &settings.proxmox_clusters,
                mapping,
            );
            Ok(PendingView {
                id: row.id,
                incident_id: row.incident_id,
                customer_id: row.customer_id,
                customer_name: row.customer_name,
                site_id: row.site_id,
                site_name: row.site_name,
                server_id: row.server_id,
                hostname: row.hostname,
                alert_incident_id: row.alert_incident_id,
                metric: row.metric,
                instance: row.instance,
                command: row.command,
                diagnosis: row.diagnosis,
                reasoning: row.reasoning,
                tier,
                rule: row.rule,
                reason: row.reason,
                commands_run: row.commands_run,
                max_commands: row.max_commands,
                opened_at: row.opened_at,
                proposed_at: row.proposed_at,
                deadline: row.deadline,
                can_approve: user.sites_with(approver_role(tier)).contains(row.site_id),
                can_reject: rejecters.contains(row.site_id),
                snapshots: SnapshotSupport {
                    available: support.is_available(),
                    reason: support.reason(),
                },
            })
        })
        .collect::<Result<_, ApiError>>()?;
    Ok(Json(PendingList { approvals }))
}

/// `POST /api/remediation/approvals/{id}/approve`: runs the command, or
/// the edited `command`. Editing never lowers the role needed: it is the
/// higher of the two commands' tiers.
pub async fn approve(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<ApproveRequest>, JsonRejection>,
) -> Result<Json<DecisionView>, ApiError> {
    let target = target(&state, &user, &id).await?;
    let request = body(request)?;
    let edited = request.command.filter(|c| *c != target.command);
    let mut tier = target.tier;
    if let Some(command) = &edited {
        check_command(command).map_err(|m| ApiError::Invalid(m.into()))?;
        let rules =
            risk::store::rule_set(&state.pool, target.customer_id, Some(target.site_id)).await?;
        let decision = risk::classify(command, &rules);
        if decision.tier == Tier::Forbidden {
            return Err(ApiError::Invalid(format!(
                "the edited command is forbidden: {}",
                decision.reason
            )));
        }
        tier = tier.max(decision.tier);
    }
    if !user
        .sites_with(approver_role(tier))
        .contains(target.site_id)
    {
        return Err(ApiError::Forbidden);
    }
    decide(
        &state,
        &user,
        target,
        tier,
        Verdict::Approve { edited },
        "remediation.approve",
    )
    .await
}

/// `POST /api/remediation/approvals/{id}/reject`: drops the command with
/// a reason, which the AI gets back.
pub async fn reject(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<RejectRequest>, JsonRejection>,
) -> Result<Json<DecisionView>, ApiError> {
    let target = target(&state, &user, &id).await?;
    let request = body(request)?;
    let reason = request.reason.trim().to_owned();
    check_reason(&reason).map_err(|m| ApiError::Invalid(m.into()))?;
    if !user.sites_with(Role::Approver).contains(target.site_id) {
        return Err(ApiError::Forbidden);
    }
    let tier = target.tier;
    decide(
        &state,
        &user,
        target,
        tier,
        Verdict::Reject { reason },
        "remediation.reject",
    )
    .await
}

/// What the decision routes need of a pending command before deciding.
struct Target {
    id: Uuid,
    customer_id: Uuid,
    site_id: Uuid,
    command: String,
    tier: Tier,
}

/// The pending command `id`, decided or not, if the user reads its site.
async fn target(state: &ApiState, user: &CurrentUser, id: &str) -> Result<Target, ApiError> {
    let id = parse_id(id)?;
    let row: Option<(Uuid, Uuid, String, String)> = sqlx::query_as(
        "SELECT i.customer_id, s.site_id, p.command, p.tier
         FROM remediation_pending_commands p
         JOIN remediation_incidents i ON i.id = p.incident_id
         JOIN servers s ON s.id = i.server_id
         WHERE p.id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?;
    authorize(user, row.as_ref().map(|r| r.1), false)?;
    let (customer_id, site_id, command, tier) = row.ok_or(ApiError::NotFound)?;
    Ok(Target {
        id,
        customer_id,
        site_id,
        command,
        tier: parse_tier(&tier)?,
    })
}

async fn decide(
    state: &ApiState,
    user: &CurrentUser,
    target: Target,
    tier: Tier,
    verdict: Verdict,
    action: &str,
) -> Result<Json<DecisionView>, ApiError> {
    let mut tx = state.pool.begin().await?;
    let decided = approval::decide(&mut tx, target.id, user.id, &verdict)
        .await
        .map_err(|e| match e {
            DecideError::NotFound => ApiError::NotFound,
            DecideError::Conflict(message) => ApiError::Conflict(message.into()),
            DecideError::Audit(AuditError::Database(e)) | DecideError::Database(e) => {
                ApiError::Database(e)
            }
            DecideError::Audit(other) => ApiError::Internal(other.to_string()),
        })?;
    let actor = user.id.to_string();
    let incident = decided.incident_id.to_string();
    // Commands and reasons are in the incident's chain; the summary row
    // only says who decided what.
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action,
            target: Some(("remediation_incident", &incident)),
            details: json!({
                "pending_command_id": target.id,
                "tier": tier,
                "edited": decided.edited,
            }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(Json(DecisionView {
        id: target.id,
        incident_id: decided.incident_id,
        state: match verdict {
            Verdict::Approve { .. } => "approved",
            Verdict::Reject { .. } => "rejected",
        },
        command: decided.command,
        edited: decided.edited,
    }))
}

fn parse_tier(text: &str) -> Result<Tier, ApiError> {
    Tier::parse(text).ok_or_else(|| ApiError::Internal(format!("unknown tier {text}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn high_impact_needs_an_administrator() {
        assert_eq!(approver_role(Tier::ReadOnly), Role::Approver);
        assert_eq!(approver_role(Tier::LowImpact), Role::Approver);
        assert_eq!(approver_role(Tier::HighImpact), Role::Administrator);
        assert_eq!(approver_role(Tier::Forbidden), Role::Administrator);
        // Operators rank below approvers.
        assert!(Role::Operator < approver_role(Tier::LowImpact));
    }

    #[test]
    fn bodies_refuse_unknown_fields() {
        let approve: ApproveRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(approve.command, None);
        assert!(serde_json::from_str::<ApproveRequest>(r#"{"tier": "read_only"}"#).is_err());
        assert!(serde_json::from_str::<RejectRequest>("{}").is_err());
    }
}
