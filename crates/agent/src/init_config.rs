//! The configuration file the Windows MSI package writes at install time
//! (ADR-0050): the upstream, the enrollment endpoint and the one-time
//! token, every other key at its default. An existing file is never
//! replaced, so upgrades and repairs keep what the administrator edited.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use monitorflow_common::config::{self, AgentConfig, ConfigError, Env};
use monitorflow_common::redact::Secret;

/// The keys the installer sets.
#[derive(Debug, Clone)]
pub struct Settings {
    pub upstream_url: String,
    pub enroll_url: Option<String>,
    pub enrollment_token: Option<Secret>,
}

impl Settings {
    /// Empty values count as unset, as an MSI property that was not
    /// given expands to nothing.
    pub fn from_args(upstream_url: String, enroll_url: String, enrollment_token: String) -> Self {
        let set = |value: String| Some(value).filter(|value| !value.trim().is_empty());
        Self {
            upstream_url,
            enroll_url: set(enroll_url),
            enrollment_token: set(enrollment_token).map(Secret::new),
        }
    }
}

/// What [`write_new`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Written,
    /// The file already existed and was left as it is.
    Kept,
}

#[derive(Debug, thiserror::Error)]
pub enum InitError {
    /// The message never contains the token (see [`ConfigError::Parse`]).
    #[error(transparent)]
    Invalid(#[from] ConfigError),
    #[error("cannot write {}: {error}", path.display())]
    Write { path: PathBuf, error: io::Error },
}

/// The file's text. Values are quoted by the TOML serializer, so quotes
/// and backslashes in them stay as they are.
pub fn render(settings: &Settings) -> String {
    let quote = |value: &str| toml::Value::String(value.to_owned()).to_string();
    let mut text = String::from(
        "# Written by the MonitorFlow MSI package (ADR-0050). Every other key is\n\
         # described, with its default, in agent.windows.example.toml next to\n\
         # monitorflow-agent.exe.\n",
    );
    text += &format!("upstream_url = {}\n", quote(&settings.upstream_url));
    if let Some(url) = &settings.enroll_url {
        text += &format!("enroll_url = {}\n", quote(url));
    }
    if let Some(token) = &settings.enrollment_token {
        text += "# One-time token: remove it once agent.log says `identity loaded`.\n";
        text += &format!("enrollment_token = {}\n", quote(token.expose()));
    }
    text
}

/// Validates the configuration as the agent would load it, then creates
/// `path` with it, unless `path` exists. On Windows the file inherits the
/// ACL of its directory; on Unix it is created with mode 0600.
pub fn write_new(path: &Path, settings: &Settings) -> Result<Outcome, InitError> {
    let text = render(settings);
    config::parse_with_env::<AgentConfig>(&text, path, &Env::new())?;
    let write_error = |error| InitError::Write {
        path: path.to_owned(),
        error,
    };
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(Outcome::Kept),
        Err(error) => return Err(write_error(error)),
    };
    file.write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(write_error)?;
    Ok(Outcome::Written)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "mfe1.secret.fingerprint";

    fn settings(upstream: &str, enroll: &str, token: &str) -> Settings {
        Settings::from_args(upstream.into(), enroll.into(), token.into())
    }

    #[test]
    fn writes_a_configuration_the_agent_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.toml");
        let given = settings(
            "https://relay.example.com:7443",
            "https://monitorflow.example.com:7445",
            TOKEN,
        );
        assert_eq!(write_new(&path, &given).unwrap(), Outcome::Written);
        let loaded: AgentConfig = config::load_with_env(&path, &Env::new()).unwrap();
        assert_eq!(loaded.upstream_url, "https://relay.example.com:7443");
        assert_eq!(loaded.enroll_url(), "https://monitorflow.example.com:7445");
        assert_eq!(loaded.enrollment_token, Some(Secret::new(TOKEN)));
        assert_eq!(loaded.data_dir, AgentConfig::default().data_dir);
    }

    #[test]
    fn empty_values_are_left_out() {
        let text = render(&settings("https://relay:7443", "", " "));
        assert!(!text.contains("enroll_url"), "{text}");
        assert!(!text.contains("enrollment_token"), "{text}");
    }

    #[test]
    fn values_are_escaped() {
        let text = render(&settings("https://relay:7443", "", "a\"b\\c'd"));
        let loaded: AgentConfig =
            config::parse_with_env(&text, Path::new("agent.toml"), &Env::new()).unwrap();
        assert_eq!(loaded.enrollment_token, Some(Secret::new("a\"b\\c'd")));
    }

    #[test]
    fn keeps_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.toml");
        std::fs::write(&path, "edited").unwrap();
        let given = settings("https://relay:7443", "", TOKEN);
        assert_eq!(write_new(&path, &given).unwrap(), Outcome::Kept);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "edited");
    }

    #[test]
    fn refuses_an_invalid_url_without_echoing_the_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.toml");
        let error = write_new(&path, &settings("http://relay:7443", "", TOKEN)).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("upstream_url"), "{message}");
        assert!(!message.contains(TOKEN), "{message}");
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.toml");
        write_new(&path, &settings("https://relay:7443", "", TOKEN)).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
