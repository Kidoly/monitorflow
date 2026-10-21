//! Commands for agents (ADR-0031): the `commands` table, the dispatcher
//! that signs and sends them on agents' streams, and their results.
//!
//! A command is `queued`, then `sent` on the agent's stream, `running` once
//! the agent says it started, and ends `succeeded`, `failed`, `expired` or
//! `cancelled`. At most one command per server is `sent` or `running`.
//! Package reports from agents are stored here too.

mod dispatcher;
mod packages;
mod store;

use chrono::{DateTime, Utc};
use monitorflow_common::proto::v1::{CommandResult, CommandStatus};
use serde::Serialize;
use uuid::Uuid;

pub use dispatcher::{Commands, DispatcherSettings};
pub use packages::{PackageStore, MAX_PACKAGES};
pub use store::{CommandStore, NewCommand};

/// Longest `stdout` or `stderr` stored, in bytes; the end is kept.
pub const MAX_OUTPUT: usize = 64 * 1024;
/// Longest `error` stored, in bytes.
pub const MAX_ERROR: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum State {
    Queued,
    Sent,
    Running,
    Succeeded,
    Failed,
    Expired,
    Cancelled,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Sent => "sent",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Expired => "expired",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn is_final(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Expired | Self::Cancelled
        )
    }
}

/// A row of `commands`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct CommandRow {
    pub id: Uuid,
    pub server_id: Uuid,
    pub kind: String,
    #[serde(skip)]
    pub payload: Vec<u8>,
    pub state: State,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub sent_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub exit_code: Option<i32>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub error: Option<String>,
    pub requested_by: Option<Uuid>,
    pub campaign_id: Option<Uuid>,
}

/// What a [`CommandResult`] does to its command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The agent started it.
    Running,
    Final {
        state: State,
        /// Whether the command ran at all, as opposed to being refused.
        ran: bool,
        exit_code: i32,
        stdout: String,
        stderr: String,
        error: Option<String>,
    },
}

impl Outcome {
    pub fn of(result: &CommandResult) -> Self {
        let status = CommandStatus::try_from(result.status).unwrap_or_default();
        let (state, ran) = match status {
            CommandStatus::Running => return Self::Running,
            CommandStatus::Succeeded => (State::Succeeded, true),
            CommandStatus::Failed => (State::Failed, true),
            CommandStatus::Expired => (State::Expired, false),
            CommandStatus::Rejected => (State::Failed, false),
            CommandStatus::Unspecified if result.exit_code == 0 => (State::Succeeded, true),
            CommandStatus::Unspecified => (State::Failed, true),
        };
        let error = (!result.error.is_empty()).then(|| text(result.error.as_bytes(), MAX_ERROR));
        Self::Final {
            state,
            ran,
            exit_code: result.exit_code,
            stdout: text(&result.stdout, MAX_OUTPUT),
            stderr: text(&result.stderr, MAX_OUTPUT),
            error,
        }
    }
}

/// The last `max` bytes of `bytes` as text: invalid UTF-8 and NUL, which
/// PostgreSQL text refuses, become U+FFFD.
fn text(bytes: &[u8], max: usize) -> String {
    let tail = &bytes[bytes.len().saturating_sub(max)..];
    String::from_utf8_lossy(tail).replace('\0', "\u{FFFD}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(status: CommandStatus, exit_code: i32) -> CommandResult {
        CommandResult {
            command_id: "c".into(),
            exit_code,
            status: status as i32,
            ..CommandResult::default()
        }
    }

    fn state(outcome: Outcome) -> (State, bool) {
        match outcome {
            Outcome::Final { state, ran, .. } => (state, ran),
            Outcome::Running => panic!("not final"),
        }
    }

    #[test]
    fn statuses_map_to_states() {
        assert_eq!(
            Outcome::of(&result(CommandStatus::Running, 0)),
            Outcome::Running
        );
        let cases = [
            (CommandStatus::Succeeded, 0, State::Succeeded, true),
            (CommandStatus::Failed, 100, State::Failed, true),
            (CommandStatus::Expired, 0, State::Expired, false),
            (CommandStatus::Rejected, 0, State::Failed, false),
            (CommandStatus::Unspecified, 0, State::Succeeded, true),
            (CommandStatus::Unspecified, 1, State::Failed, true),
        ];
        for (status, code, expected, ran) in cases {
            assert_eq!(
                state(Outcome::of(&result(status, code))),
                (expected, ran),
                "{status:?}"
            );
        }
        // Unknown future statuses read as unspecified.
        let mut unknown = result(CommandStatus::Succeeded, 0);
        unknown.status = 99;
        assert_eq!(state(Outcome::of(&unknown)), (State::Succeeded, true));
    }

    #[test]
    fn output_keeps_the_end_and_becomes_valid_text() {
        let mut long = vec![b'a'; MAX_OUTPUT];
        long.extend_from_slice(b"end\0\xff");
        let mut r = result(CommandStatus::Failed, 1);
        r.stdout = long;
        r.error = "x".repeat(MAX_ERROR + 10);
        let Outcome::Final { stdout, error, .. } = Outcome::of(&r) else {
            panic!()
        };
        assert!(
            stdout.ends_with("end\u{FFFD}\u{FFFD}"),
            "{}",
            &stdout[stdout.len() - 10..]
        );
        assert!(stdout.len() <= MAX_OUTPUT + 4);
        assert_eq!(error.unwrap().len(), MAX_ERROR);
        let Outcome::Final { error, .. } = Outcome::of(&result(CommandStatus::Succeeded, 0)) else {
            panic!()
        };
        assert_eq!(error, None);
    }

    #[test]
    fn final_states() {
        assert!(!State::Queued.is_final() && !State::Sent.is_final() && !State::Running.is_final());
        assert!(
            State::Succeeded.is_final() && State::Expired.is_final() && State::Cancelled.is_final()
        );
        assert_eq!(State::Running.as_str(), "running");
    }
}
