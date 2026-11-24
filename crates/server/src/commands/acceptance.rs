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
const KINDS: [&str; 9] = [
    command::AI_COMMANDS,
    command::APPLY_UPDATES,
    command::CONFIGURE_SOURCES,
    command::MAJOR_UPGRADE,
    command::REVERT_SOURCES,
    command::ROOT_ACTION,
    command::RUN_SCRIPT,
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

    /// Stable code of [`Self::message`], for clients to word it.
    pub fn code(self) -> &'static str {
        match self {
            Self::Off { windows: false } => "commands_off",
            Self::Off { windows: true } => "commands_off_windows",
            Self::Unsupported { windows: false } => "system_unsupported",
            Self::Unsupported { windows: true } => "agent_too_old",
        }
    }

    /// The refusal of [`Self::code`] `code`, `None` for another code.
    pub fn from_code(code: &str) -> Option<Self> {
        [false, true]
            .into_iter()
            .flat_map(|windows| [Self::Off { windows }, Self::Unsupported { windows }])
            .find(|refusal| refusal.code() == code)
    }

    pub fn message(self) -> &'static str {
        match self {
            // The Commands tab of the server's page has the switch, or the
            // one line to run on the server when it cannot be turned on
            // from there (ADR-0098, ADR-0175).
            Self::Off { windows: false } => {
                "AI commands are off on this server; the Commands tab of its page turns them \
                 on, or gives the line to run there (MF_ALLOW_COMMANDS=1)"
            }
            Self::Off { windows: true } => {
                "AI commands are off on this server; the Commands tab of its page turns them \
                 on, or gives the command to run there (`monitorflow-agent.exe ai-commands on`)"
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

/// Why a server's agent would refuse a `root_action` command (ADR-0181).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootRefusal {
    /// AI commands are off, or the host did not allow root actions.
    Off,
    /// The sudo rules predate root actions.
    SudoRules,
    /// A Windows server: root actions are Linux only.
    Windows,
}

impl RootRefusal {
    fn of(word: Option<&str>, os_family: &str) -> Option<Self> {
        match word {
            Some(OFF) => Some(Self::Off),
            Some(UNSUPPORTED) if os_family == "windows" => Some(Self::Windows),
            Some(UNSUPPORTED) => Some(Self::SudoRules),
            _ => None,
        }
    }

    /// Stable code of [`Self::message`], for clients to word it.
    pub fn code(self) -> &'static str {
        match self {
            Self::Off => "root_actions_off",
            Self::SudoRules => "root_actions_sudo_rules",
            Self::Windows => "root_actions_windows",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::Off => {
                "AI root actions are not allowed on this server; run `sudo monitorflow-agent \
                 ai-commands root-actions on` there, or the installer with MF_AI_ROOT_ACTIONS=1"
            }
            Self::SudoRules => {
                "the sudo rules on this server predate AI root actions; run the install command \
                 again"
            }
            Self::Windows => "AI root actions run on Linux servers only",
        }
    }
}

/// What `server_id`'s agent last reported for `kind`: `accepted`, `off`,
/// `unsupported`, or `None` when unknown.
pub async fn reported(
    executor: impl PgExecutor<'_>,
    server_id: Uuid,
    kind: &str,
) -> Result<Option<String>, sqlx::Error> {
    let word: Option<Option<String>> =
        sqlx::query_scalar("SELECT command_kinds->>$2 FROM servers WHERE id = $1")
            .bind(server_id)
            .bind(kind)
            .fetch_optional(executor)
            .await?;
    Ok(word.flatten())
}

/// The refusal `server_id` last reported for `root_action` commands, if
/// any. An agent that does not report the kind is not refused here: it
/// refuses the command itself.
pub async fn root_refusal(
    executor: impl PgExecutor<'_>,
    server_id: Uuid,
) -> Result<Option<RootRefusal>, sqlx::Error> {
    let row: Option<(Option<String>, String)> = sqlx::query_as(
        "SELECT command_kinds->>'root_action', os_family FROM servers WHERE id = $1",
    )
    .bind(server_id)
    .fetch_optional(executor)
    .await?;
    Ok(row.and_then(|(word, os_family)| RootRefusal::of(word.as_deref(), &os_family)))
}

/// Why a server's agent would refuse library scripts (ADR-0179), as it
/// last reported, with the way to allow them on its system; `None` when
/// it accepts them or never said.
pub fn script_refusal(word: Option<&str>, os_family: &str) -> Option<&'static str> {
    let windows = os_family == "windows";
    match word {
        Some(OFF) if windows => Some(
            "library scripts are off on this server; run `monitorflow-agent.exe scripts on` \
             there as an administrator, or the MSI with ALLOW_SCRIPTS=1",
        ),
        Some(OFF) => Some(
            "library scripts are off on this server; run the installer there with \
             MF_ALLOW_SCRIPTS=1",
        ),
        Some(UNSUPPORTED) => Some("this server's agent cannot run library scripts"),
        _ => None,
    }
}

/// [`script_refusal`] of what `server_id` last reported.
pub async fn script_refusal_of(
    executor: impl PgExecutor<'_>,
    server_id: Uuid,
) -> Result<Option<&'static str>, sqlx::Error> {
    let row: Option<(Option<String>, String)> =
        sqlx::query_as("SELECT command_kinds->>'run_script', os_family FROM servers WHERE id = $1")
            .bind(server_id)
            .fetch_optional(executor)
            .await?;
    Ok(row.and_then(|(word, os_family)| script_refusal(word.as_deref(), &os_family)))
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
    fn a_refusal_reads_back_from_its_code() {
        for windows in [false, true] {
            for refusal in [Refusal::Off { windows }, Refusal::Unsupported { windows }] {
                assert_eq!(Refusal::from_code(refusal.code()), Some(refusal));
            }
        }
        assert_eq!(Refusal::from_code("device"), None);
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
                state("ai_commands", CommandAcceptance::Disabled.into()),
                state("root_action", CommandAcceptance::Unsupported.into()),
            ],
            ..Hello::default()
        };
        let stored = from_hello(&hello).unwrap();
        assert_eq!(
            stored,
            BTreeMap::from([
                ("ai_commands", OFF),
                ("apply_updates", ACCEPTED),
                ("configure_sources", UNSUPPORTED),
                ("root_action", UNSUPPORTED),
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

    #[test]
    fn root_refusals_say_what_to_do_on_the_host() {
        assert_eq!(RootRefusal::of(Some(OFF), "debian"), Some(RootRefusal::Off));
        assert_eq!(
            RootRefusal::of(Some(UNSUPPORTED), "rhel"),
            Some(RootRefusal::SudoRules)
        );
        assert_eq!(
            RootRefusal::of(Some(UNSUPPORTED), "windows"),
            Some(RootRefusal::Windows)
        );
        assert_eq!(RootRefusal::of(Some(ACCEPTED), "debian"), None);
        assert_eq!(RootRefusal::of(None, "debian"), None);
        assert!(RootRefusal::Off.message().contains("root-actions on"));
        assert!(RootRefusal::SudoRules
            .message()
            .contains("run the install command again"));
    }

    #[test]
    fn script_refusals_name_the_way_to_allow_them() {
        assert!(script_refusal(Some(OFF), "debian")
            .unwrap()
            .contains("MF_ALLOW_SCRIPTS=1"));
        assert!(script_refusal(Some(OFF), "windows")
            .unwrap()
            .contains("scripts on"));
        assert_eq!(script_refusal(Some(ACCEPTED), "debian"), None);
        assert_eq!(script_refusal(None, "debian"), None);
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
