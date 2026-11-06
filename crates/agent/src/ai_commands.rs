//! `monitorflow-agent ai-commands on|off` (ADR-0084): turns AI commands
//! on or off in the configuration file, the Windows counterpart of the
//! Linux installer's `MF_ALLOW_COMMANDS` (ADR-0064). `ai-commands from-web
//! on|off` lets the web interface turn them on, or stops it, the
//! counterpart of `MF_AI_COMMANDS_FROM_WEB` (ADR-0098).
//!
//! It sets `[commands] shell` or `from_web` and leaves every other line,
//! comments included, as it was. The file is rewritten in place, so it keeps its
//! access list, and only when the result still loads. The agent reads it
//! at start, so the service must restart for it to apply.

use std::path::{Path, PathBuf};

use monitorflow_common::config::{self, AgentConfig, ConfigError, Env};

/// What [`set`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Changed,
    /// The file already said so.
    Unchanged,
}

#[derive(Debug, thiserror::Error)]
pub enum AiCommandsError {
    #[error("cannot read {}: {error}", path.display())]
    Read {
        path: PathBuf,
        error: std::io::Error,
    },
    #[error("{} is not valid TOML: {error}", path.display())]
    Toml {
        path: PathBuf,
        error: Box<toml_edit::TomlError>,
    },
    #[error("{}: `commands` is not a table", path.display())]
    NotATable { path: PathBuf },
    /// The message never contains secrets (see [`ConfigError::Parse`]).
    #[error(transparent)]
    Invalid(#[from] ConfigError),
    #[error("cannot write {}: {error}", path.display())]
    Write {
        path: PathBuf,
        error: std::io::Error,
    },
}

/// A key of `[commands]` these commands set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// `shell`: AI commands themselves.
    Shell,
    /// `from_web`: whether the web interface may turn them on.
    FromWeb,
}

impl Key {
    fn name(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::FromWeb => "from_web",
        }
    }

    fn get(self, config: &AgentConfig) -> bool {
        match self {
            Self::Shell => config.commands.shell,
            Self::FromWeb => config.commands.from_web,
        }
    }
}

/// `text` with `commands.<key>` set to `on`.
pub fn edit(text: &str, key: Key, on: bool, path: &Path) -> Result<String, AiCommandsError> {
    let mut document: toml_edit::DocumentMut =
        text.parse().map_err(|error| AiCommandsError::Toml {
            path: path.to_owned(),
            error: Box::new(error),
        })?;
    let commands = document
        .entry("commands")
        .or_insert_with(toml_edit::table)
        .as_table_like_mut()
        .ok_or_else(|| AiCommandsError::NotATable {
            path: path.to_owned(),
        })?;
    commands.insert(key.name(), toml_edit::value(on));
    Ok(document.to_string())
}

/// Sets `commands.<key>` to `on` in the configuration file at `path`.
/// Also returns the configuration as it now reads.
pub fn set(path: &Path, key: Key, on: bool) -> Result<(Outcome, AgentConfig), AiCommandsError> {
    let text = std::fs::read_to_string(path).map_err(|error| AiCommandsError::Read {
        path: path.to_owned(),
        error,
    })?;
    let edited = edit(&text, key, on, path)?;
    let config: AgentConfig = config::parse_with_env(&edited, path, &Env::new())?;
    if key.get(&config) == on && edited == text {
        return Ok((Outcome::Unchanged, config));
    }
    std::fs::write(path, edited).map_err(|error| AiCommandsError::Write {
        path: path.to_owned(),
        error,
    })?;
    Ok((Outcome::Changed, config))
}

#[cfg(test)]
mod tests {
    use super::*;

    const WRITTEN: &str = "# Written by the MSI.\nupstream_url = \"https://relay:7443\"\n";

    fn load(text: &str) -> AgentConfig {
        config::parse_with_env(text, Path::new("agent.toml"), &Env::new()).unwrap()
    }

    #[test]
    fn turns_commands_on_and_off_keeping_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.toml");
        std::fs::write(&path, WRITTEN).unwrap();
        assert!(!load(WRITTEN).commands.shell);
        assert_eq!(set(&path, Key::Shell, true).unwrap().0, Outcome::Changed);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(WRITTEN), "{text}");
        assert!(load(&text).commands.shell);
        assert_eq!(set(&path, Key::Shell, true).unwrap().0, Outcome::Unchanged);
        assert_eq!(set(&path, Key::Shell, false).unwrap().0, Outcome::Changed);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!load(&text).commands.shell);
        assert!(text.starts_with(WRITTEN), "{text}");
    }

    #[test]
    fn allows_the_web_interface_and_takes_it_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.toml");
        std::fs::write(&path, WRITTEN).unwrap();
        let (outcome, config) = set(&path, Key::FromWeb, true).unwrap();
        assert_eq!(outcome, Outcome::Changed);
        assert!(config.commands.from_web && !config.commands.shell);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("from_web = true"), "{text}");
        assert_eq!(set(&path, Key::FromWeb, false).unwrap().0, Outcome::Changed);
        assert!(
            !load(&std::fs::read_to_string(&path).unwrap())
                .commands
                .from_web
        );
    }

    #[test]
    fn edits_an_existing_table() {
        let text =
            format!("{WRITTEN}\n[commands] # AI\nshell = false\n\n[log]\nformat = \"json\"\n");
        let edited = edit(&text, Key::Shell, true, Path::new("agent.toml")).unwrap();
        assert_eq!(edited, text.replace("shell = false", "shell = true"));
        let inline = format!("{WRITTEN}commands = {{ shell = false }}\n");
        assert!(
            load(&edit(&inline, Key::Shell, true, Path::new("a")).unwrap())
                .commands
                .shell
        );
    }

    #[test]
    fn refuses_what_it_cannot_edit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.toml");
        assert!(matches!(
            set(&path, Key::Shell, true),
            Err(AiCommandsError::Read { .. })
        ));
        for text in ["upstream_url = ", "commands = 1\n"] {
            std::fs::write(&path, text).unwrap();
            assert!(set(&path, Key::Shell, true).is_err(), "{text}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        }
        // Valid TOML the agent would not load is left as it was.
        std::fs::write(&path, "upstream_url = 3\n").unwrap();
        assert!(matches!(
            set(&path, Key::Shell, true),
            Err(AiCommandsError::Invalid(_))
        ));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "upstream_url = 3\n"
        );
    }
}
