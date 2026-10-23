//! The `shell` command of the AI remediation loop (ADR-0043): one command
//! line run with `/bin/sh -c` as the agent's user.
//!
//! The server sends only command lines its risk rules let through (or an
//! approver accepted), signed like every command. The agent adds what it
//! controls: the kind is refused unless `commands.shell` is enabled, the
//! environment is reset, stdin is empty, and a timeout kills the whole
//! process group, pipelines included.

use std::process::Stdio;
use std::time::Duration;

use monitorflow_common::proto::v1::ShellCommand;
use prost::Message;
use tokio::io::{AsyncRead, AsyncReadExt};

use super::{Handler, Output, MAX_OUTPUT};

/// Timeout when the command does not set one.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
/// Longest timeout accepted; longer ones are cut to it.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(60 * 60);
/// `PATH` of the command; nothing else is inherited but `LC_ALL=C`.
const PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

pub struct ShellHandler {
    enabled: bool,
    shell: std::path::PathBuf,
}

impl ShellHandler {
    /// `enabled` is the agent's `commands.shell`.
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            shell: "/bin/sh".into(),
        }
    }
}

#[async_trait::async_trait]
impl Handler for ShellHandler {
    async fn run(&self, payload: &[u8]) -> Output {
        if !self.enabled {
            return Output::rejected("shell commands are disabled on this agent (commands.shell)");
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
        run(&self.shell, line, timeout).await
    }
}

async fn run(shell: &std::path::Path, line: &str, timeout: Duration) -> Output {
    let mut command = tokio::process::Command::new(shell);
    command
        .arg("-c")
        .arg(line)
        .env_clear()
        .env("PATH", PATH)
        .env("LC_ALL", "C")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Its own process group, so a timeout also kills a pipeline's
        // other stages and anything they started.
        .process_group(0)
        .kill_on_drop(true);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return Output {
                exit_code: -1,
                error: Some(format!("cannot start {}: {error}", shell.display())),
                ..Output::default()
            }
        }
    };
    let pid = child.id();
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let finished = tokio::time::timeout(timeout, async {
        tokio::try_join!(tail(stdout), tail(stderr), child.wait())
    })
    .await;
    match finished {
        Ok(Ok((stdout, stderr, status))) => Output {
            // -1 when killed by a signal.
            exit_code: status.code().unwrap_or(-1),
            stdout,
            stderr,
            error: None,
            rejected: false,
        },
        Ok(Err(error)) => Output {
            exit_code: -1,
            error: Some(format!("reading the command's output: {error}")),
            ..Output::default()
        },
        Err(_) => {
            if let Some(pid) = pid.and_then(|p| rustix::process::Pid::from_raw(p as i32)) {
                // The group may be gone already.
                let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
            }
            let _ = child.kill().await;
            Output {
                exit_code: -1,
                error: Some(format!("timed out after {} s", timeout.as_secs())),
                ..Output::default()
            }
        }
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

    fn payload(command: &str, timeout_seconds: u32) -> Vec<u8> {
        ShellCommand {
            command: command.into(),
            timeout_seconds,
        }
        .encode_to_vec()
    }

    #[tokio::test]
    async fn runs_pipelines_with_a_clean_environment() {
        let handler = ShellHandler::new(true);
        let output = handler
            .run(&payload("echo $PATH $LC_ALL $HOME | tr a-z A-Z; exit 3", 0))
            .await;
        assert!(!output.rejected);
        assert_eq!(output.exit_code, 3);
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            format!("{} C\n", PATH.to_uppercase())
        );
        let output = handler.run(&payload("ls /nonexistent-dir", 0)).await;
        assert_ne!(output.exit_code, 0);
        assert!(!output.stderr.is_empty());
    }

    #[tokio::test]
    async fn refuses_when_disabled_or_malformed() {
        let output = ShellHandler::new(false).run(&payload("true", 0)).await;
        assert!(output.rejected);
        assert!(output.error.unwrap().contains("commands.shell"));
        let handler = ShellHandler::new(true);
        assert!(handler.run(b"\xff\xff").await.rejected);
        assert!(handler.run(&payload("  ", 0)).await.rejected);
        assert!(handler.run(&payload("true\nreboot", 0)).await.rejected);
    }

    #[tokio::test]
    async fn timeouts_kill_the_whole_pipeline() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("late");
        let line = format!("sleep 3 | cat; touch {}", marker.display());
        let started = std::time::Instant::now();
        let output = ShellHandler::new(true).run(&payload(&line, 1)).await;
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(output.exit_code, -1);
        assert_eq!(output.error.as_deref(), Some("timed out after 1 s"));
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(!marker.exists(), "the shell survived its timeout");
    }

    #[tokio::test]
    async fn keeps_the_end_of_long_output() {
        let output = ShellHandler::new(true)
            .run(&payload("head -c 300000 /dev/zero; echo end", 0))
            .await;
        assert_eq!(output.exit_code, 0);
        assert!(output.stdout.len() <= 2 * MAX_OUTPUT);
        assert!(output.stdout.ends_with(b"end\n"));
    }
}
