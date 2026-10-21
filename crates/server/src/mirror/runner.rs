//! Running the mirror tools (`aptly`, `reposync`, `createrepo_c`, `gpg`)
//! behind [`Runner`], so tests substitute scripts for them (ADR-0030).
//!
//! Commands never go through a shell: arguments are passed as they are.
//! The environment is cleared except for `PATH`, `HOME`, the proxy
//! variables and what the caller sets, so nothing of the server's own
//! environment (database URL and other secrets) reaches the tools.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use monitorflow_common::config::MirrorConfig;
use tokio::io::{AsyncRead, AsyncReadExt};

/// Bytes kept of a command's stdout and of its stderr: the end, where the
/// errors are.
pub const OUTPUT_TAIL: usize = 64 * 1024;

/// Variables passed through from the server's environment when set.
const PASSED_ENV: [&str; 8] = [
    "PATH",
    "HOME",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Aptly,
    Reposync,
    CreaterepoC,
    Gpg,
}

impl Tool {
    pub fn name(self) -> &'static str {
        match self {
            Self::Aptly => "aptly",
            Self::Reposync => "reposync",
            Self::CreaterepoC => "createrepo_c",
            Self::Gpg => "gpg",
        }
    }
}

/// One command to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub tool: Tool,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
}

impl Invocation {
    pub fn new<I, S>(tool: Tool, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        Self {
            tool,
            args: args.into_iter().map(Into::into).collect(),
            cwd: None,
        }
    }

    pub fn in_dir(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }
}

/// The command line as a run's log shows it.
impl fmt::Display for Invocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.tool.name())?;
        for arg in &self.args {
            let arg = arg.to_string_lossy();
            if arg.is_empty() || arg.contains(|c: char| c.is_whitespace() || c == '\'') {
                write!(f, " '{}'", arg.replace('\'', r"'\''"))?;
            } else {
                write!(f, " {arg}")?;
            }
        }
        Ok(())
    }
}

/// What a finished command printed. A non-zero exit is not an error here;
/// see [`Output::check`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub success: bool,
    /// `exit status: 1`, `signal: 9 (SIGKILL)`…
    pub status: String,
    /// The last [`OUTPUT_TAIL`] bytes, lossily decoded.
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    /// `Ok(self)` on success, else the error with the last line of stderr.
    pub fn check(self, invocation: &Invocation) -> Result<Self, RunError> {
        if self.success {
            return Ok(self);
        }
        let last = self
            .stderr
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .chars()
            .take(500)
            .collect();
        Err(RunError::Failed {
            tool: invocation.tool.name(),
            status: self.status.clone(),
            last_line: last,
            output: Box::new(self),
        })
    }

    /// stdout then stderr, as stored in a run's output.
    pub fn combined(&self) -> String {
        let mut text = self.stdout.clone();
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&self.stderr);
        text
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("cannot run {tool} ({program}): {error}")]
    Spawn {
        tool: &'static str,
        program: String,
        error: std::io::Error,
    },
    #[error("{tool} did not finish within {}s and was killed", timeout.as_secs())]
    Timeout {
        tool: &'static str,
        timeout: Duration,
    },
    #[error("{tool}: {error}")]
    Io {
        tool: &'static str,
        error: std::io::Error,
    },
    #[error("{tool} failed ({status}): {}", last_line.escape_debug())]
    Failed {
        tool: &'static str,
        status: String,
        last_line: String,
        output: Box<Output>,
    },
}

impl RunError {
    /// What the command printed before it failed, if it ran.
    pub fn output(&self) -> Option<&Output> {
        match self {
            Self::Failed { output, .. } => Some(output),
            _ => None,
        }
    }
}

/// Runs one tool command to completion.
#[async_trait::async_trait]
pub trait Runner: Send + Sync + 'static {
    async fn run(&self, invocation: &Invocation) -> Result<Output, RunError>;
}

/// Runs the real programs with `tokio::process`.
#[derive(Debug, Clone)]
pub struct CommandRunner {
    aptly: PathBuf,
    reposync: PathBuf,
    createrepo_c: PathBuf,
    gpg: PathBuf,
    timeout: Duration,
    gnupg_home: PathBuf,
}

impl CommandRunner {
    /// The programs of `config`, with `GNUPGHOME` set to `gnupg_home` for
    /// every command (aptly runs gpg to sign).
    pub fn new(config: &MirrorConfig, gnupg_home: impl Into<PathBuf>) -> Self {
        Self {
            aptly: config.aptly.clone(),
            reposync: config.reposync.clone(),
            createrepo_c: config.createrepo_c.clone(),
            gpg: config.gpg.clone(),
            timeout: Duration::from_secs(config.sync_timeout),
            gnupg_home: gnupg_home.into(),
        }
    }

    fn program(&self, tool: Tool) -> &Path {
        match tool {
            Tool::Aptly => &self.aptly,
            Tool::Reposync => &self.reposync,
            Tool::CreaterepoC => &self.createrepo_c,
            Tool::Gpg => &self.gpg,
        }
    }
}

#[async_trait::async_trait]
impl Runner for CommandRunner {
    async fn run(&self, invocation: &Invocation) -> Result<Output, RunError> {
        let tool = invocation.tool.name();
        let program = self.program(invocation.tool);
        let mut command = tokio::process::Command::new(program);
        command
            .args(&invocation.args)
            .env_clear()
            .envs(
                PASSED_ENV
                    .iter()
                    .filter_map(|k| Some((k, std::env::var_os(k)?))),
            )
            .env("LC_ALL", "C")
            .env("GNUPGHOME", &self.gnupg_home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Its own process group, so a timeout also kills the programs
            // it started (aptly runs gpg, reposync downloaders).
            .process_group(0)
            .kill_on_drop(true);
        if let Some(cwd) = &invocation.cwd {
            command.current_dir(cwd);
        }
        let mut child = command.spawn().map_err(|error| RunError::Spawn {
            tool,
            program: program.display().to_string(),
            error,
        })?;
        let pid = child.id();
        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");
        let finished = tokio::time::timeout(self.timeout, async {
            tokio::try_join!(tail(stdout), tail(stderr), child.wait())
        })
        .await;
        let (stdout, stderr, status) = match finished {
            Ok(result) => result.map_err(|error| RunError::Io { tool, error })?,
            Err(_) => {
                if let Some(pid) = pid.and_then(|p| rustix::process::Pid::from_raw(p as i32)) {
                    // The group may be gone already.
                    let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
                }
                let _ = child.kill().await;
                return Err(RunError::Timeout {
                    tool,
                    timeout: self.timeout,
                });
            }
        };
        Ok(Output {
            success: status.success(),
            status: status.to_string(),
            stdout,
            stderr,
        })
    }
}

/// Reads `reader` to the end, keeping the last [`OUTPUT_TAIL`] bytes.
async fn tail(mut reader: impl AsyncRead + Unpin) -> std::io::Result<String> {
    let mut kept = Vec::new();
    let mut dropped = 0usize;
    let mut chunk = vec![0; 16 * 1024];
    loop {
        let n = reader.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        kept.extend_from_slice(&chunk[..n]);
        if kept.len() > 2 * OUTPUT_TAIL {
            let cut = kept.len() - OUTPUT_TAIL;
            kept.drain(..cut);
            dropped += cut;
        }
    }
    if kept.len() > OUTPUT_TAIL {
        let cut = kept.len() - OUTPUT_TAIL;
        kept.drain(..cut);
        dropped += cut;
    }
    let text = String::from_utf8_lossy(&kept);
    Ok(if dropped > 0 {
        format!("[{dropped} bytes omitted]\n{text}")
    } else {
        text.into_owned()
    })
}

/// Keeps the last `max` bytes of `text` at a character boundary, with a
/// marker when something was cut: what a run's `output` column stores.
pub fn keep_tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("[{start} bytes omitted]\n{}", &text[start..])
}

#[cfg(test)]
mod tests;
