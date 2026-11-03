//! The `shell` command of the AI remediation loop (ADR-0043): one command
//! line run as the agent's user, with `/bin/sh -c` on Unix and Windows
//! PowerShell on Windows (ADR-0084).
//!
//! The server sends only command lines its risk rules let through (or an
//! approver accepted), signed like every command, and classifies them by
//! the rules of the shell that runs them. The agent adds what it
//! controls: the kind is refused unless `commands.shell` is enabled, the
//! environment is reset, stdin is empty, and a timeout kills everything
//! the command started, pipelines included.

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

use std::time::Duration;

use monitorflow_common::proto::v1::{CommandAcceptance, ShellCommand};
use prost::Message;
use tokio::io::{AsyncRead, AsyncReadExt};

use super::{Handler, Output, MAX_OUTPUT};

/// Timeout when the command does not set one.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
/// Longest timeout accepted; longer ones are cut to it.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// Why a command is refused while `commands.shell` is off, with the way
/// to turn it on on this platform.
#[cfg(unix)]
const DISABLED: &str = "shell commands are disabled on this agent (commands.shell); \
                        run the installer with MF_ALLOW_COMMANDS=1 to allow them";
#[cfg(windows)]
const DISABLED: &str = "shell commands are disabled on this agent (commands.shell); \
                        run `monitorflow-agent.exe ai-commands on` as an administrator \
                        and restart the MonitorFlowAgent service to allow them";

pub struct ShellHandler {
    enabled: bool,
}

impl ShellHandler {
    /// `enabled` is the agent's `commands.shell`.
    pub fn new(enabled: bool) -> Self {
        Self { enabled }
    }
}

#[async_trait::async_trait]
impl Handler for ShellHandler {
    fn acceptance(&self) -> CommandAcceptance {
        if self.enabled {
            CommandAcceptance::Accepted
        } else {
            CommandAcceptance::Disabled
        }
    }

    async fn run(&self, payload: &[u8]) -> Output {
        if !self.enabled {
            return Output::rejected(DISABLED);
        }
        let Ok(request) = ShellCommand::decode(payload) else {
            return Output::rejected("invalid ShellCommand payload");
        };
        let line = request.command.trim();
        if line.is_empty() || line.contains(['\n', '\r', '\0']) {
            return Output::rejected("the command must be one non-empty line");
        }
        let timeout = match request.timeout_seconds {
            0 => DEFAULT_TIMEOUT,
            s => Duration::from_secs(s.into()).min(MAX_TIMEOUT),
        };
        tracing::info!(timeout_s = timeout.as_secs(), "running a shell command");
        #[cfg(unix)]
        return unix::run(line, timeout).await;
        #[cfg(windows)]
        return windows::run(line, timeout).await;
    }
}

/// What a command that could not start reports.
fn not_started(program: &str, error: std::io::Error) -> Output {
    Output {
        exit_code: -1,
        error: Some(format!("cannot start {program}: {error}")),
        ..Output::default()
    }
}

/// What a command killed at its timeout reports.
fn timed_out(timeout: Duration) -> Output {
    Output {
        exit_code: -1,
        error: Some(format!("timed out after {} s", timeout.as_secs())),
        ..Output::default()
    }
}

/// Reads `reader` to the end, keeping the last [`MAX_OUTPUT`] bytes, so a
/// noisy command cannot exhaust memory.
async fn tail(mut reader: impl AsyncRead + Unpin) -> std::io::Result<Vec<u8>> {
    let mut kept = Vec::new();
    let mut chunk = vec![0; 16 * 1024];
    loop {
        let n = reader.read(&mut chunk).await?;
        if n == 0 {
            return Ok(kept);
        }
        kept.extend_from_slice(&chunk[..n]);
        if kept.len() > 2 * MAX_OUTPUT {
            kept.drain(..kept.len() - MAX_OUTPUT);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn payload(command: &str, timeout_seconds: u32) -> Vec<u8> {
        ShellCommand {
            command: command.into(),
            timeout_seconds,
        }
        .encode_to_vec()
    }

    /// A command that succeeds on every platform.
    const TRUE: &str = "whoami";

    /// The report is true of what the handler then does (ADR-0076).
    #[tokio::test]
    async fn reports_what_it_does() {
        let (on, off) = (ShellHandler::new(true), ShellHandler::new(false));
        assert_eq!(on.acceptance(), CommandAcceptance::Accepted);
        let output = on.run(&payload(TRUE, 0)).await;
        assert!(!output.rejected && output.exit_code == 0, "{output:?}");
        assert_eq!(off.acceptance(), CommandAcceptance::Disabled);
        assert!(off.run(&payload(TRUE, 0)).await.rejected);
    }

    #[tokio::test]
    async fn refuses_when_disabled_or_malformed() {
        let output = ShellHandler::new(false).run(&payload(TRUE, 0)).await;
        assert!(output.rejected);
        assert!(output.error.unwrap().contains("commands.shell"));
        let handler = ShellHandler::new(true);
        assert!(handler.run(b"\xff\xff").await.rejected);
        assert!(handler.run(&payload("  ", 0)).await.rejected);
        assert!(handler.run(&payload("whoami\nreboot", 0)).await.rejected);
        assert!(handler.run(&payload("whoami\rreboot", 0)).await.rejected);
    }
}
