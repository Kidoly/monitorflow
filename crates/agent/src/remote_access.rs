//! `monitorflow-agent remote-access terminal on|off` (ADR-0178): allows
//! or refuses remote terminals on this host, `[remote] terminal` in the
//! configuration file. The Linux installer's `MF_REMOTE_TERMINAL` and the
//! MSI's `REMOTE_TERMINAL` run it.
//!
//! The file belongs to root (Linux) or to administrators (Windows), so
//! only they can run it: the agent's user cannot, and neither can the
//! central server. It leaves every other line, comments included, as it
//! was, and writes the file in place, so it keeps its owner and access
//! list, only when the result still loads. The agent reads the file at
//! start, so it must restart for the change to apply.

use std::path::Path;

use monitorflow_common::config::{self, AgentConfig, Env};

pub use crate::ai_commands::{AiCommandsError as RemoteAccessError, Outcome};

/// `text` with `remote.terminal` set to `on`.
pub fn edit(text: &str, on: bool, path: &Path) -> Result<String, RemoteAccessError> {
    edit_key(text, "terminal", Some(toml_edit::value(on)), path)
}

/// `text` with `remote.<key>` set to `value`, or removed with `None`.
fn edit_key(
    text: &str,
    key: &str,
    value: Option<toml_edit::Item>,
    path: &Path,
) -> Result<String, RemoteAccessError> {
    let mut document: toml_edit::DocumentMut =
        text.parse().map_err(|error| RemoteAccessError::Toml {
            path: path.to_owned(),
            error: Box::new(error),
        })?;
    let remote = document
        .entry("remote")
        .or_insert_with(toml_edit::table)
        .as_table_like_mut()
        .ok_or_else(|| RemoteAccessError::NotATable {
            path: path.to_owned(),
        })?;
    match value {
        Some(value) => {
            remote.insert(key, value);
        }
        None => {
            remote.remove(key);
        }
    }
    Ok(document.to_string())
}

/// Sets `remote.terminal` to `on` in the configuration file at `path`.
pub fn set(path: &Path, on: bool) -> Result<(Outcome, AgentConfig), RemoteAccessError> {
    write_key(path, "terminal", Some(toml_edit::value(on)))
}

/// Sets `remote.terminal_user` to `user` in the configuration file at
/// `path`, or removes it with `None`: terminals then run as the agent's
/// user. The configuration's checks refuse a name that is not a plain
/// account name.
pub fn set_user(
    path: &Path,
    user: Option<&str>,
) -> Result<(Outcome, AgentConfig), RemoteAccessError> {
    write_key(path, "terminal_user", user.map(toml_edit::value))
}

fn write_key(
    path: &Path,
    key: &str,
    value: Option<toml_edit::Item>,
) -> Result<(Outcome, AgentConfig), RemoteAccessError> {
    let text = std::fs::read_to_string(path).map_err(|error| RemoteAccessError::Read {
        path: path.to_owned(),
        error,
    })?;
    let edited = edit_key(&text, key, value, path)?;
    let config: AgentConfig = config::parse_with_env(&edited, path, &Env::new())?;
    if edited == text {
        return Ok((Outcome::Unchanged, config));
    }
    std::fs::write(path, edited).map_err(|error| RemoteAccessError::Write {
        path: path.to_owned(),
        error,
    })?;
    Ok((Outcome::Changed, config))
}

#[cfg(test)]
mod tests {
    use super::*;

    const WRITTEN: &str = "# Written by the installer.\nupstream_url = \"https://relay:7443\"\n";

    fn load(text: &str) -> AgentConfig {
        config::parse_with_env(text, Path::new("agent.toml"), &Env::new()).unwrap()
    }

    #[test]
    fn allows_terminals_and_takes_them_back_keeping_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.toml");
        std::fs::write(&path, WRITTEN).unwrap();
        assert!(!load(WRITTEN).remote.terminal);
        let (outcome, config) = set(&path, true).unwrap();
        assert_eq!(outcome, Outcome::Changed);
        assert!(config.remote.terminal);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(WRITTEN), "{text}");
        assert!(text.contains("[remote]\nterminal = true"), "{text}");
        assert_eq!(set(&path, true).unwrap().0, Outcome::Unchanged);
        assert_eq!(set(&path, false).unwrap().0, Outcome::Changed);
        assert!(
            !load(&std::fs::read_to_string(&path).unwrap())
                .remote
                .terminal
        );
    }

    #[test]
    fn chooses_the_account_and_refuses_what_is_not_a_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.toml");
        std::fs::write(&path, WRITTEN).unwrap();
        let (outcome, config) = set_user(&path, Some("ops")).unwrap();
        assert_eq!(outcome, Outcome::Changed);
        assert_eq!(config.remote.terminal_user.as_deref(), Some("ops"));
        assert_eq!(set_user(&path, Some("ops")).unwrap().0, Outcome::Unchanged);
        assert!(set_user(&path, Some("-oops")).is_err());
        assert_eq!(
            load(&std::fs::read_to_string(&path).unwrap())
                .remote
                .terminal_user
                .as_deref(),
            Some("ops")
        );
        assert_eq!(set_user(&path, None).unwrap().0, Outcome::Changed);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(load(&text).remote.terminal_user, None);
        assert!(text.starts_with(WRITTEN), "{text}");
    }

    #[test]
    fn edits_an_existing_table_and_refuses_what_it_cannot() {
        let text =
            format!("{WRITTEN}\n[remote] # here\nterminal = false\n\n[log]\nformat = \"json\"\n");
        let edited = edit(&text, true, Path::new("agent.toml")).unwrap();
        assert_eq!(edited, text.replace("terminal = false", "terminal = true"));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.toml");
        assert!(matches!(
            set(&path, true),
            Err(RemoteAccessError::Read { .. })
        ));
        for text in ["upstream_url = ", "remote = 1\n"] {
            std::fs::write(&path, text).unwrap();
            assert!(set(&path, true).is_err(), "{text}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        }
    }
}
