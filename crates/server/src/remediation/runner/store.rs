//! The loop's SQL: incidents to open, drive or sweep, leases, state
//! changes with their events, and pending commands.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::remediation::audit::{self, Actor, AuditError, IncidentState, NewEvent};
use crate::remediation::risk::Decision;

/// The incident row and what the loop needs of its server.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Incident {
    pub id: Uuid,
    pub customer_id: Uuid,
    pub site_id: Uuid,
    pub server_id: Uuid,
    pub peer_id: String,
    pub alert_incident_id: Option<Uuid>,
    pub state: IncidentState,
    pub opened_at: DateTime<Utc>,
    pub max_commands: i32,
    pub max_duration_s: i32,
}

impl Incident {
    /// When the duration limit escalates it.
    pub fn deadline(&self) -> DateTime<Utc> {
        self.opened_at + chrono::Duration::seconds(self.max_duration_s.into())
    }

    pub fn max_commands(&self) -> u32 {
        u32::try_from(self.max_commands).unwrap_or(0)
    }
}

pub async fn incident(pool: &PgPool, id: Uuid) -> Result<Option<Incident>, sqlx::Error> {
    sqlx::query_as(
        "SELECT i.id, i.customer_id, s.site_id, i.server_id, s.peer_id, i.alert_incident_id,
                i.state, i.opened_at, i.max_commands, i.max_duration_s
         FROM remediation_incidents i JOIN servers s ON s.id = i.server_id
         WHERE i.id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// An open alert that never had a remediation.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct NewAlert {
    pub id: Uuid,
    pub server_id: Uuid,
    pub metric: String,
    pub instance: String,
}

/// Open alerts opened within `max_age` without any remediation incident,
/// oldest first. Alerts opened before remediation was enabled stay alone.
pub async fn new_alerts(
    pool: &PgPool,
    max_age: Duration,
    limit: i64,
) -> Result<Vec<NewAlert>, sqlx::Error> {
    sqlx::query_as(
        "SELECT a.id, a.server_id, a.metric, a.instance FROM incidents a
         WHERE a.state = 'open' AND a.opened_at > now() - make_interval(secs => $1)
           -- No command reaches an offline host (ADR-0057).
           AND a.metric <> 'offline'
           AND NOT EXISTS (SELECT 1 FROM remediation_incidents r WHERE r.alert_incident_id = a.id)
         ORDER BY a.opened_at LIMIT $2",
    )
    .bind(max_age.as_secs_f64())
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Incidents to drive: `open` or `running`, without a live lease of
/// another holder than `holder`.
pub async fn unleased(pool: &PgPool, holder: Uuid, limit: i64) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT i.id FROM remediation_incidents i
         WHERE i.state IN ('open', 'running')
           AND NOT EXISTS (SELECT 1 FROM remediation_loop_leases l
                           WHERE l.incident_id = i.id AND l.expires_at > now()
                             AND l.holder <> $1)
         ORDER BY i.opened_at LIMIT $2",
    )
    .bind(holder)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Incidents waiting for approval.
pub async fn parked(pool: &PgPool) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT id FROM remediation_incidents WHERE state = 'awaiting_approval' ORDER BY opened_at",
    )
    .fetch_all(pool)
    .await
}

/// Takes the lease of `incident` for `holder`, if it is free, expired or
/// already `holder`'s.
pub async fn acquire(
    pool: &PgPool,
    incident: Uuid,
    holder: Uuid,
    ttl: Duration,
) -> Result<bool, sqlx::Error> {
    let done = sqlx::query(
        "INSERT INTO remediation_loop_leases AS l (incident_id, holder, expires_at)
         VALUES ($1, $2, now() + make_interval(secs => $3))
         ON CONFLICT (incident_id) DO UPDATE
             SET holder = EXCLUDED.holder, expires_at = EXCLUDED.expires_at
             WHERE l.holder = EXCLUDED.holder OR l.expires_at <= now()",
    )
    .bind(incident)
    .bind(holder)
    .bind(ttl.as_secs_f64())
    .execute(pool)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Extends `holder`'s lease; `false` when it lost it.
pub async fn renew(
    pool: &PgPool,
    incident: Uuid,
    holder: Uuid,
    ttl: Duration,
) -> Result<bool, sqlx::Error> {
    let done = sqlx::query(
        "UPDATE remediation_loop_leases SET expires_at = now() + make_interval(secs => $3)
         WHERE incident_id = $1 AND holder = $2 AND expires_at > now()",
    )
    .bind(incident)
    .bind(holder)
    .bind(ttl.as_secs_f64())
    .execute(pool)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Gives up `holder`'s leases, of `incident` or of every incident.
pub async fn release(
    pool: &PgPool,
    incident: Option<Uuid>,
    holder: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "DELETE FROM remediation_loop_leases
         WHERE holder = $2 AND ($1::uuid IS NULL OR incident_id = $1)",
    )
    .bind(incident)
    .bind(holder)
    .execute(pool)
    .await?;
    Ok(())
}

/// Whether the alert `id` resolved: the sensor returned to normal.
pub async fn alert_resolved(pool: &PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    let state: Option<String> = sqlx::query_scalar("SELECT state FROM incidents WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(state.as_deref() == Some("resolved"))
}

/// Locks `incident` and returns its state, for a change in `tx`.
pub async fn lock(
    tx: &mut Transaction<'_, Postgres>,
    incident: Uuid,
) -> Result<IncidentState, AuditError> {
    sqlx::query_scalar("SELECT state FROM remediation_incidents WHERE id = $1 FOR UPDATE")
        .bind(incident)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(AuditError::NotFound)
}

/// Moves `incident` from `from` to `to` with its `state_change` event.
pub async fn transition(
    tx: &mut Transaction<'_, Postgres>,
    incident: Uuid,
    from: IncidentState,
    to: IncidentState,
) -> Result<(), AuditError> {
    audit::append_in(
        tx,
        incident,
        NewEvent::state_change(Actor::System, from, to),
    )
    .await?;
    sqlx::query(
        "UPDATE remediation_incidents
         SET state = $2, closed_at = CASE WHEN $3 THEN now() END
         WHERE id = $1",
    )
    .bind(incident)
    .bind(to)
    .bind(to.is_closed())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Records a proposal that waits for approval.
pub async fn insert_pending(
    tx: &mut Transaction<'_, Postgres>,
    incident: Uuid,
    proposal_seq: i64,
    diagnosis: &str,
    command: &str,
    reasoning: &str,
    decision: &Decision,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO remediation_pending_commands
             (incident_id, proposal_seq, command, diagnosis, reasoning, tier, rule, reason)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(incident)
    .bind(proposal_seq)
    .bind(command)
    .bind(diagnosis)
    .bind(reasoning)
    .bind(decision.tier.as_str())
    .bind(json!(decision.rule))
    .bind(&decision.reason)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Ends the pending command of `incident`, if any, as `state` (`expired`
/// or `cancelled`).
pub async fn close_pending(
    tx: &mut Transaction<'_, Postgres>,
    incident: Uuid,
    state: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE remediation_pending_commands SET state = $2, decided_at = now()
         WHERE incident_id = $1 AND state = 'pending'",
    )
    .bind(incident)
    .bind(state)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Cancels command `id` if the dispatcher has not sent it yet. Returns
/// whether it did.
pub async fn cancel_queued(pool: &PgPool, id: Uuid, reason: &str) -> Result<bool, sqlx::Error> {
    let done = sqlx::query(
        "UPDATE commands SET state = 'cancelled', finished_at = now(), error = $2
         WHERE id = $1 AND state = 'queued'",
    )
    .bind(id)
    .bind(reason)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() == 1)
}
