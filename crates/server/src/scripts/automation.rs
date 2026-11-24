//! Automation rules (ADR-0189): run a library script on a server when
//! something happens there.
//!
//! Rules read the events integrations already get (ADR-0102), which every
//! change writes in its own transaction: `incident.opened` for
//! `incident_opened` and `check_failed`, `server.added` (not a server
//! coming back after a removal) for `server_joined`. Each event a rule
//! takes is recorded in `automation_firings` first, so one instance of a
//! cluster takes it once. Events older than [`LOOKBACK`] or older than
//! the rule are never taken: a rule does not replay the past.
//!
//! A run made by a rule is classified as an AI command would be, and
//! approved the same way (see [`super::run`]). Two limits keep a rule from
//! flooding: at most one run per server within the rule's cooldown, and
//! at most [`MAX_RUNS_PER_HOUR`] runs of a rule in an hour.

use std::collections::BTreeMap;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::types::Json;
use sqlx::PgPool;
use uuid::Uuid;

use super::run::{self, NewRun, RunState, Target, TargetServer};
use super::store;
use crate::api::audit::{self, Event};
use crate::commands::Commands;
use crate::remediation::risk::Tier;

/// How far back events are taken.
pub const LOOKBACK: chrono::Duration = chrono::Duration::hours(1);
/// Most runs one rule makes in an hour; more events are skipped.
pub const MAX_RUNS_PER_HOUR: i64 = 50;
/// Most events taken by one pass.
const BATCH: i64 = 100;

/// What starts a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = AutomationEvent)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// An incident opens on a server, of any metric or of one.
    IncidentOpened,
    /// An incident opens on one check (its metric) of a server.
    CheckFailed,
    /// A server joins: enrolled for the first time.
    ServerJoined,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::IncidentOpened => "incident_opened",
            Self::CheckFailed => "check_failed",
            Self::ServerJoined => "server_joined",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        [Self::IncidentOpened, Self::CheckFailed, Self::ServerJoined]
            .into_iter()
            .find(|e| e.as_str() == text)
    }
}

/// Whether `metric` is a metric name a rule can name.
pub fn valid_metric(metric: &str) -> bool {
    let mut chars = metric.chars();
    metric.len() <= 200
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == ':')
        && chars.all(|c| c.is_ascii_alphanumeric() || "_:.".contains(c))
}

#[derive(sqlx::FromRow)]
struct Candidate {
    rule_id: Uuid,
    rule_name: String,
    script_id: Uuid,
    parameters: Json<BTreeMap<String, String>>,
    cooldown_minutes: i32,
    event_id: Uuid,
    event: String,
    server_id: Uuid,
    site_id: Uuid,
    customer_id: Uuid,
    os_family: String,
    incident_id: Option<String>,
    metric: Option<String>,
}

/// Takes the events rules wait for and makes their runs; returns how many
/// runs it made.
pub async fn fire(pool: &PgPool, commands: Option<&Commands>) -> Result<usize, sqlx::Error> {
    let candidates: Vec<Candidate> = sqlx::query_as(
        "SELECT r.id AS rule_id, r.name AS rule_name, r.script_id, r.parameters,
                r.cooldown_minutes, e.id AS event_id, e.event, s.id AS server_id, s.site_id,
                st.customer_id, s.os_family, e.details #>> '{incident,id}' AS incident_id,
                e.details #>> '{incident,metric}' AS metric
         FROM integration_events e
         JOIN servers s ON s.id = (e.details #>> '{server,id}')::uuid
         JOIN sites st ON st.id = s.site_id
         JOIN automation_rules r ON r.enabled
              AND e.created_at >= r.created_at
              AND ((r.customer_id IS NULL AND r.site_id IS NULL)
                   OR r.customer_id = st.customer_id OR r.site_id = s.site_id)
              AND ((r.event IN ('incident_opened', 'check_failed')
                    AND e.event = 'incident.opened'
                    AND coalesce(r.metric = e.details #>> '{incident,metric}',
                                 e.details #>> '{incident,metric}' <> 'offline'))
                   OR (r.event = 'server_joined' AND e.event = 'server.added'
                       AND NOT coalesce((e.details ->> 'restored')::boolean, false)))
         WHERE e.created_at > now() - $1::interval
           AND e.event IN ('incident.opened', 'server.added')
           AND s.revoked_at IS NULL
           AND NOT EXISTS (SELECT 1 FROM automation_firings f
                           WHERE f.rule_id = r.id AND f.event_id = e.id)
         ORDER BY e.seq LIMIT $2",
    )
    .bind(format!("{} minutes", LOOKBACK.num_minutes()))
    .bind(BATCH)
    .fetch_all(pool)
    .await?;
    let mut made = 0;
    for candidate in candidates {
        match take(pool, commands, &candidate).await {
            Ok(true) => made += 1,
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(rule = %candidate.rule_id, %error, "an automation rule failed")
            }
        }
    }
    Ok(made)
}

/// Records that the rule took the event, and makes its run unless a limit
/// or a check skips it. True when it made one.
async fn take(
    pool: &PgPool,
    commands: Option<&Commands>,
    c: &Candidate,
) -> Result<bool, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let first: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO automation_firings (rule_id, event_id, server_id) VALUES ($1, $2, $3)
         ON CONFLICT DO NOTHING RETURNING rule_id",
    )
    .bind(c.rule_id)
    .bind(c.event_id)
    .bind(c.server_id)
    .fetch_optional(&mut *tx)
    .await?;
    if first.is_none() {
        // Another instance took it.
        return Ok(false);
    }
    let (recent, last_hour): (bool, i64) = sqlx::query_as(
        "SELECT
           EXISTS (SELECT 1 FROM automation_firings WHERE rule_id = $1 AND server_id = $2
                   AND run_id IS NOT NULL
                   AND created_at > now() - make_interval(mins => $3)),
           (SELECT count(*) FROM automation_firings WHERE rule_id = $1 AND run_id IS NOT NULL
            AND created_at > now() - interval '1 hour')",
    )
    .bind(c.rule_id)
    .bind(c.server_id)
    .bind(c.cooldown_minutes)
    .fetch_one(&mut *tx)
    .await?;
    let script = store::get(&mut *tx, c.script_id).await?;
    let skipped = match &script {
        _ if recent => Some(format!(
            "the rule ran on this server less than {} minutes ago",
            c.cooldown_minutes
        )),
        _ if last_hour >= MAX_RUNS_PER_HOUR => Some(format!(
            "the rule made {MAX_RUNS_PER_HOUR} runs in the last hour"
        )),
        None => Some("the script does not exist".into()),
        Some(s) if s.deleted_at.is_some() => Some("the script was deleted".into()),
        Some(s) if s.customer_id.is_some_and(|own| own != c.customer_id) => {
            Some("the script belongs to another customer".into())
        }
        Some(s) if !s.interpreter.fits(&c.os_family) => {
            Some(format!("the script does not run on {}", c.os_family))
        }
        Some(_) => None,
    };
    let script = match (skipped, script) {
        (None, Some(script)) => script,
        (why, _) => {
            let why = why.unwrap_or_default();
            sqlx::query(
                "UPDATE automation_firings SET skipped = left($3, 1000)
                 WHERE rule_id = $1 AND event_id = $2",
            )
            .bind(c.rule_id)
            .bind(c.event_id)
            .bind(&why)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            tracing::info!(rule = %c.rule_id, server = %c.server_id, reason = %why, "automation rule skipped an event");
            return Ok(false);
        }
    };
    let values = match super::values(&script.parameters, &c.parameters.0) {
        Ok(values) => values,
        Err(why) => {
            sqlx::query(
                "UPDATE automation_firings SET skipped = left($3, 1000)
                 WHERE rule_id = $1 AND event_id = $2",
            )
            .bind(c.rule_id)
            .bind(c.event_id)
            .bind(&why)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(false);
        }
    };
    let servers = [TargetServer {
        id: c.server_id,
        site_id: c.site_id,
        customer_id: c.customer_id,
        os_family: c.os_family.clone(),
    }];
    let decision =
        run::classify_on(pool, script.interpreter, &script.body, &values, &servers).await?;
    let state = match decision.tier {
        Tier::Forbidden => RunState::Refused,
        Tier::ReadOnly => RunState::Scheduled,
        Tier::LowImpact if run::low_impact_auto(pool, &servers).await? => RunState::Scheduled,
        Tier::LowImpact | Tier::HighImpact => RunState::AwaitingApproval,
    };
    let target = Target::Servers {
        server_ids: vec![c.server_id],
    };
    let trigger = json!({
        "event": c.event,
        "event_id": c.event_id,
        "incident_id": c.incident_id,
        "metric": c.metric,
    });
    let id = run::insert(
        &mut tx,
        &NewRun {
            script: &script,
            version: script.version,
            customer: c.customer_id,
            values: &values,
            target: &target,
            servers: &servers,
            requested_by: None,
            rule: Some(c.rule_id),
            trigger: Some(trigger.clone()),
            decision: &decision,
            state,
            scheduled_at: Utc::now(),
        },
    )
    .await?;
    sqlx::query("UPDATE automation_firings SET run_id = $3 WHERE rule_id = $1 AND event_id = $2")
        .bind(c.rule_id)
        .bind(c.event_id)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "system",
            actor_id: "automation",
            action: "script.run",
            target: Some(("script_run", &id.to_string())),
            details: json!({
                "script_id": script.id,
                "script": script.name,
                "version": script.version,
                "rule_id": c.rule_id,
                "rule": c.rule_name,
                "trigger": trigger,
                "server_ids": [c.server_id],
                "tier": decision.tier,
                "state": state.as_str(),
            }),
        },
    )
    .await?;
    tx.commit().await?;
    tracing::info!(rule = %c.rule_id, run = %id, state = state.as_str(), "automation rule made a run");
    if state == RunState::Scheduled {
        run::dispatch(pool, commands, id).await?;
    }
    Ok(true)
}

/// A rule's last firings, for its page: what it ran or why it did not.
#[derive(Debug, Clone, Serialize, sqlx::FromRow, utoipa::ToSchema)]
#[schema(as = AutomationFiring)]
pub struct Firing {
    pub event_id: Uuid,
    pub server_id: Option<Uuid>,
    pub hostname: Option<String>,
    /// The run it made, if any.
    pub run_id: Option<Uuid>,
    /// Why it made none.
    pub skipped: Option<String>,
    pub created_at: chrono::DateTime<Utc>,
}

/// The last `limit` firings of `rule`, newest first.
pub async fn firings(pool: &PgPool, rule: Uuid, limit: i64) -> Result<Vec<Firing>, sqlx::Error> {
    sqlx::query_as(
        "SELECT f.event_id, f.server_id, s.hostname, f.run_id, f.skipped, f.created_at
         FROM automation_firings f LEFT JOIN servers s ON s.id = f.server_id
         WHERE f.rule_id = $1 ORDER BY f.created_at DESC LIMIT $2",
    )
    .bind(rule)
    .bind(limit)
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_and_metrics() {
        for kind in [
            EventKind::IncidentOpened,
            EventKind::CheckFailed,
            EventKind::ServerJoined,
        ] {
            assert_eq!(EventKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(EventKind::parse("incident.opened"), None);
        assert!(valid_metric("monitorflow_cpu_usage_ratio") && valid_metric("website:up"));
        assert!(!valid_metric("") && !valid_metric("1x") && !valid_metric("a b"));
    }
}
