//! What each server's agent does with each kind of command (ADR-0076),
//! stored from its `Hello` in `servers.command_kinds`.
//!
//! A kind missing from the report, like the whole report from an agent or
//! relay before ADR-0076, is unknown: neither accepted nor refused. Only a
//! reported refusal stops anything here; the agent decides in the end.

use std::collections::BTreeMap;

use monitorflow_common::command;
use monitorflow_common::proto::v1::{CommandAcceptance, Hello};
use sqlx::PgExecutor;
use uuid::Uuid;

const ACCEPTED: &str = "accepted";
const OFF: &str = "off";
const UNSUPPORTED: &str = "unsupported";

/// The kinds stored. An agent names only what it runs, but its report
/// passes through relays, so nothing else reaches the database.
const KINDS: [&str; 5] = [
    command::APPLY_UPDATES,
    command::CONFIGURE_SOURCES,
    command::REVERT_SOURCES,
    command::SELF_UPDATE,
    command::SHELL,
];

/// `hello`'s report as stored, `None` when it holds none. Values this
/// server does not know are left out: a newer agent is never refused for
/// one, and its state stays unknown.
pub fn from_hello(hello: &Hello) -> Option<BTreeMap<&'static str, &'static str>> {
    if hello.command_kinds.is_empty() {
        return None;
    }
    let states = hello.command_kinds.iter().filter_map(|state| {
        let kind = KINDS.into_iter().find(|kind| *kind == state.kind)?;
        let word = match CommandAcceptance::try_from(state.acceptance).ok()? {
            CommandAcceptance::Accepted => ACCEPTED,
            CommandAcceptance::Disabled => OFF,
            CommandAcceptance::Unsupported => UNSUPPORTED,
            CommandAcceptance::Unspecified => return None,
        };
        Some((kind, word))
    });
    Some(states.collect())
}

/// Why a server's agent would refuse a `shell` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Its administrator has not allowed them.
    Off { windows: bool },
    /// Its agent cannot run them: on Windows, one older than ADR-0084.
    Unsupported { windows: bool },
}

impl Refusal {
    /// `word`, a stored state, on a server of `os_family`; `None` unless
    /// it is a refusal.
    fn of(word: Option<&str>, os_family: &str) -> Option<Self> {
        let windows = os_family == "windows";
        match word {
            Some(OFF) => Some(Self::Off { windows }),
            Some(UNSUPPORTED) => Some(Self::Unsupported { windows }),
            _ => None,
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::Off { windows: false } => {
                "AI commands are off on this server; run the installer there with \
                 MF_ALLOW_COMMANDS=1 to allow them"
            }
            Self::Off { windows: true } => {
                "AI commands are off on this server; run `monitorflow-agent.exe ai-commands on` \
                 there as an administrator, then restart the MonitorFlowAgent service"
            }
            Self::Unsupported { windows: false } => {
                "this server's system does not run AI commands yet"
            }
            Self::Unsupported { windows: true } => {
                "this server's agent is too old to run AI commands; it updates by itself"
            }
        }
    }
}

/// The refusal `server_id` last reported for `shell` commands, if any.
pub async fn shell_refusal(
    executor: impl PgExecutor<'_>,
    server_id: Uuid,
) -> Result<Option<Refusal>, sqlx::Error> {
    let row: Option<(Option<String>, String)> =
        sqlx::query_as("SELECT command_kinds->>'shell', os_family FROM servers WHERE id = $1")
            .bind(server_id)
            .fetch_optional(executor)
            .await?;
    Ok(row.and_then(|(word, os_family)| Refusal::of(word.as_deref(), &os_family)))
}

#[cfg(test)]
mod tests {
    use monitorflow_common::proto::v1::CommandKindState;

    use super::*;

    fn state(kind: &str, acceptance: i32) -> CommandKindState {
        CommandKindState {
            kind: kind.into(),
            acceptance,
        }
    }

    #[test]
    fn no_report_is_not_an_empty_one() {
        assert_eq!(from_hello(&Hello::default()), None);
    }

    #[test]
    fn reports_keep_what_this_server_knows() {
        let hello = Hello {
            command_kinds: vec![
                state("shell", CommandAcceptance::Disabled.into()),
                state("configure_sources", CommandAcceptance::Unsupported.into()),
                state("apply_updates", CommandAcceptance::Accepted.into()),
                state("self_update", CommandAcceptance::Unspecified.into()),
                state("revert_sources", 99),
                state("reboot", CommandAcceptance::Accepted.into()),
            ],
            ..Hello::default()
        };
        let stored = from_hello(&hello).unwrap();
        assert_eq!(
            stored,
            BTreeMap::from([
                ("apply_updates", ACCEPTED),
                ("configure_sources", UNSUPPORTED),
                ("shell", OFF),
            ])
        );
    }

    #[test]
    fn only_a_reported_refusal_refuses() {
        let off = Refusal::Off { windows: false };
        assert_eq!(Refusal::of(Some(OFF), "debian"), Some(off));
        assert_eq!(
            Refusal::of(Some(UNSUPPORTED), "debian"),
            Some(Refusal::Unsupported { windows: false })
        );
        assert_eq!(Refusal::of(Some(ACCEPTED), "debian"), None);
        assert_eq!(Refusal::of(Some("from a newer server"), "debian"), None);
        assert_eq!(Refusal::of(None, "windows"), None);
    }

    /// The way to turn AI commands on is the one of the server's system.
    #[test]
    fn messages_follow_the_system() {
        let linux = Refusal::of(Some(OFF), "rhel").unwrap().message();
        assert!(linux.contains("MF_ALLOW_COMMANDS=1"), "{linux}");
        let windows = Refusal::of(Some(OFF), "windows").unwrap().message();
        assert!(windows.contains("ai-commands on"), "{windows}");
        assert!(!windows.contains("MF_ALLOW_COMMANDS"), "{windows}");
        let old = Refusal::of(Some(UNSUPPORTED), "windows").unwrap().message();
        assert!(old.contains("too old"), "{old}");
    }
}
