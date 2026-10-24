//! Approvers' decisions on commands waiting for approval (architecture §8
//! Approval, ADR-0045).
//!
//! A decision resumes a parked incident without touching the loop
//! (ADR-0043): under the incident's row lock it appends `edit` then
//! `approval`, or `rejection`, by the user, marks the pending command
//! decided and moves the incident back to `running`, in the caller's
//! transaction. The loop's next poll leases the incident and acts on the
//! new events. Who may decide is the API's business; this module only
//! checks that the decision is still possible.

use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::audit::{self, Actor, AuditError, IncidentState, NewEvent};

/// Longest command, as `remediation_pending_commands` allows.
pub const MAX_COMMAND_BYTES: usize = 4096;
/// Longest rejection reason, in characters.
pub const MAX_REASON_CHARS: usize = 1000;

/// What an approver decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Runs the proposed command, or `edited` instead when given and
    /// different.
    Approve { edited: Option<String> },
    /// Drops the command; the reason goes back to the AI.
    Reject { reason: String },
}

#[derive(Debug, thiserror::Error)]
pub enum DecideError {
    #[error("pending command not found")]
    NotFound,
    /// Decided already, the incident moved on, or its deadline passed.
    #[error("{0}")]
    Conflict(&'static str),
    #[error(transparent)]
    Audit(#[from] AuditError),
    #[error("database: {0}")]
    Database(#[from] sqlx::Error),
}

/// What a decision recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decided {
    pub incident_id: Uuid,
    /// The command that will run, or the one rejected.
    pub command: String,
    pub edited: bool,
}

/// Why `command` cannot replace a proposal, if it cannot: empty, longer
/// than [`MAX_COMMAND_BYTES`], or with a NUL byte the agent cannot pass
/// to a shell.
pub fn check_command(command: &str) -> Result<(), &'static str> {
    if command.trim().is_empty() {
        return Err("command must not be empty");
    }
    if command.len() > MAX_COMMAND_BYTES {
        return Err("command must be at most 4096 bytes");
    }
    if command.contains('\0') {
        return Err("command must not contain NUL bytes");
    }
    Ok(())
}

/// Why `reason` cannot explain a rejection, if it cannot.
pub fn check_reason(reason: &str) -> Result<(), &'static str> {
    if reason.trim().is_empty() {
        return Err("reason must not be empty");
    }
    if reason.chars().count() > MAX_REASON_CHARS {
        return Err("reason must be at most 1000 characters");
    }
    Ok(())
}

/// Records `user`'s `verdict` on the pending command `pending` in `tx`.
/// [`DecideError::Conflict`] when the command is no longer pending, the
/// incident no longer waits for approval, or its deadline passed: nothing
/// is written then.
pub async fn decide(
    tx: &mut Transaction<'_, Postgres>,
    pending: Uuid,
    user: Uuid,
    verdict: &Verdict,
) -> Result<Decided, DecideError> {
    let incident: Uuid =
        sqlx::query_scalar("SELECT incident_id FROM remediation_pending_commands WHERE id = $1")
            .bind(pending)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or(DecideError::NotFound)?;
    // The incident first, then its pending row: the loop's order when it
    // closes an incident.
    let (state, overdue): (IncidentState, bool) = sqlx::query_as(
        "SELECT state, opened_at + make_interval(secs => max_duration_s) <= now()
         FROM remediation_incidents WHERE id = $1 FOR UPDATE",
    )
    .bind(incident)
    .fetch_one(&mut **tx)
    .await?;
    let (proposed, pending_state): (String, String) = sqlx::query_as(
        "SELECT command, state FROM remediation_pending_commands WHERE id = $1 FOR UPDATE",
    )
    .bind(pending)
    .fetch_one(&mut **tx)
    .await?;
    if pending_state != "pending" {
        return Err(DecideError::Conflict("the command is no longer pending"));
    }
    if state != IncidentState::AwaitingApproval {
        return Err(DecideError::Conflict(
            "the incident is no longer waiting for approval",
        ));
    }
    if overdue {
        return Err(DecideError::Conflict(
            "the incident reached its duration limit",
        ));
    }

    let (decided, new_state) = match verdict {
        Verdict::Approve { edited } => {
            let edited = edited.as_deref().filter(|e| *e != proposed);
            if let Some(edited) = edited {
                audit::append_in(tx, incident, NewEvent::edit(user, &proposed, edited)).await?;
            }
            let command = edited.unwrap_or(&proposed).to_owned();
            audit::append_in(
                tx,
                incident,
                NewEvent::approval(Actor::User(user), &command),
            )
            .await?;
            let decided = Decided {
                incident_id: incident,
                command,
                edited: edited.is_some(),
            };
            (decided, "approved")
        }
        Verdict::Reject { reason } => {
            audit::append_in(
                tx,
                incident,
                NewEvent::rejection(user, &proposed, Some(reason)),
            )
            .await?;
            let decided = Decided {
                incident_id: incident,
                command: proposed,
                edited: false,
            };
            (decided, "rejected")
        }
    };
    sqlx::query(
        "UPDATE remediation_pending_commands
         SET state = $2, decided_at = now(), decided_by = $3
         WHERE id = $1",
    )
    .bind(pending)
    .bind(new_state)
    .bind(user)
    .execute(&mut **tx)
    .await?;
    audit::append_in(
        tx,
        incident,
        NewEvent::state_change(
            Actor::User(user),
            IncidentState::AwaitingApproval,
            IncidentState::Running,
        ),
    )
    .await?;
    sqlx::query("UPDATE remediation_incidents SET state = 'running' WHERE id = $1")
        .bind(incident)
        .execute(&mut **tx)
        .await?;
    Ok(decided)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_and_reasons_are_bounded() {
        assert!(check_command("systemctl reload nginx").is_ok());
        assert!(check_command("  \n").is_err());
        assert!(check_command(&"a".repeat(MAX_COMMAND_BYTES)).is_ok());
        assert!(check_command(&"a".repeat(MAX_COMMAND_BYTES + 1)).is_err());
        assert!(check_command("echo \0").is_err());
        assert!(check_reason("not during business hours").is_ok());
        assert!(check_reason(" ").is_err());
        assert!(check_reason(&"é".repeat(MAX_REASON_CHARS)).is_ok());
        assert!(check_reason(&"é".repeat(MAX_REASON_CHARS + 1)).is_err());
    }
}
