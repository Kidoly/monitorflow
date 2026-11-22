//! What a new attempt on an alert is told of the ones before it
//! (ADR-0165): how each ended, the commands it ran and what became of
//! them, so the AI does not start again blind. Read from their audit
//! events, which were redacted when written, and sent as a data block of
//! the context like the rest.

use std::fmt::Write;

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use super::progress::{head, tail};
use crate::remediation::audit::{self, AuditError, EventKind, IncidentState, StoredEvent};

/// Earlier attempts told, newest first.
pub const MAX_ATTEMPTS: i64 = 3;
/// The end of each command's output told, in bytes.
const OUTPUT_BYTES: usize = 1024;
/// The longest text of the AI or a reason told, in bytes.
const TEXT_BYTES: usize = 1024;

/// Name of the context's data block.
pub const BLOCK: &str = "earlier_attempts";

/// One earlier remediation of the alert.
#[derive(Debug, Clone)]
pub struct Earlier {
    pub opened_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    pub state: IncidentState,
    pub events: Vec<StoredEvent>,
}

/// An earlier remediation's ID, times and state.
type Row = (Uuid, DateTime<Utc>, Option<DateTime<Utc>>, IncidentState);

/// The remediations of `alert` opened before `incident`, newest first.
pub async fn of(pool: &PgPool, alert: Uuid, incident: Uuid) -> Result<Vec<Earlier>, AuditError> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT r.id, r.opened_at, r.closed_at, r.state FROM remediation_incidents r
         JOIN remediation_incidents this ON this.id = $2
         WHERE r.alert_incident_id = $1 AND r.id <> $2
           AND (r.opened_at, r.id) < (this.opened_at, this.id)
         ORDER BY r.opened_at DESC, r.id DESC LIMIT $3",
    )
    .bind(alert)
    .bind(incident)
    .bind(MAX_ATTEMPTS)
    .fetch_all(pool)
    .await?;
    let mut earlier = Vec::with_capacity(rows.len());
    for (id, opened_at, closed_at, state) in rows {
        earlier.push(Earlier {
            opened_at,
            closed_at,
            state,
            events: audit::events(pool, id).await?,
        });
    }
    Ok(earlier)
}

fn text<'a>(event: &'a StoredEvent, key: &str) -> &'a str {
    event.payload[key].as_str().unwrap_or_default()
}

/// One line, control characters escaped.
fn flat(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

/// The block's body.
pub fn render(earlier: &[Earlier]) -> String {
    let mut out = String::from(
        "Earlier AI remediation attempts on this alert, newest first. The alert is still \
         open. Do not repeat what failed unless something changed since.\n",
    );
    for attempt in earlier {
        let _ = write!(out, "\nattempt opened {}", attempt.opened_at.to_rfc3339());
        match attempt.closed_at {
            Some(at) => {
                let _ = writeln!(out, ", {} {}", attempt.state.as_str(), at.to_rfc3339());
            }
            None => {
                let _ = writeln!(out, ", {}", attempt.state.as_str());
            }
        }
        for event in &attempt.events {
            match event.kind {
                EventKind::AiProposal => {
                    let diagnosis = head(text(event, "diagnosis"), TEXT_BYTES);
                    let _ = writeln!(out, "  diagnosis: {}", flat(&diagnosis));
                }
                EventKind::CommandQueued => {
                    let _ = writeln!(out, "  command: {}", flat(text(event, "command")));
                }
                EventKind::CommandResult => {
                    match event.payload["exit_code"].as_i64() {
                        Some(code) => {
                            let _ = writeln!(out, "    exit code {code}");
                        }
                        None => {
                            let error = head(text(event, "error"), TEXT_BYTES);
                            let _ = writeln!(out, "    did not complete: {}", flat(&error));
                        }
                    }
                    let stdout = tail(text(event, "stdout"), OUTPUT_BYTES);
                    for line in stdout.lines() {
                        let _ = writeln!(out, "    | {}", flat(line));
                    }
                }
                EventKind::Rejection => {
                    let reason = head(text(event, "reason"), TEXT_BYTES);
                    let _ = writeln!(out, "    rejected by an approver: {}", flat(&reason));
                }
                EventKind::Escalation => {
                    let reason = head(text(event, "reason"), TEXT_BYTES);
                    let _ = writeln!(out, "  ended: {}", flat(&reason));
                }
                EventKind::Note if event.payload.get("to").is_some() => {
                    let _ = writeln!(out, "  ended: {}", flat(text(event, "text")));
                }
                _ => {}
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::remediation::audit::{Actor, NewEvent};

    fn stored(events: Vec<NewEvent>) -> Vec<StoredEvent> {
        events
            .into_iter()
            .zip(1..)
            .map(|(e, seq)| StoredEvent {
                seq,
                kind: e.kind,
                actor_kind: "system".into(),
                actor_id: "server".into(),
                payload: e.payload,
                created_at: Utc::now(),
                prev_hash: Vec::new(),
                hash: Vec::new(),
            })
            .collect()
    }

    #[test]
    fn tells_the_commands_their_outcome_and_the_end() {
        let refusal = "shell commands are disabled on this agent (commands.shell)";
        let at = DateTime::parse_from_rfc3339("2026-11-04T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let events = stored(vec![
            NewEvent::context_sent("m", json!({ "rendered": "SECRET CONTEXT" })),
            NewEvent::ai_proposal("m", "disk / is 90% full", "see usage", Some("df -h /")),
            NewEvent::approval(Actor::System, "df -h /"),
            NewEvent::command_queued(Uuid::nil(), "df -h /"),
            NewEvent::command_result(
                "agent",
                Uuid::nil(),
                None,
                "",
                "",
                Some(refusal),
                std::time::Duration::ZERO,
            ),
            NewEvent::closing(
                IncidentState::Running,
                IncidentState::Escalated,
                &format!("the command did not run to completion: {refusal}"),
                "incomplete",
                None,
                None,
            ),
        ]);
        let text = render(&[Earlier {
            opened_at: at,
            closed_at: Some(at + chrono::Duration::seconds(10)),
            state: IncidentState::Escalated,
            events,
        }]);
        assert!(
            text.contains(
                "attempt opened 2026-11-04T10:00:00+00:00, escalated 2026-11-04T10:00:10+00:00"
            ),
            "{text}"
        );
        assert!(text.contains("  diagnosis: disk / is 90% full\n"), "{text}");
        assert!(text.contains("  command: df -h /\n"), "{text}");
        assert!(
            text.contains(&format!("    did not complete: {refusal}\n")),
            "{text}"
        );
        assert!(
            text.contains("  ended: the command did not run to completion"),
            "{text}"
        );
        // The context it was sent is not repeated.
        assert!(!text.contains("SECRET CONTEXT"), "{text}");
    }

    #[test]
    fn outputs_are_cut_and_kept_on_their_lines() {
        let events = stored(vec![
            NewEvent::command_queued(Uuid::nil(), "du -sh /var"),
            NewEvent::command_result(
                "agent",
                Uuid::nil(),
                Some(0),
                &format!("{}\nlast line\u{1b}[31m", "x".repeat(4000)),
                "",
                None,
                std::time::Duration::ZERO,
            ),
        ]);
        let text = render(&[Earlier {
            opened_at: Utc::now(),
            closed_at: None,
            state: IncidentState::Running,
            events,
        }]);
        assert!(text.contains("    exit code 0\n"), "{text}");
        assert!(text.contains("    | last line\\u{1b}[31m\n"), "{text}");
        assert!(text.len() < 2000, "{}", text.len());
    }
}
