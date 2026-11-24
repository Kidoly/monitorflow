//! `monitorflow-agent remote-shell` (ADR-0190): the root helper that
//! starts a remote terminal's login shell as the account the host's
//! administrator chose, `remote.terminal_user`, never root unless named
//! there.
//!
//! The agent runs it through an exact sudo rule, with the signed
//! `remote_session` command on a pipe as its stdin and the terminal as its
//! stdout and stderr. Like every helper (ADR-0108), it believes nothing the
//! agent's user says: it checks the command against its own copy of the
//! certificate authority, for this machine, unexpired and never used, and
//! reads the account from `agent.toml`, which root owns. Then it puts the
//! terminal on its stdin too and becomes `runuser -l <account>`.

use std::convert::Infallible;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

use monitorflow_common::command::REMOTE_SESSION;
use monitorflow_common::config::{valid_terminal_user, AgentConfig};

use crate::helper_trust::{authorize, HelperRequest, Place};

/// Where `runuser` is, on Debian, Ubuntu, RHEL and Rocky.
const RUNUSER: [&str; 2] = ["/usr/sbin/runuser", "/sbin/runuser"];

/// The account the configuration names, once it allows terminals.
pub fn account(config: &AgentConfig) -> Result<String, String> {
    if !config.remote.terminal {
        return Err("remote terminals are not allowed on this server (remote.terminal)".into());
    }
    match &config.remote.terminal_user {
        Some(user) if valid_terminal_user(user) => Ok(user.clone()),
        Some(_) => Err("remote.terminal_user is not a valid account name".into()),
        None => Err(
            "remote.terminal_user is not set: terminals run as the agent's user, without this helper"
                .into(),
        ),
    }
}

/// `runuser -l <user>`: a login shell of `user`, with that account's
/// environment, and the terminal type kept.
pub fn login(runuser: &Path, user: &str) -> Command {
    let mut command = Command::new(runuser);
    command
        .env_clear()
        .env("TERM", "xterm-256color")
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .args(["-l", user]);
    command
}

/// Checks the request on stdin and becomes the account's login shell.
/// Returns only when something refused it.
pub fn run(config_path: &Path, now_unix_ms: i64) -> Result<Infallible, String> {
    let request = HelperRequest::read(&mut std::io::stdin().lock()).map_err(|e| e.to_string())?;
    let config: AgentConfig =
        monitorflow_common::config::load(config_path).map_err(|e| e.to_string())?;
    let user = account(&config)?;
    let place = Place::agent(&config.tls_paths());
    authorize(&place, &request, REMOTE_SESSION, now_unix_ms).map_err(|e| e.to_string())?;
    let runuser = RUNUSER
        .iter()
        .map(Path::new)
        .find(|p| p.exists())
        .ok_or("runuser is not installed (util-linux)")?;
    // The terminal on stdout becomes stdin too: the pipe carried only the
    // request.
    rustix::stdio::dup2_stdin(std::io::stdout()).map_err(|e| e.to_string())?;
    let error = login(runuser, &user).exec();
    Err(format!("cannot start a shell as {user}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(terminal: bool, user: Option<&str>) -> AgentConfig {
        let mut config = AgentConfig::default();
        config.remote.terminal = terminal;
        config.remote.terminal_user = user.map(Into::into);
        config
    }

    #[test]
    fn the_account_comes_from_the_hosts_configuration_only() {
        assert_eq!(account(&config(true, Some("ops"))).unwrap(), "ops");
        assert!(account(&config(false, Some("ops")))
            .unwrap_err()
            .contains("remote.terminal"));
        assert!(account(&config(true, None))
            .unwrap_err()
            .contains("not set"));
        assert!(account(&config(true, Some("-c id"))).is_err());
    }

    #[test]
    fn the_shell_is_a_login_of_that_account() {
        let command = login(Path::new("/usr/sbin/runuser"), "ops");
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["-l", "ops"]);
        let env: Vec<_> = command
            .get_envs()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        assert!(env.contains(&"TERM".to_owned()), "{env:?}");
        assert!(!env.iter().any(|k| k.starts_with("MONITORFLOW")), "{env:?}");
    }
}
