//! Package operations (ADR-0031): one [`PackageManager`] per OS family,
//! the `apply_updates` command and the periodic [`PackageReport`].
//!
//! Privileged steps run through `sudo -n` with fixed command lines (see
//! [`exec`]); everything else runs as the agent's user.

pub mod apply;
pub mod apt;
pub mod dnf;
pub mod exec;
pub mod sources;

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use monitorflow_common::command;
use monitorflow_common::proto::v1::{ApplyUpdates, Package, PackageReport};
use tokio::sync::watch;

use crate::os::OsFamily;
use crate::shutdown::Shutdown;
pub use apply::ApplyUpdatesHandler;
pub use exec::{Run, Tools};

/// Longest stderr line quoted in an error.
const STDERR_QUOTED: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PackageError {
    #[error("cannot run {0}: {1}")]
    Spawn(String, String),
    #[error("`{0}` did not finish in time")]
    Timeout(String),
    #[error("`{line}` exited with {code}: {stderr}")]
    Failed {
        line: String,
        code: i32,
        stderr: String,
    },
}

impl PackageError {
    pub fn failed(run: &Run) -> Self {
        let stderr = String::from_utf8_lossy(&run.stderr);
        let first = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        Self::Failed {
            line: run.line.clone(),
            code: run.code,
            stderr: first.chars().take(STDERR_QUOTED).collect(),
        }
    }
}

/// The host's package manager.
#[async_trait::async_trait]
pub trait PackageManager: Send + Sync + 'static {
    /// Refreshes repository metadata, as root; `None` when the upgrade
    /// does it itself.
    async fn refresh(&self) -> Result<Option<Run>, PackageError>;
    /// Upgrades what `request` asks, as root; `None` when nothing matched.
    /// Package names are already checked with [`valid_name`].
    async fn upgrade(&self, request: &ApplyUpdates) -> Result<Option<Run>, PackageError>;
    /// Installed packages with their upgradable version, from cached
    /// metadata.
    async fn inventory(&self) -> Result<Vec<Package>, PackageError>;
    async fn reboot_required(&self) -> Result<bool, PackageError>;
}

/// apt on the Debian family, dnf on the RHEL family.
pub fn for_family(family: OsFamily, tools: Tools) -> Arc<dyn PackageManager> {
    match family {
        OsFamily::Debian => Arc::new(apt::Apt::new(tools)),
        OsFamily::Rhel => Arc::new(dnf::Dnf::new(tools)),
    }
}

/// Package names from the server reach a command line; see
/// [`command::valid_package_name`].
pub use command::valid_package_name as valid_name;

/// Collects package reports and publishes the latest one to the stream,
/// which sends it on the current connection, or on the next one when
/// disconnected.
#[derive(Clone)]
pub struct Reports {
    manager: Arc<dyn PackageManager>,
    latest: watch::Sender<Option<PackageReport>>,
}

impl Reports {
    /// Reports from `manager`, and the receiver for the stream.
    pub fn new(manager: Arc<dyn PackageManager>) -> (Self, watch::Receiver<Option<PackageReport>>) {
        let (latest, rx) = watch::channel(None);
        (Self { manager, latest }, rx)
    }

    /// Collects a report and publishes it. A failure is logged and
    /// publishes nothing: an empty inventory would look like a wiped host.
    pub async fn collect(&self) {
        let packages = match self.manager.inventory().await {
            Ok(packages) => packages,
            Err(error) => {
                tracing::warn!(%error, "cannot list installed packages; no package report");
                return;
            }
        };
        let reboot_required = self
            .manager
            .reboot_required()
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "cannot tell whether a reboot is required; reporting none");
                false
            });
        let upgradable = packages
            .iter()
            .filter(|p| !p.upgradable_version.is_empty())
            .count();
        tracing::info!(
            packages = packages.len(),
            upgradable,
            reboot_required,
            "package report collected"
        );
        self.latest.send_replace(Some(PackageReport {
            packages,
            reboot_required,
            collected_at_unix_ms: unix_ms(),
        }));
    }

    /// Collects at once, then every `interval`, until `shutdown`.
    pub async fn run(self, interval: Duration, mut shutdown: Shutdown) {
        loop {
            self.collect().await;
            tokio::select! {
                () = tokio::time::sleep(interval) => {}
                () = shutdown.wait() => return,
            }
        }
    }
}

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

#[cfg(test)]
mod tests;
