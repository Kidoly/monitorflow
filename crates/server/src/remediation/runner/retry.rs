//! New attempts on an alert that is still open (ADR-0165).
//!
//! The loop opens one remediation when an alert opens. Once it closes,
//! the alert can get another, each a remediation incident of its own with
//! its own chain and limits: by itself once, when the earlier one stopped
//! because the server refused AI commands and the server has accepted
//! them since, or when a person asks. The new incident's first event is a
//! `note` with code `attempt` that names who started it, why, and how the
//! earlier one ended.

use std::time::Duration;

use serde_json::{json, Value};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::remediation::audit::{self, Actor, AuditError, IncidentState, NewEvent, NewIncident};

/// Codes of the closing event of a remediation that stopped because the
/// server reported refusing AI commands (`commands::acceptance::Refusal`).
pub const REFUSAL_CODES: [&str; 4] = [
    "commands_off",
    "commands_off_windows",
    "system_unsupported",
    "agent_too_old",
];

/// How the agent's refusal of a `shell` command starts, on every system
/// (`crates/agent/src/commands/shell.rs`). A remediation whose command
/// came back with it stopped for the same reason as one the server's
/// report stopped.
pub const AGENT_SHELL_OFF: &str = "shell commands are disabled on this agent";

/// Code of the note that starts an attempt.
pub const ATTEMPT_CODE: &str = "attempt";

/// Who starts an attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// A person, through the API.
    Person(Uuid),
    /// The loop: the server accepts AI commands since the earlier attempt
    /// stopped on a refusal.
    CommandsAccepted,
}

impl Trigger {
    /// Stable code, in the note's `trigger`.
    pub fn code(self) -> &'static str {
        match self {
            Self::Person(_) => "person",
            Self::CommandsAccepted => "commands_accepted",
        }
    }

    fn actor(self) -> Actor {
        match self {
            Self::Person(user) => Actor::User(user),
            Self::CommandsAccepted => Actor::System,
        }
    }

    fn text(self) -> &'static str {
        match self {
            Self::Person(_) => "started by a person",
            Self::CommandsAccepted => "started because the server now accepts AI commands",
        }
    }
}

/// Why no attempt can start on an alert now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blocked {
    /// The alert resolved: there is nothing left to fix.
    AlertResolved,
    /// A remediation of the alert is open, running or waits for approval.
    InProgress,
    /// Another attempt started since the caller looked.
    Changed,
}

impl Blocked {
    /// Stable code for clients.
    pub fn code(self) -> &'static str {
        match self {
            Self::AlertResolved => "alert_resolved",
            Self::InProgress => "in_progress",
            Self::Changed => "changed",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::AlertResolved => "the alert is resolved",
            Self::InProgress => {
                "an AI remediation of this alert is already running or waiting for approval"
            }
            Self::Changed => "another AI remediation of this alert started meanwhile",
        }
    }
}

/// Why `alert` cannot get an attempt, from its state and the state of its
/// latest remediation. A device or an offline host gets one too: the AI
/// diagnoses it without commands (ADR-0175).
pub fn blocked(alert_state: &str, latest: Option<IncidentState>) -> Option<Blocked> {
    if alert_state != "open" {
        return Some(Blocked::AlertResolved);
    }
    if latest.is_some_and(|state| !state.is_closed()) {
        return Some(Blocked::InProgress);
    }
    None
}

#[derive(Debug, thiserror::Error)]
pub enum RetryError {
    #[error("alert not found")]
    NotFound,
    #[error("{}", .0.message())]
    Blocked(Blocked),
    #[error(transparent)]
    Audit(#[from] AuditError),
    #[error("database: {0}")]
    Database(#[from] sqlx::Error),
}

/// An alert and where its remediations stand.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AlertAttempts {
    pub server_id: Uuid,
    pub site_id: Uuid,
    pub metric: String,
    pub instance: String,
    /// `open` or `resolved`.
    pub state: String,
    pub opened_at: chrono::DateTime<chrono::Utc>,
    pub device: bool,
    /// Remediation incidents of the alert so far.
    pub attempts: i64,
    pub latest_id: Option<Uuid>,
    pub latest_state: Option<IncidentState>,
}

impl AlertAttempts {
    pub fn blocked(&self) -> Option<Blocked> {
        blocked(&self.state, self.latest_state)
    }
}

const ALERT: &str = "SELECT a.server_id, s.site_id, a.metric, a.instance, a.state, a.opened_at,
            s.kind = 'device' AS device,
            (SELECT count(*) FROM remediation_incidents r
             WHERE r.alert_incident_id = a.id) AS attempts,
            l.id AS latest_id, l.state AS latest_state
     FROM incidents a
     JOIN servers s ON s.id = a.server_id
     LEFT JOIN LATERAL (SELECT r.id, r.state FROM remediation_incidents r
                        WHERE r.alert_incident_id = a.id
                        ORDER BY r.opened_at DESC, r.id DESC LIMIT 1) l ON true
     WHERE a.id = $1";

/// `alert` and its remediations, or `None` when it does not exist.
pub async fn alert(pool: &PgPool, alert: Uuid) -> Result<Option<AlertAttempts>, sqlx::Error> {
    sqlx::query_as(ALERT).bind(alert).fetch_optional(pool).await
}

/// What a new attempt needs besides the alert.
#[derive(Debug, Clone, Copy)]
pub struct Attempt {
    pub trigger: Trigger,
    /// The latest remediation the caller saw, for the loop: when another
    /// started since, nothing opens. `None` takes whatever is latest.
    pub after: Option<Uuid>,
    pub max_commands: u32,
    pub max_duration: Duration,
}

/// What [`open`] opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    pub id: Uuid,
    /// 1 for an alert's first remediation.
    pub attempt: i64,
    pub previous: Option<Uuid>,
}

/// Opens a remediation of `alert` in `tx`, with its `attempt` note. The
/// alert's row stays locked until `tx` ends, so two callers never open two
/// attempts after the same one; the unique index on active remediations
/// backs that up.
pub async fn open(
    tx: &mut Transaction<'_, Postgres>,
    alert: Uuid,
    attempt: Attempt,
) -> Result<Opened, RetryError> {
    sqlx::query("SELECT 1 FROM incidents WHERE id = $1 FOR UPDATE")
        .bind(alert)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(RetryError::NotFound)?;
    let found: AlertAttempts = sqlx::query_as(ALERT)
        .bind(alert)
        .fetch_one(&mut **tx)
        .await?;
    if let Some(blocked) = found.blocked() {
        return Err(RetryError::Blocked(blocked));
    }
    if attempt.after.is_some() && attempt.after != found.latest_id {
        return Err(RetryError::Blocked(Blocked::Changed));
    }
    let previous = match found.latest_id {
        Some(id) => Some(ending(tx, id).await?),
        None => None,
    };
    let id = audit::open(
        &mut **tx,
        &NewIncident {
            server_id: found.server_id,
            alert_incident_id: Some(alert),
            metric: found.metric,
            instance: found.instance,
            max_commands: attempt.max_commands,
            max_duration: attempt.max_duration,
        },
    )
    .await?;
    let number = found.attempts + 1;
    let note = note(attempt.trigger, number, previous.as_ref());
    audit::append_in(tx, id, note).await?;
    Ok(Opened {
        id,
        attempt: number,
        previous: found.latest_id,
    })
}

/// How the remediation `id` ended: its state, and the code and sentence of
/// the event that closed it.
async fn ending(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<Value, sqlx::Error> {
    let row: Option<(IncidentState, Option<Value>)> = sqlx::query_as(
        "SELECT r.state, e.payload
         FROM remediation_incidents r
         LEFT JOIN remediation_incident_events e ON e.incident_id = r.id AND e.seq = r.head_seq
         WHERE r.id = $1",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    let (state, payload) = row.unwrap_or((IncidentState::Failed, None));
    let payload = payload.unwrap_or_default();
    let reason = payload["reason"]
        .as_str()
        .or_else(|| payload["text"].as_str());
    Ok(json!({
        "id": id,
        "state": state.as_str(),
        "code": payload["code"].as_str(),
        "reason": reason,
    }))
}

/// The first event of an attempt: who started it, its number, and how the
/// one before ended.
pub fn note(trigger: Trigger, attempt: i64, previous: Option<&Value>) -> NewEvent {
    let mut text = format!("attempt {attempt}, {}", trigger.text());
    if let Some(state) = previous.and_then(|p| p["state"].as_str()) {
        text.push_str(&format!(", after the attempt before was {state}"));
    }
    let mut event = NewEvent::note(trigger.actor(), &text);
    event.payload["code"] = json!(ATTEMPT_CODE);
    event.payload["trigger"] = json!(trigger.code());
    event.payload["attempt"] = json!(attempt);
    event.payload["previous"] = previous.cloned().unwrap_or(Value::Null);
    event
}

/// Open alerts whose latest remediation escalated because the server
/// refused AI commands, on a server that started accepting them after
/// that, oldest alert first: the alert and that remediation.
pub async fn refused_then_accepted(
    pool: &PgPool,
    limit: i64,
) -> Result<Vec<(Uuid, Uuid)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT a.id, r.id FROM incidents a
         JOIN servers s ON s.id = a.server_id
         JOIN LATERAL (SELECT r.id, r.state, r.closed_at, r.head_seq
                       FROM remediation_incidents r WHERE r.alert_incident_id = a.id
                       ORDER BY r.opened_at DESC, r.id DESC LIMIT 1) r ON true
         JOIN remediation_incident_events e ON e.incident_id = r.id AND e.seq = r.head_seq
         WHERE a.state = 'open' AND a.metric <> 'offline' AND s.kind <> 'device'
           AND r.state = 'escalated' AND s.shell_accepted_at > r.closed_at
           AND e.kind = 'escalation'
           AND (e.payload->>'code' = ANY($1)
                -- The agent's own refusal; events before codes have none.
                OR coalesce(e.payload->>'code', 'incomplete') = 'incomplete'
                   AND EXISTS (SELECT 1 FROM remediation_incident_events c
                               WHERE c.incident_id = r.id AND c.seq = r.head_seq - 1
                                 AND c.kind = 'command_result'
                                 AND starts_with(c.payload->>'error', $2)))
         ORDER BY a.opened_at LIMIT $3",
    )
    .bind(&REFUSAL_CODES[..])
    .bind(AGENT_SHELL_OFF)
    .bind(limit)
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_open_alert_with_nothing_in_progress_gets_one() {
        assert_eq!(blocked("open", None), None);
        assert_eq!(blocked("open", Some(IncidentState::Escalated)), None);
        assert_eq!(blocked("open", Some(IncidentState::Failed)), None);
        assert_eq!(blocked("open", Some(IncidentState::Resolved)), None);
        for active in [
            IncidentState::Open,
            IncidentState::Running,
            IncidentState::AwaitingApproval,
        ] {
            assert_eq!(blocked("open", Some(active)), Some(Blocked::InProgress));
        }
        assert_eq!(blocked("resolved", None), Some(Blocked::AlertResolved));
    }

    #[test]
    fn the_note_says_who_and_how_the_one_before_ended() {
        let user = Uuid::from_u128(1);
        let previous = json!({ "id": Uuid::from_u128(2), "state": "escalated",
                               "code": "incomplete", "reason": "refused" });
        let note = note(Trigger::Person(user), 2, Some(&previous));
        assert_eq!(note.actor, Actor::User(user));
        assert_eq!(note.payload["code"], "attempt");
        assert_eq!(note.payload["trigger"], "person");
        assert_eq!(note.payload["attempt"], 2);
        assert_eq!(note.payload["previous"], previous);
        assert_eq!(
            note.payload["text"],
            "attempt 2, started by a person, after the attempt before was escalated"
        );
        let first = super::note(Trigger::CommandsAccepted, 1, None);
        assert_eq!(first.actor, Actor::System);
        assert_eq!(first.payload["previous"], Value::Null);
        assert_eq!(
            first.payload["text"],
            "attempt 1, started because the server now accepts AI commands"
        );
    }

    #[test]
    fn refusal_codes_are_the_acceptance_ones() {
        use crate::commands::acceptance::Refusal;
        for windows in [false, true] {
            for refusal in [Refusal::Off { windows }, Refusal::Unsupported { windows }] {
                assert!(REFUSAL_CODES.contains(&refusal.code()), "{refusal:?}");
            }
        }
    }
}
