//! Where an incident stands, read from its events alone.
//!
//! The loop keeps no state of its own: every step reads the incident's
//! events, takes the next action they call for and appends what it did.
//! A loop resumed after a restart, or after an approver decided, therefore
//! runs the same code as one that never stopped, and the AI gets back
//! exactly what the audit trail shows it was sent.

use chrono::{DateTime, Utc};
use monitorflow_ai::{CommandOutcome, Exchange, ProposedCommand, StopOutcome};
use serde_json::Value;
use uuid::Uuid;

use crate::remediation::audit::{EventKind, StoredEvent};

/// The end of each output stream sent back to the AI, in bytes. The audit
/// trail keeps up to the 64 KiB the server stores.
pub const AI_OUTPUT_BYTES: usize = 8 * 1024;

/// What the events call for next.
#[derive(Debug, Clone, PartialEq)]
pub enum Next {
    /// Gather the context and record it.
    Context,
    /// Ask the AI for the next step.
    Ask,
    /// Classify the AI's proposal, recorded as event `seq`.
    Classify {
        seq: i64,
        diagnosis: String,
        proposal: ProposedCommand,
    },
    /// Run `command`, approved by the system (a tier that needs no
    /// approval) or by a user.
    Run {
        command: String,
        by_system: bool,
        /// Whether the snapshot hook already ran for it.
        snapshot_done: bool,
    },
    /// Wait for the result of the command queued as `command_id`.
    Wait { command_id: Uuid, command: String },
    /// The AI decided to stop at `at`.
    Stopped {
        outcome: StopOutcome,
        reason: String,
        at: DateTime<Utc>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    /// The rendered context as sent, once recorded.
    pub context: Option<String>,
    /// Proposals and what became of them, oldest first.
    pub history: Vec<Exchange>,
    /// Commands queued so far, the count the command limit applies to.
    pub commands_queued: u32,
    pub next: Next,
}

fn text(payload: &Value, key: &str) -> String {
    payload[key].as_str().unwrap_or_default().to_owned()
}

/// Reads `events`, in sequence order. Events the loop does not act on
/// (state changes, notes, escalations) are skipped.
pub fn of(events: &[StoredEvent]) -> Progress {
    let mut progress = Progress {
        context: None,
        history: Vec::new(),
        commands_queued: 0,
        next: Next::Context,
    };
    // The proposal being worked on.
    let mut current: Option<(String, ProposedCommand)> = None;
    for event in events {
        let p = &event.payload;
        match event.kind {
            EventKind::ContextSent => {
                progress.context = Some(text(&p["context"], "rendered"));
                progress.next = Next::Ask;
            }
            EventKind::AiProposal => match p["command"].as_str() {
                Some(command) => {
                    let proposal = ProposedCommand {
                        command: command.to_owned(),
                        reasoning: text(p, "reasoning"),
                    };
                    current = Some((text(p, "diagnosis"), proposal.clone()));
                    progress.next = Next::Classify {
                        seq: event.seq,
                        diagnosis: text(p, "diagnosis"),
                        proposal,
                    };
                }
                None => {
                    let outcome = match p["stop"].as_str() {
                        Some("resolved") => StopOutcome::Resolved,
                        _ => StopOutcome::Escalate,
                    };
                    progress.next = Next::Stopped {
                        outcome,
                        reason: text(p, "reason"),
                        at: event.created_at,
                    };
                }
            },
            EventKind::Approval => {
                progress.next = Next::Run {
                    command: text(p, "command"),
                    by_system: event.actor_kind == "system",
                    snapshot_done: false,
                };
            }
            EventKind::Snapshot => {
                if let Next::Run { snapshot_done, .. } = &mut progress.next {
                    *snapshot_done = true;
                }
            }
            EventKind::Rejection => {
                if let Some((diagnosis, proposal)) = current.take() {
                    let reason = match p["reason"].as_str() {
                        Some(reason) => format!("rejected by an approver: {reason}"),
                        None => "rejected by an approver".to_owned(),
                    };
                    progress.history.push(Exchange {
                        diagnosis,
                        proposal,
                        outcome: CommandOutcome::NotRun { reason },
                    });
                }
                progress.next = Next::Ask;
            }
            EventKind::CommandQueued => {
                progress.commands_queued += 1;
                progress.next = Next::Wait {
                    command_id: p["command_id"]
                        .as_str()
                        .and_then(|id| id.parse().ok())
                        .unwrap_or_default(),
                    command: text(p, "command"),
                };
            }
            EventKind::CommandResult => {
                let command = match &progress.next {
                    Next::Wait { command, .. } => command.clone(),
                    _ => String::new(),
                };
                if let Some((diagnosis, proposal)) = current.take() {
                    let exit_code = p["exit_code"].as_i64().and_then(|c| i32::try_from(c).ok());
                    progress.history.push(Exchange {
                        diagnosis,
                        proposal,
                        outcome: CommandOutcome::Executed {
                            command,
                            exit_code,
                            output: output(p),
                        },
                    });
                }
                progress.next = Next::Ask;
            }
            EventKind::Classification
            | EventKind::Edit
            | EventKind::Escalation
            | EventKind::StateChange
            | EventKind::Note => {}
        }
    }
    progress
}

/// The redacted output of a `command_result` payload as the AI sees it:
/// the end of each stream, and the error if any.
fn output(payload: &Value) -> String {
    let mut out = String::new();
    for stream in ["stdout", "stderr"] {
        let text = payload[stream].as_str().unwrap_or_default();
        out.push_str(&format!(
            "--- {stream} ---\n{}\n",
            tail(text, AI_OUTPUT_BYTES)
        ));
    }
    if let Some(error) = payload["error"].as_str() {
        out.push_str(&format!("--- error ---\n{error}\n"));
    }
    out
}

/// The last `max` bytes of `text`, after a marker when cut.
pub fn tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("[TRUNCATED: {start} bytes] {}", &text[start..])
}

/// The first `max` bytes of `text`, after which a marker says what was
/// cut: AI text is bounded before it is stored.
pub fn head(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{} [TRUNCATED: {} bytes]", &text[..end], text.len() - end)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::remediation::audit::{Actor, NewEvent};
    use crate::remediation::risk::{classify, RuleSet};

    fn stored(events: Vec<NewEvent>) -> Vec<StoredEvent> {
        events
            .into_iter()
            .zip(1..)
            .map(|(e, seq)| {
                let (actor_kind, actor_id) = match e.actor {
                    Actor::User(id) => ("user", id.to_string()),
                    Actor::System => ("system", "server".into()),
                    Actor::Ai(m) => ("ai", m),
                    Actor::Peer(p) => ("peer", p),
                };
                StoredEvent {
                    seq,
                    kind: e.kind,
                    actor_kind: actor_kind.into(),
                    actor_id,
                    payload: e.payload,
                    created_at: Utc::now(),
                    prev_hash: Vec::new(),
                    hash: Vec::new(),
                }
            })
            .collect()
    }

    fn context() -> NewEvent {
        NewEvent::context_sent("mock/m", json!({ "rendered": "BLOCKS", "redactions": {} }))
    }

    #[test]
    fn a_new_incident_needs_its_context_then_a_step() {
        assert_eq!(of(&[]).next, Next::Context);
        let progress = of(&stored(vec![context()]));
        assert_eq!(progress.next, Next::Ask);
        assert_eq!(progress.context.as_deref(), Some("BLOCKS"));
    }

    #[test]
    fn a_run_goes_from_proposal_to_history() {
        let id = Uuid::from_u128(7);
        let mut events = vec![
            context(),
            NewEvent::ai_proposal("mock/m", "disk full", "see usage", Some("df -h")),
        ];
        let progress = of(&stored(events.clone()));
        assert!(matches!(progress.next, Next::Classify { seq: 2, .. }));
        events.push(NewEvent::classification(
            "df -h",
            &classify("df -h", &RuleSet::default()),
        ));
        events.push(NewEvent::approval(Actor::System, "df -h"));
        let progress = of(&stored(events.clone()));
        assert_eq!(
            progress.next,
            Next::Run {
                command: "df -h".into(),
                by_system: true,
                snapshot_done: false
            }
        );
        events.push(NewEvent::snapshot("none", None, "no snapshots"));
        assert!(matches!(
            of(&stored(events.clone())).next,
            Next::Run {
                snapshot_done: true,
                ..
            }
        ));
        events.push(NewEvent::command_queued(id, "df -h"));
        let progress = of(&stored(events.clone()));
        assert_eq!(progress.commands_queued, 1);
        assert_eq!(
            progress.next,
            Next::Wait {
                command_id: id,
                command: "df -h".into()
            }
        );
        events.push(NewEvent::command_result(
            "agent",
            id,
            Some(0),
            "100%",
            "",
            None,
            Duration::from_millis(5),
        ));
        let progress = of(&stored(events));
        assert_eq!(progress.next, Next::Ask);
        assert_eq!(progress.history.len(), 1);
        let CommandOutcome::Executed {
            command,
            exit_code,
            output,
        } = &progress.history[0].outcome
        else {
            panic!("not executed");
        };
        assert_eq!((command.as_str(), *exit_code), ("df -h", Some(0)));
        assert_eq!(output, "--- stdout ---\n100%\n--- stderr ---\n\n");
        assert_eq!(progress.history[0].diagnosis, "disk full");
    }

    #[test]
    fn approvals_by_users_rejections_and_stops() {
        let user = Uuid::from_u128(1);
        let mut events = vec![
            context(),
            NewEvent::ai_proposal("mock/m", "d", "r", Some("systemctl restart nginx")),
            NewEvent::edit(user, "systemctl restart nginx", "systemctl reload nginx"),
            NewEvent::approval(Actor::User(user), "systemctl reload nginx"),
        ];
        assert_eq!(
            of(&stored(events.clone())).next,
            Next::Run {
                command: "systemctl reload nginx".into(),
                by_system: false,
                snapshot_done: false
            }
        );
        events.truncate(2);
        events.push(NewEvent::rejection(
            user,
            "systemctl restart nginx",
            Some("no"),
        ));
        let progress = of(&stored(events.clone()));
        assert_eq!(progress.next, Next::Ask);
        assert_eq!(
            progress.history[0].outcome,
            CommandOutcome::NotRun {
                reason: "rejected by an approver: no".into()
            }
        );
        events.push(NewEvent::ai_stop(
            "mock/m",
            "fixed",
            "resolved",
            "space freed",
        ));
        let Next::Stopped {
            outcome, reason, ..
        } = of(&stored(events)).next
        else {
            panic!("not stopped");
        };
        assert_eq!(
            (outcome, reason.as_str()),
            (StopOutcome::Resolved, "space freed")
        );
    }

    #[test]
    fn outputs_are_cut_at_their_end_on_char_boundaries() {
        let long = format!("{}é{}", "a".repeat(10), "b".repeat(AI_OUTPUT_BYTES));
        let cut = tail(&long, AI_OUTPUT_BYTES);
        assert!(cut.starts_with("[TRUNCATED: 12 bytes] b"), "{}", &cut[..30]);
        assert_eq!(tail("short", 10), "short");
        assert_eq!(head("ééé", 3), "é [TRUNCATED: 4 bytes]");
    }
}
