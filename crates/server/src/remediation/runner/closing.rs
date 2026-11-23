//! Why a remediation incident closes. The event that closes it keeps the
//! English sentence in `reason` (or `text` for a resolved one), and adds a
//! stable `code`, with what varies in `detail` and `limit`, so clients can
//! word it in their own language.

use super::reach::DiagnosisOnly;
use crate::commands::acceptance::{Refusal, RootRefusal};

/// Why an incident closes; [`Self::code`] never changes for a variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CloseReason {
    /// The alert resolved.
    SensorNormal,
    /// `limit` seconds passed since it opened.
    DurationLimit(i64),
    /// `limit` commands ran.
    CommandLimit(i64),
    /// The AI stopped and handed over, saying why (untrusted text).
    AiEscalated(String),
    /// The AI says it is fixed, the sensor disagrees.
    NotFixed,
    AlertGone,
    ContextFailed(String),
    AiFailed(String),
    /// The rules forbid the AI's command, with the rule's reason.
    ForbiddenProposal(String),
    /// The rules forbid the command as approved, with the rule's reason.
    ApprovedForbidden(String),
    /// The rules changed: a command approved by policy now needs a person.
    NeedsApproval(String),
    /// The agent would refuse the command.
    Refused(Refusal),
    /// The agent would refuse the root action (ADR-0181).
    RootRefused(RootRefusal),
    SnapshotFailed(String),
    CommandGone,
    /// No result in time for the command.
    TimedOut(String),
    /// The command did not run to completion. Why is in the
    /// `command_result` just before, so it is not repeated (`detail` is
    /// absent); `reason` still says it, for older clients.
    Incomplete(String),
    /// The AI gave its diagnosis where no command can run (ADR-0175). The
    /// code is that of the reason, so a refusal keeps its own and gets the
    /// new attempt of ADR-0165; the event adds `diagnosis_only: true`.
    DiagnosisOnly(DiagnosisOnly),
}

impl CloseReason {
    /// Stable code for clients. Codes are only ever added.
    pub fn code(&self) -> &'static str {
        match self {
            Self::SensorNormal => "sensor_normal",
            Self::DurationLimit(_) => "duration_limit",
            Self::CommandLimit(_) => "command_limit",
            Self::AiEscalated(_) => "ai_escalated",
            Self::NotFixed => "not_fixed",
            Self::AlertGone => "alert_gone",
            Self::ContextFailed(_) => "context_failed",
            Self::AiFailed(_) => "ai_failed",
            Self::ForbiddenProposal(_) => "forbidden_proposal",
            Self::ApprovedForbidden(_) => "approved_forbidden",
            Self::NeedsApproval(_) => "needs_approval",
            Self::Refused(refusal) => refusal.code(),
            Self::RootRefused(refusal) => refusal.code(),
            Self::SnapshotFailed(_) => "snapshot_failed",
            Self::CommandGone => "command_gone",
            Self::TimedOut(_) => "timed_out",
            Self::Incomplete(_) => "incomplete",
            Self::DiagnosisOnly(why) => why.code(),
        }
    }

    /// The English sentence, as `reason` always held it.
    pub fn text(&self) -> String {
        match self {
            Self::SensorNormal => "the sensor returned to normal".into(),
            Self::DurationLimit(s) => format!("the duration limit of {s} s was reached"),
            Self::CommandLimit(n) => format!("the command limit of {n} was reached"),
            Self::AiEscalated(why) => format!("the AI escalated: {why}"),
            Self::NotFixed => {
                "the AI considers the problem fixed, but the sensor still breaches its threshold"
                    .into()
            }
            Self::AlertGone => "the alert that opened the incident is gone".into(),
            Self::ContextFailed(e) => format!("the context could not be gathered: {e}"),
            Self::AiFailed(e) => format!("the AI provider failed: {e}"),
            Self::ForbiddenProposal(r) => format!("the AI proposed a forbidden command ({r})"),
            Self::ApprovedForbidden(r) => format!("the approved command is forbidden ({r})"),
            Self::NeedsApproval(r) => {
                format!("the command now needs approval ({r}); the rules changed")
            }
            Self::Refused(refusal) => refusal.message().into(),
            Self::RootRefused(refusal) => refusal.message().into(),
            Self::SnapshotFailed(e) => format!("the snapshot before the change failed: {e}"),
            Self::CommandGone => "the queued command is gone".into(),
            Self::TimedOut(command) => format!("the command timed out: {command}"),
            Self::Incomplete(why) => format!("the command did not run to completion: {why}"),
            Self::DiagnosisOnly(why) => {
                format!(
                    "the AI diagnosed the incident without commands: {}",
                    why.message()
                )
            }
        }
    }

    /// The part of the sentence that varies, words a client cannot
    /// translate (an error, a rule's reason, the AI's words, a command).
    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::AiEscalated(s)
            | Self::ContextFailed(s)
            | Self::AiFailed(s)
            | Self::ForbiddenProposal(s)
            | Self::ApprovedForbidden(s)
            | Self::NeedsApproval(s)
            | Self::SnapshotFailed(s)
            | Self::TimedOut(s) => Some(s),
            _ => None,
        }
    }

    /// The number in the sentence, for the limits.
    pub fn limit(&self) -> Option<i64> {
        match self {
            Self::DurationLimit(n) | Self::CommandLimit(n) => Some(*n),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_english_sentence_and_adds_a_code() {
        let cases = [
            (
                CloseReason::DurationLimit(2),
                "duration_limit",
                "the duration limit of 2 s was reached",
                None,
                Some(2),
            ),
            (
                CloseReason::CommandLimit(1),
                "command_limit",
                "the command limit of 1 was reached",
                None,
                Some(1),
            ),
            (
                CloseReason::AiEscalated("grow the volume".into()),
                "ai_escalated",
                "the AI escalated: grow the volume",
                Some("grow the volume"),
                None,
            ),
            (
                CloseReason::Incomplete("refused".into()),
                "incomplete",
                "the command did not run to completion: refused",
                None,
                None,
            ),
            (
                CloseReason::DiagnosisOnly(DiagnosisOnly::Refused(Refusal::Off { windows: false })),
                "commands_off",
                "the AI diagnosed the incident without commands: AI commands are off on this \
                 server; the Commands tab of its page turns them on, or gives the line to run \
                 there (MF_ALLOW_COMMANDS=1)",
                None,
                None,
            ),
            (
                CloseReason::DiagnosisOnly(DiagnosisOnly::Device),
                "device",
                "the AI diagnosed the incident without commands: this is a device without an \
                 agent, so no command can run on it",
                None,
                None,
            ),
            (
                CloseReason::Refused(Refusal::Off { windows: true }),
                "commands_off_windows",
                Refusal::Off { windows: true }.message(),
                None,
                None,
            ),
            (
                CloseReason::RootRefused(RootRefusal::SudoRules),
                "root_actions_sudo_rules",
                RootRefusal::SudoRules.message(),
                None,
                None,
            ),
        ];
        for (reason, code, text, detail, limit) in cases {
            assert_eq!(reason.code(), code);
            assert_eq!(reason.text(), text);
            assert_eq!(reason.detail(), detail);
            assert_eq!(reason.limit(), limit);
        }
    }
}
