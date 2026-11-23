//! Runs of library scripts (ADR-0179): which servers, whether a person
//! must approve, and the signed `run_script` command each server gets.
//!
//! A run is classified when it is made, for every server by the rules of
//! its customer and site, and takes the highest tier. Then:
//!
//! - forbidden: refused, nothing is queued;
//! - asked by a person: it runs when that person may approve its tier on
//!   every server's site (approver for read-only and low impact,
//!   administrator for high impact, as for AI commands), otherwise it
//!   waits in the approval queue;
//! - started by an automation rule: read-only runs at once, low impact
//!   when every site runs low-impact AI commands without approval, the
//!   rest waits in the approval queue, exactly as an AI command would.
//!
//! A run due now or later is `scheduled`; [`dispatch`] queues its
//! commands once its time comes, classifying again so a rule added since
//! still refuses it.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use monitorflow_common::command::RUN_SCRIPT;
use monitorflow_common::proto::v1::{RunScript, ScriptParameter};
use prost::Message;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::types::Json;
use sqlx::{PgExecutor, PgPool};
use uuid::Uuid;

use super::store::{self, Script};
use super::{classify, Interpreter};
use crate::api::Role;
use crate::commands::acceptance::script_refusal;
use crate::commands::{CommandStore, Commands, NewCommand};
use crate::remediation::risk::{self, Decision, Tier};

/// Most servers of one run.
pub const MAX_SERVERS: usize = 1000;
/// A run waiting for approval longer than this expires.
pub const APPROVAL_WINDOW: chrono::Duration = chrono::Duration::hours(24);
/// How long a command waits for an offline server, beyond the script's
/// own timeout: a script meant for now should not run a day later.
pub const DELIVERY_WINDOW: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// The servers a run is for, as asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = ScriptTarget)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Target {
    /// One server or a chosen group of servers of one customer.
    Servers { server_ids: Vec<Uuid> },
    /// Every server of a site, as it is when the run is made.
    Site { site_id: Uuid },
    /// The servers of a business service (ADR-0155).
    BusinessService { business_service_id: Uuid },
}

/// A server of a run.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct TargetServer {
    pub id: Uuid,
    pub site_id: Uuid,
    pub customer_id: Uuid,
    pub os_family: String,
}

const SERVERS: &str = "SELECT s.id, s.site_id, t.customer_id, s.os_family
    FROM servers s JOIN sites t ON t.id = s.site_id";

/// The servers `target` names that exist and are not removed, by
/// hostname. Servers of a site or business service are taken as they are
/// now.
pub async fn servers_of(
    db: impl PgExecutor<'_>,
    target: &Target,
) -> Result<Vec<TargetServer>, sqlx::Error> {
    let (filter, ids): (&str, Vec<Uuid>) = match target {
        Target::Servers { server_ids } => ("s.id = ANY($1)", server_ids.clone()),
        Target::Site { site_id } => ("s.site_id = ANY($1)", vec![*site_id]),
        Target::BusinessService {
            business_service_id,
        } => (
            "s.id IN (SELECT server_id FROM business_service_members
                      WHERE service_id = ANY($1))",
            vec![*business_service_id],
        ),
    };
    // SQL-safe: interpolates only constant SQL chosen above.
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SERVERS} WHERE {filter} AND s.revoked_at IS NULL ORDER BY s.hostname, s.id"
    )))
    .bind(ids)
    .fetch_all(db)
    .await
}

/// The classification of `body` with `values` on `servers`: by the rules
/// of each server's customer and site, the highest tier, a forbidden one
/// first.
pub async fn classify_on(
    db: &PgPool,
    interpreter: Interpreter,
    body: &str,
    values: &[(String, String)],
    servers: &[TargetServer],
) -> Result<Decision, sqlx::Error> {
    let mut places: Vec<(Uuid, Uuid)> =
        servers.iter().map(|s| (s.customer_id, s.site_id)).collect();
    places.sort();
    places.dedup();
    let mut worst: Option<Decision> = None;
    for (customer, site) in places {
        let rules = risk::store::rule_set(db, customer, Some(site)).await?;
        let decision = classify(interpreter, body, values, &rules);
        if worst.as_ref().is_none_or(|w| decision.tier > w.tier) {
            worst = Some(decision);
        }
    }
    // Without servers, the built-in rules alone.
    Ok(worst.unwrap_or_else(|| classify(interpreter, body, values, &Default::default())))
}

/// Whether every site of `servers` runs low-impact AI commands without
/// approval (ADR-0045).
pub async fn low_impact_auto(db: &PgPool, servers: &[TargetServer]) -> Result<bool, sqlx::Error> {
    let mut places: Vec<(Uuid, Uuid)> =
        servers.iter().map(|s| (s.customer_id, s.site_id)).collect();
    places.sort();
    places.dedup();
    for (customer, site) in places {
        if !risk::store::low_impact_auto(db, customer, Some(site)).await? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The least role that approves a run of `tier`, as for AI commands.
pub fn approver_role(tier: Tier) -> Role {
    crate::api::approvals::approver_role(tier)
}

/// The state of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = ScriptRunState)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    AwaitingApproval,
    Scheduled,
    Dispatched,
    Rejected,
    Expired,
    Cancelled,
    Refused,
}

impl RunState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AwaitingApproval => "awaiting_approval",
            Self::Scheduled => "scheduled",
            Self::Dispatched => "dispatched",
            Self::Rejected => "rejected",
            Self::Expired => "expired",
            Self::Cancelled => "cancelled",
            Self::Refused => "refused",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        [
            Self::AwaitingApproval,
            Self::Scheduled,
            Self::Dispatched,
            Self::Rejected,
            Self::Expired,
            Self::Cancelled,
            Self::Refused,
        ]
        .into_iter()
        .find(|s| s.as_str() == text)
    }
}

/// A run to record.
pub struct NewRun<'a> {
    pub script: &'a Script,
    pub version: i32,
    pub customer: Uuid,
    pub values: &'a [(String, String)],
    pub target: &'a Target,
    pub servers: &'a [TargetServer],
    pub requested_by: Option<Uuid>,
    pub rule: Option<Uuid>,
    pub trigger: Option<Value>,
    pub decision: &'a Decision,
    pub state: RunState,
    pub scheduled_at: DateTime<Utc>,
}

/// Records `run` and its servers.
pub async fn insert(tx: &mut sqlx::PgConnection, run: &NewRun<'_>) -> Result<Uuid, sqlx::Error> {
    let parameters: BTreeMap<&str, &str> = run
        .values
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO script_runs (script_id, version, customer_id, parameters, target,
                                  requested_by, rule_id, trigger, state, tier, rule, reason,
                                  scheduled_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) RETURNING id",
    )
    .bind(run.script.id)
    .bind(run.version)
    .bind(run.customer)
    .bind(Json(&parameters))
    .bind(Json(run.target))
    .bind(run.requested_by)
    .bind(run.rule)
    .bind(run.trigger.as_ref().map(Json))
    .bind(run.state.as_str())
    .bind(run.decision.tier.as_str())
    .bind(Json(&run.decision.rule))
    .bind(&run.decision.reason)
    .bind(run.scheduled_at)
    .fetch_one(&mut *tx)
    .await?;
    let servers: Vec<Uuid> = run.servers.iter().map(|s| s.id).collect();
    sqlx::query("INSERT INTO script_run_targets (run_id, server_id) SELECT $1, unnest($2::uuid[])")
        .bind(id)
        .bind(&servers)
        .execute(&mut *tx)
        .await?;
    Ok(id)
}

#[derive(sqlx::FromRow)]
struct Claimed {
    script_id: Uuid,
    version: i32,
    parameters: Json<BTreeMap<String, String>>,
    requested_by: Option<Uuid>,
}

#[derive(sqlx::FromRow)]
struct Due {
    server_id: Uuid,
    site_id: Uuid,
    customer_id: Uuid,
    os_family: String,
    peer_id: String,
    removed: bool,
    acceptance: Option<String>,
}

/// Queues the commands of the run `id` if it is scheduled and due, in
/// one transaction; one instance claims it. Returns whether it did.
pub async fn dispatch(
    pool: &PgPool,
    commands: Option<&Commands>,
    id: Uuid,
) -> Result<bool, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let claimed: Option<Claimed> = sqlx::query_as(
        "UPDATE script_runs SET state = 'dispatched', dispatched_at = now()
         WHERE id = $1 AND state = 'scheduled' AND scheduled_at <= now()
         RETURNING script_id, version, parameters, requested_by",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(claimed) = claimed else {
        return Ok(false);
    };
    let script = store::get(&mut *tx, claimed.script_id).await?;
    let version = store::version(&mut *tx, claimed.script_id, claimed.version).await?;
    let due: Vec<Due> = sqlx::query_as(
        "SELECT t.server_id, s.site_id, st.customer_id, s.os_family, s.peer_id,
                s.revoked_at IS NOT NULL AS removed, s.command_kinds->>'run_script' AS acceptance
         FROM script_run_targets t JOIN servers s ON s.id = t.server_id
         JOIN sites st ON st.id = s.site_id
         WHERE t.run_id = $1 ORDER BY s.hostname",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    let mut peers = Vec::new();
    let mut rules = BTreeMap::new();
    for server in due {
        let error = match (&script, &version) {
            (Some(script), Some(version)) if script.deleted_at.is_none() => {
                let values: Vec<(String, String)> =
                    claimed.parameters.0.clone().into_iter().collect();
                let place = (server.customer_id, server.site_id);
                if let std::collections::btree_map::Entry::Vacant(e) = rules.entry(place) {
                    e.insert(risk::store::rule_set(&mut *tx, place.0, Some(place.1)).await?);
                }
                let decision = classify(script.interpreter, &version.body, &values, &rules[&place]);
                if server.removed {
                    Some("the server was removed".to_owned())
                } else if !script.interpreter.fits(&server.os_family) {
                    Some(format!(
                        "this server runs {}; the script is for {}",
                        server.os_family,
                        match script.interpreter {
                            Interpreter::Sh => "Linux (sh)",
                            Interpreter::PowerShell => "Windows (PowerShell)",
                        }
                    ))
                } else if decision.tier == Tier::Forbidden {
                    Some(format!("forbidden by the risk rules: {}", decision.reason))
                } else if let Some(why) =
                    script_refusal(server.acceptance.as_deref(), &server.os_family)
                {
                    Some(why.to_owned())
                } else {
                    let payload = RunScript {
                        interpreter: script.interpreter.as_str().into(),
                        body: version.body.clone(),
                        parameters: values
                            .iter()
                            .map(|(name, value)| ScriptParameter {
                                name: name.clone(),
                                value: value.clone(),
                            })
                            .collect(),
                        timeout_seconds: u32::try_from(script.timeout_s).unwrap_or(600),
                        script_id: script.id.to_string(),
                        version: u32::try_from(version.version).unwrap_or_default(),
                    };
                    let timeout = std::time::Duration::from_secs(
                        u64::try_from(script.timeout_s).unwrap_or(600),
                    );
                    let row = CommandStore::enqueue_in(
                        &mut *tx,
                        &NewCommand {
                            server_id: server.server_id,
                            kind: RUN_SCRIPT.into(),
                            payload: payload.encode_to_vec(),
                            ttl: timeout + DELIVERY_WINDOW,
                            requested_by: claimed.requested_by,
                            campaign_id: None,
                        },
                    )
                    .await?;
                    sqlx::query(
                        "UPDATE script_run_targets SET command_id = $3
                         WHERE run_id = $1 AND server_id = $2",
                    )
                    .bind(id)
                    .bind(server.server_id)
                    .bind(row.id)
                    .execute(&mut *tx)
                    .await?;
                    peers.push(server.peer_id.clone());
                    None
                }
            }
            _ => Some("the script was deleted".to_owned()),
        };
        if let Some(error) = error {
            sqlx::query(
                "UPDATE script_run_targets SET error = left($3, 1000)
                 WHERE run_id = $1 AND server_id = $2",
            )
            .bind(id)
            .bind(server.server_id)
            .bind(error)
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;
    if let Some(commands) = commands {
        for peer in peers {
            commands.dispatch(&peer);
        }
    }
    tracing::info!(run = %id, "dispatched a script run");
    Ok(true)
}

/// Runs due now, oldest first.
pub async fn due(db: impl PgExecutor<'_>, limit: i64) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT id FROM script_runs WHERE state = 'scheduled' AND scheduled_at <= now()
         ORDER BY scheduled_at, id LIMIT $1",
    )
    .bind(limit)
    .fetch_all(db)
    .await
}

/// Marks runs nobody approved within [`APPROVAL_WINDOW`] expired.
pub async fn expire(db: impl PgExecutor<'_>) -> Result<u64, sqlx::Error> {
    let done = sqlx::query(
        "UPDATE script_runs SET state = 'expired', decided_at = now()
         WHERE state = 'awaiting_approval' AND created_at < now() - $1::interval",
    )
    .bind(format!("{} hours", APPROVAL_WINDOW.num_hours()))
    .execute(db)
    .await?;
    Ok(done.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_read_as_documented() {
        let id = Uuid::nil();
        let target: Target =
            serde_json::from_value(serde_json::json!({"kind": "site", "site_id": id})).unwrap();
        assert_eq!(target, Target::Site { site_id: id });
        let target: Target =
            serde_json::from_value(serde_json::json!({"kind": "servers", "server_ids": [id]}))
                .unwrap();
        assert_eq!(
            target,
            Target::Servers {
                server_ids: vec![id]
            }
        );
        assert!(serde_json::from_value::<Target>(serde_json::json!({"kind": "all"})).is_err());
    }

    #[test]
    fn states_round_trip() {
        for state in [
            RunState::AwaitingApproval,
            RunState::Dispatched,
            RunState::Refused,
        ] {
            assert_eq!(RunState::parse(state.as_str()), Some(state));
        }
        assert_eq!(approver_role(Tier::LowImpact), Role::Approver);
        assert_eq!(approver_role(Tier::HighImpact), Role::Administrator);
    }
}
