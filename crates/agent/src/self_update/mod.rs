//! Agent self-update (ADR-0053).
//!
//! On Linux the `self_update` command goes through three processes:
//!
//! 1. [`handler`], in the agent as its own user, downloads the release
//!    from the command's URLs, checks it, and writes a [`Header`] line and
//!    the binary to `sudo -n /usr/bin/monitorflow-agent self-update`.
//! 2. [`helper::install`], as root, checks everything again, swaps the
//!    binary atomically, keeps the previous one in [`STATE_DIR`], records a
//!    [`Status`] and starts the watchdog in a transient systemd unit.
//! 3. [`helper::watch`], as root, from the previous binary and outside the
//!    agent's cgroup, restarts the agent, waits for the new one to write its
//!    [`Connected`] file, and puts the previous binary back if it does not.
//!
//! The old agent is stopped while the command runs, so its journal entry
//! stays unfinished. When the server sends the command again, the
//! dispatcher asks [`StatusResumer`], which answers from the status file.
//!
//! The relay updates itself the same way (ADR-0055): the steps take a
//! [`Product`], [`AGENT`] or [`RELAY`], for the binary, unit and files.

pub mod handler;
#[cfg(unix)]
pub mod helper;
pub mod swap;

use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use monitorflow_common::client::identity::write_atomic;
use monitorflow_common::release::Platform;
use serde::{Deserialize, Serialize};

use crate::commands::{Output, Resumer};

/// Root's directory: the previous binary, the status and the lock.
pub const STATE_DIR: &str = "/var/lib/monitorflow/self-update";
pub const STATUS_FILE: &str = "status.json";
pub const PREVIOUS_FILE: &str = "previous";
/// Under the agent's `data_dir`.
pub const AGENT_DIR: &str = "self-update";
pub const CONNECTED_FILE: &str = "connected";

/// What updates itself: its binary, service, watchdog unit and root's
/// state directory, and its file in a release.
#[derive(Debug, Clone, Copy)]
pub struct Product {
    /// `agent` or `relay`, in messages.
    pub name: &'static str,
    /// The installed binary, as the sudo rule names it.
    pub bin: &'static str,
    pub service: &'static str,
    /// The transient unit running the watchdog.
    pub watch_unit: &'static str,
    /// Where the watchdog's log is.
    pub watch_log: &'static str,
    pub state_dir: &'static str,
    asset: fn(Platform) -> Option<&'static str>,
}

impl Product {
    /// Its file in a release for `platform`, if released there.
    pub fn asset(&self, platform: Platform) -> Option<&'static str> {
        (self.asset)(platform)
    }
}

pub const AGENT: Product = Product {
    name: "agent",
    bin: "/usr/bin/monitorflow-agent",
    service: "monitorflow-agent",
    watch_unit: "monitorflow-agent-self-update",
    watch_log: "journalctl -u monitorflow-agent-self-update",
    state_dir: STATE_DIR,
    asset: |platform| Some(platform.agent_asset()),
};

pub const RELAY: Product = Product {
    name: "relay",
    bin: "/usr/bin/monitorflow-relay",
    service: "monitorflow-relay",
    watch_unit: "monitorflow-relay-self-update",
    watch_log: "journalctl -u monitorflow-relay-self-update",
    state_dir: "/var/lib/monitorflow/relay-self-update",
    asset: Platform::relay_asset,
};

/// Where the privileged side keeps the status: [`STATE_DIR`] on Linux,
/// the update service's own directory on Windows.
pub fn state_dir() -> PathBuf {
    #[cfg(unix)]
    return PathBuf::from(STATE_DIR);
    #[cfg(windows)]
    return monitorflow_common::config::windows_base_dir()
        .join("update")
        .join("self-update");
}

pub const DEFAULT_HEALTH_TIMEOUT_S: u32 = 300;
pub const MIN_HEALTH_TIMEOUT_S: u32 = 30;
pub const MAX_HEALTH_TIMEOUT_S: u32 = 3600;
/// Added to the health timeout for the restart itself, before a status
/// still `watching` counts as abandoned (a reboot during the watch).
pub const RESTART_ALLOWANCE: Duration = Duration::from_secs(120);

/// Longest status or connected file read.
const MAX_STATE_FILE: u64 = 64 * 1024;

/// The health timeout of a command's `health_timeout_seconds`: 0 for the
/// default, else within the bounds.
pub fn health_timeout(seconds: u32) -> Result<Duration, String> {
    match seconds {
        0 => Ok(Duration::from_secs(DEFAULT_HEALTH_TIMEOUT_S.into())),
        s if (MIN_HEALTH_TIMEOUT_S..=MAX_HEALTH_TIMEOUT_S).contains(&s) => {
            Ok(Duration::from_secs(s.into()))
        }
        _ => Err(format!(
            "health_timeout_seconds must be 0 or {MIN_HEALTH_TIMEOUT_S} to {MAX_HEALTH_TIMEOUT_S}"
        )),
    }
}

/// The first line the helper reads on stdin; the binary follows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    pub command_id: String,
    /// `SHA256SUMS`, as text.
    pub sums: String,
    /// `SHA256SUMS.sig`, base64.
    pub signature: String,
    /// `VERSION`, as text, newline included.
    pub version_file: String,
    pub allow_downgrade: bool,
    pub health_timeout_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Installed; the watchdog restarts the agent and waits for it.
    Watching,
    Succeeded,
    RolledBack,
    /// Nothing changed in the end, like a watchdog that could not start.
    Failed,
}

/// The last update, in [`STATE_DIR`]: written by root, read by the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub command_id: String,
    pub from: String,
    pub to: String,
    pub phase: Phase,
    /// The agent's data directory, where the new agent writes
    /// [`Connected`].
    pub data_dir: PathBuf,
    pub health_timeout_ms: u64,
    /// When the helper started the watchdog.
    pub installed_at_unix_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Status {
    /// Past this, a status still `watching` lost its watchdog.
    pub fn deadline_unix_ms(&self) -> i64 {
        let allowance = RESTART_ALLOWANCE.as_millis() as i64;
        self.installed_at_unix_ms
            .saturating_add(self.health_timeout_ms as i64)
            .saturating_add(allowance)
    }

    pub fn read(path: &Path) -> io::Result<Option<Self>> {
        match read_small(path)? {
            None => Ok(None),
            Some(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(io::Error::other),
        }
    }

    /// Readable by the agent, which answers its command from it.
    pub fn write(&self, path: &Path) -> io::Result<()> {
        let bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        write_atomic(path, &bytes, 0o644).map_err(io::Error::other)
    }

    /// The command's result as this agent, running `running`, reports it
    /// at `now`; `None` while the watchdog may still decide.
    pub fn outcome(&self, running: &str, now_unix_ms: i64) -> Option<Output> {
        let failed = |error: String| Output {
            exit_code: 1,
            error: Some(error),
            ..Output::default()
        };
        let reason = self.reason.as_deref().unwrap_or("no reason recorded");
        match self.phase {
            Phase::Succeeded => Some(Output {
                stdout: format!("updated from {} to {}\n", self.from, self.to).into_bytes(),
                ..Output::default()
            }),
            Phase::RolledBack => Some(failed(format!("rolled back to {}: {reason}", self.from))),
            Phase::Failed => Some(failed(reason.to_owned())),
            Phase::Watching if now_unix_ms <= self.deadline_unix_ms() => None,
            Phase::Watching if running == self.to => Some(Output {
                stdout: format!(
                    "updated from {} to {}; the watchdog did not finish (was the host restarted?), so no rollback is possible\n",
                    self.from, self.to
                )
                .into_bytes(),
                ..Output::default()
            }),
            Phase::Watching => Some(failed(format!(
                "the update to {} did not finish; running {running}",
                self.to
            ))),
        }
    }
}

/// Written by an agent each time the upstream accepts its stream: the
/// watchdog's proof that a new version works.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connected {
    pub version: String,
    pub pid: u32,
    pub at_unix_ms: i64,
}

impl Connected {
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join(AGENT_DIR).join(CONNECTED_FILE)
    }

    /// Records that this process, of this version, is connected now.
    pub fn record(data_dir: &Path) -> io::Result<()> {
        let connected = Self {
            version: monitorflow_common::VERSION.to_owned(),
            pid: std::process::id(),
            at_unix_ms: unix_ms(),
        };
        let bytes = serde_json::to_vec(&connected).map_err(io::Error::other)?;
        write_atomic(&Self::path(data_dir), &bytes, 0o600).map_err(io::Error::other)
    }

    /// Read by root from a directory the agent's user owns: never through
    /// a symbolic link, and small.
    pub fn read(data_dir: &Path) -> io::Result<Option<Self>> {
        match read_small(&Self::path(data_dir))? {
            None => Ok(None),
            Some(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
        }
    }
}

/// The file at `path`, `None` when missing; refuses links and large files.
fn read_small(path: &Path) -> io::Result<Option<Vec<u8>>> {
    use std::io::Read;

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut bytes = Vec::new();
    file.take(MAX_STATE_FILE + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_STATE_FILE {
        return Err(io::Error::other(format!("{} is too large", path.display())));
    }
    Ok(Some(bytes))
}

/// Answers a `self_update` the restart interrupted, from the status file.
#[derive(Debug, Clone)]
pub struct StatusResumer {
    status: PathBuf,
    poll: Duration,
}

impl StatusResumer {
    pub fn new(state_dir: &Path) -> Self {
        Self {
            status: state_dir.join(STATUS_FILE),
            poll: Duration::from_secs(2),
        }
    }

    pub fn with_poll(mut self, poll: Duration) -> Self {
        self.poll = poll;
        self
    }
}

#[async_trait::async_trait]
impl Resumer for StatusResumer {
    async fn resume(
        &self,
        command_id: &str,
    ) -> Option<Pin<Box<dyn Future<Output = Output> + Send>>> {
        let path = self.status.clone();
        let status = tokio::task::spawn_blocking(move || Status::read(&path))
            .await
            .ok()?
            .ok()??;
        if status.command_id != command_id {
            return None;
        }
        let (path, poll) = (self.status.clone(), self.poll);
        Some(Box::pin(async move {
            let mut status = status;
            loop {
                if let Some(output) = status.outcome(monitorflow_common::VERSION, unix_ms()) {
                    return output;
                }
                tokio::time::sleep(poll).await;
                let read = path.clone();
                match tokio::task::spawn_blocking(move || Status::read(&read)).await {
                    Ok(Ok(Some(newer))) if newer.command_id == status.command_id => status = newer,
                    Ok(Ok(_)) => {
                        return Output {
                            exit_code: 1,
                            error: Some("the update status disappeared".into()),
                            ..Output::default()
                        }
                    }
                    Ok(Err(error)) => tracing::warn!(%error, "cannot read the update status"),
                    Err(error) => tracing::warn!(%error, "cannot read the update status"),
                }
            }
        }))
    }
}

pub(crate) fn unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

#[cfg(test)]
mod tests;
