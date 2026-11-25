//! Ticket statuses from the PSA back to MonitorFlow (ADR-0164, ADR-0191).
//!
//! Every minute the leader reads, one by one, the open tickets of
//! incidents checked longest ago, at most [`PER_PASS`] of them: HaloPSA's
//! public documentation gives no filter on the update time, and a ticket
//! read by its id is a documented request. A ticket moved to one of the
//! connection's closed statuses, or assigned to an agent, while its
//! incident is open and not acknowledged, acknowledges the incident, which
//! stops its follow-up and repeat notifications; it never resolves it, as
//! incidents resolve from measurements only (ADR-0017). A ticket deleted
//! or merged ends the link. Both are audited as actions of the connection.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use serde_json::json;
use sqlx::PgPool;
use tokio::sync::Mutex;
use tokio::time::{interval, MissedTickBehavior};
use uuid::Uuid;

use super::halopsa::{Limiter, TicketState};
use super::{Connection, Halo, TicketSettings};
use crate::api::audit;
use crate::notify::follow_up;

/// Time between two passes.
pub const POLL: Duration = Duration::from_secs(60);
/// Tickets read at most per pass: 150 requests in HaloPSA's 5 minutes,
/// a quarter of what the process allows itself.
pub const PER_PASS: i64 = 30;

/// What reading a ticket back means for its incident.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seen {
    /// Nothing to do but remember what the PSA said.
    Unchanged,
    /// Acknowledge the incident: the ticket was closed in the PSA.
    Closed,
    /// Acknowledge the incident: an agent took the ticket.
    Assigned(i64),
    /// The ticket is gone: the link ends, with this reason.
    Ended(String),
}

/// What `ticket` (`None`: the PSA no longer has it) means for an incident
/// that is `open` and `acknowledged` or not, under `settings`, the
/// application acting as the agent `own_agent`.
pub fn seen(
    ticket: Option<&TicketState>,
    settings: &TicketSettings,
    own_agent: Option<i64>,
    open: bool,
    acknowledged: bool,
) -> Seen {
    let Some(ticket) = ticket.filter(|t| !t.deleted) else {
        return Seen::Ended("The ticket was deleted in HaloPSA.".into());
    };
    if let Some(into) = ticket.merged_into {
        return Seen::Ended(format!(
            "The ticket was merged into ticket {into} in HaloPSA."
        ));
    }
    if !open || acknowledged {
        return Seen::Unchanged;
    }
    if ticket
        .status_id
        .is_some_and(|s| settings.closed_status_ids.contains(&s))
    {
        return Seen::Closed;
    }
    match ticket.agent_id {
        // 1 is "unassigned" in HaloPSA; the application's own agent is
        // the ticket type's default, not someone taking it.
        Some(agent) if settings.acknowledge_on_assign && agent > 1 && Some(agent) != own_agent => {
            Seen::Assigned(agent)
        }
        _ => Seen::Unchanged,
    }
}

#[derive(Debug, sqlx::FromRow)]
struct Watched {
    incident_id: Uuid,
    ticket_id: i64,
    open: bool,
    acknowledged: bool,
}

pub struct Poller {
    pool: PgPool,
    limiter: Arc<Limiter>,
    /// Agents' names by id, read once per process.
    agents: Mutex<HashMap<i64, String>>,
}

impl Poller {
    pub fn new(pool: PgPool) -> Self {
        Self::with_limiter(pool, Limiter::shared())
    }

    pub fn with_limiter(pool: PgPool, limiter: Arc<Limiter>) -> Self {
        Self {
            pool,
            limiter,
            agents: Mutex::new(HashMap::new()),
        }
    }

    /// Reads tickets back every `poll` until `stop` resolves.
    pub async fn run(self, poll: Duration, stop: impl Future<Output = ()>) {
        tokio::pin!(stop);
        let mut tick = interval(poll);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = &mut stop => return,
                _ = tick.tick() => {}
            }
            if let Err(error) = self.pass().await {
                tracing::warn!(%error, "could not read PSA tickets back; will retry");
            }
        }
    }

    /// One pass; how many tickets were read.
    pub async fn pass(&self) -> Result<usize, sqlx::Error> {
        let Some(connection) = super::load(&self.pool).await? else {
            return Ok(0);
        };
        if !connection.enabled {
            return Ok(0);
        }
        let Ok(credentials) = connection.credentials() else {
            return Ok(0);
        };
        let halo = Halo::new(credentials).with_limiter(self.limiter.clone());
        let watched: Vec<Watched> = sqlx::query_as(
            "SELECT t.incident_id, t.ticket_id, i.state = 'open' AS open,
                    EXISTS (SELECT 1 FROM incident_acknowledgements a
                            WHERE a.incident_id = t.incident_id) AS acknowledged
             FROM psa_tickets t JOIN incidents i ON i.id = t.incident_id
             WHERE t.connection_id = $1 AND t.state = 'open' AND t.ticket_id IS NOT NULL
             ORDER BY t.checked_at NULLS FIRST, t.incident_id
             LIMIT $2",
        )
        .bind(connection.id)
        .bind(PER_PASS)
        .fetch_all(&self.pool)
        .await?;
        let mut read = 0;
        for ticket in &watched {
            let state = match halo.ticket(ticket.ticket_id).await {
                Ok(state) => state,
                Err(error) => {
                    super::record_failure(&self.pool, connection.id, &error.message).await?;
                    // A rate limit or an outage: the rest waits for the next pass.
                    break;
                }
            };
            read += 1;
            let outcome = seen(
                state.as_ref(),
                &connection.tickets,
                connection.agent_id,
                ticket.open,
                ticket.acknowledged,
            );
            self.apply(&connection, &halo, ticket, state.as_ref(), outcome)
                .await?;
        }
        if read > 0 {
            super::record_success(&self.pool, connection.id, None).await?;
        }
        Ok(read)
    }

    /// The name of the agent `id`, from the cache or the PSA.
    async fn agent_name(&self, halo: &Halo, id: i64) -> Option<String> {
        if let Some(name) = self.agents.lock().await.get(&id) {
            return Some(name.clone());
        }
        let name = halo.agent(id).await.ok().flatten()?;
        self.agents.lock().await.insert(id, name.clone());
        Some(name)
    }

    async fn apply(
        &self,
        connection: &Connection,
        halo: &Halo,
        ticket: &Watched,
        state: Option<&TicketState>,
        outcome: Seen,
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE psa_tickets SET checked_at = now(), psa_status_id = $2, psa_agent_id = $3
             WHERE incident_id = $1",
        )
        .bind(ticket.incident_id)
        .bind(state.and_then(|s| s.status_id))
        .bind(state.and_then(|s| s.agent_id))
        .execute(&mut *tx)
        .await?;
        let target = ticket.incident_id.to_string();
        match outcome {
            Seen::Unchanged => {}
            Seen::Ended(reason) => {
                sqlx::query(
                    "UPDATE psa_tickets SET state = 'ended', error = $2, updated_at = now()
                     WHERE incident_id = $1",
                )
                .bind(ticket.incident_id)
                .bind(&reason)
                .execute(&mut *tx)
                .await?;
                record(
                    &mut tx,
                    "psa.ticket.end",
                    &target,
                    json!({
                        "ticket_id": ticket.ticket_id,
                        "merged_into": state.and_then(|s| s.merged_into),
                    }),
                )
                .await?;
                tracing::info!(incident = %ticket.incident_id, ticket = ticket.ticket_id, "a PSA ticket ended its link");
            }
            Seen::Closed | Seen::Assigned(_) => {
                let agent = match state.and_then(|s| s.agent_id).filter(|a| *a > 1) {
                    Some(id) if Some(id) != connection.agent_id => self.agent_name(halo, id).await,
                    _ => None,
                };
                let (reason, note) = match outcome {
                    Seen::Closed => (
                        "closed",
                        format!("Closed in HaloPSA, ticket {}.", ticket.ticket_id),
                    ),
                    _ => (
                        "assigned",
                        format!("Assigned in HaloPSA, ticket {}.", ticket.ticket_id),
                    ),
                };
                let username = match &agent {
                    Some(name) => format!("{name} (HaloPSA)"),
                    None => "HaloPSA".to_owned(),
                };
                let done = follow_up::acknowledge(
                    &mut tx,
                    &[ticket.incident_id],
                    (None, &username),
                    "psa",
                    Some(&note),
                    Utc::now(),
                )
                .await?;
                if !done.is_empty() {
                    record(
                        &mut tx,
                        "psa.ticket.acknowledge",
                        &target,
                        json!({
                            "ticket_id": ticket.ticket_id,
                            "reason": reason,
                            "status_id": state.and_then(|s| s.status_id),
                            "agent_id": state.and_then(|s| s.agent_id),
                        }),
                    )
                    .await?;
                    tracing::info!(incident = %ticket.incident_id, ticket = ticket.ticket_id, reason, "acknowledged an incident from its PSA ticket");
                }
            }
        }
        tx.commit().await
    }
}

/// An action of the connection in the audit log: Halo ids, never ticket
/// text.
async fn record(
    tx: &mut sqlx::PgConnection,
    action: &str,
    incident: &str,
    details: serde_json::Value,
) -> Result<(), sqlx::Error> {
    audit::record(
        &mut *tx,
        audit::Event {
            actor_kind: "system",
            actor_id: "psa",
            action,
            target: Some(("incident", incident)),
            details,
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> TicketSettings {
        TicketSettings {
            ticket_type_id: None,
            ticket_type_name: None,
            category: None,
            min_severity: "critical".into(),
            resolved_status_id: Some(9),
            resolved_status_name: Some("Closed".into()),
            warning_priority_id: None,
            warning_priority_name: None,
            critical_priority_id: None,
            critical_priority_name: None,
            closed_status_ids: vec![9, 20],
            acknowledge_on_assign: true,
        }
    }

    fn ticket(status: i64, agent: i64) -> TicketState {
        TicketState {
            status_id: Some(status),
            agent_id: Some(agent),
            deleted: false,
            merged_into: None,
        }
    }

    #[test]
    fn a_closed_status_acknowledges_an_open_incident_only() {
        let s = settings();
        assert_eq!(
            seen(Some(&ticket(9, 1)), &s, Some(42), true, false),
            Seen::Closed
        );
        assert_eq!(
            seen(Some(&ticket(20, 1)), &s, Some(42), true, false),
            Seen::Closed
        );
        assert_eq!(
            seen(Some(&ticket(2, 1)), &s, Some(42), true, false),
            Seen::Unchanged
        );
        // Resolved, or acknowledged already: nothing to do.
        assert_eq!(
            seen(Some(&ticket(9, 1)), &s, Some(42), false, false),
            Seen::Unchanged
        );
        assert_eq!(
            seen(Some(&ticket(9, 1)), &s, Some(42), true, true),
            Seen::Unchanged
        );
    }

    #[test]
    fn an_agent_taking_it_acknowledges_but_not_the_application_itself() {
        let mut s = settings();
        assert_eq!(
            seen(Some(&ticket(2, 7)), &s, Some(42), true, false),
            Seen::Assigned(7)
        );
        assert_eq!(
            seen(Some(&ticket(2, 42)), &s, Some(42), true, false),
            Seen::Unchanged
        );
        assert_eq!(
            seen(Some(&ticket(2, 1)), &s, Some(42), true, false),
            Seen::Unchanged
        );
        s.acknowledge_on_assign = false;
        assert_eq!(
            seen(Some(&ticket(2, 7)), &s, Some(42), true, false),
            Seen::Unchanged
        );
    }

    #[test]
    fn a_deleted_or_merged_ticket_ends_the_link() {
        let s = settings();
        assert!(matches!(seen(None, &s, None, true, false), Seen::Ended(_)));
        let mut deleted = ticket(2, 1);
        deleted.deleted = true;
        assert!(matches!(
            seen(Some(&deleted), &s, None, false, true),
            Seen::Ended(_)
        ));
        let mut merged = ticket(2, 1);
        merged.merged_into = Some(77);
        assert_eq!(
            seen(Some(&merged), &s, None, true, false),
            Seen::Ended("The ticket was merged into ticket 77 in HaloPSA.".into())
        );
    }
}
