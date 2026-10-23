//! Snapshots before change-tier commands (architecture §8, ADR-0044).
//!
//! An arbitrary shell command cannot be undone reliably, so before the
//! remediation loop runs a low- or high-impact command it asks a
//! [`Snapshotter`] for a snapshot of the server. Proxmox VE VM snapshots
//! come first ([`ProxmoxSnapshotter`]); LVM, Btrfs and ZFS may follow
//! behind the same trait. A server without snapshot support gets
//! [`Support::Unavailable`], and the interface shows that rollback is
//! limited.
//!
//! Snapshot names are deterministic, [`SnapshotName::for_step`], so a retry
//! of the same step finds the snapshot it already took. Only snapshots
//! MonitorFlow recorded as its own are ever rolled back to or deleted by
//! retention.

pub mod proxmox;
pub mod store;
#[cfg(test)]
mod tests;

use std::fmt;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

pub use proxmox::{ProxmoxClient, ProxmoxOptions, ProxmoxSnapshotter};
pub use store::VmRef;

/// Longest error text kept from a Proxmox answer or task.
const MAX_MESSAGE: usize = 512;

/// Why a server cannot be snapshotted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Unsupported {
    /// `[proxmox]` is off.
    Disabled,
    /// The server has no Proxmox VM mapping.
    NotMapped,
    /// The mapping names a cluster missing from the configuration.
    UnknownCluster,
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Disabled => "snapshots are not enabled",
            Self::NotMapped => "the server is not mapped to a Proxmox VM",
            Self::UnknownCluster => "the server's Proxmox cluster is not configured",
        })
    }
}

/// Whether a server can be snapshotted before a change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Support {
    Available(VmRef),
    Unavailable(Unsupported),
}

impl Support {
    /// From a server's mapping and the configured clusters: available only
    /// when the integration is on, the server mapped and its cluster
    /// configured.
    pub fn of(enabled: bool, clusters: &[String], mapping: Option<VmRef>) -> Self {
        match mapping {
            _ if !enabled => Self::Unavailable(Unsupported::Disabled),
            None => Self::Unavailable(Unsupported::NotMapped),
            Some(vm) if !clusters.contains(&vm.cluster) => {
                Self::Unavailable(Unsupported::UnknownCluster)
            }
            Some(vm) => Self::Available(vm),
        }
    }

    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available(_))
    }

    pub fn reason(&self) -> Option<Unsupported> {
        match self {
            Self::Available(_) => None,
            Self::Unavailable(reason) => Some(*reason),
        }
    }
}

/// Name of a snapshot MonitorFlow takes: `mf-<12 hex>-<step>`, where the
/// hex digits start the incident's UUID. At most 25 characters, within
/// Proxmox's 40 and its `[A-Za-z][A-Za-z0-9_-]*` grammar.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct SnapshotName(String);

impl SnapshotName {
    /// The snapshot before step `step` of remediation incident `incident`.
    pub fn for_step(incident: Uuid, step: u32) -> Self {
        let hex = incident.simple().to_string();
        Self(format!("mf-{}-{step}", &hex[..12]))
    }

    /// `name` if it has the shape [`for_step`](Self::for_step) produces.
    pub fn parse(name: &str) -> Option<Self> {
        let rest = name.strip_prefix("mf-")?;
        let (hex, step) = rest.split_once('-')?;
        let hex_ok = hex.len() == 12 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        let step_ok = step.parse::<u32>().is_ok() && step.bytes().all(|b| b.is_ascii_digit());
        (hex_ok && step_ok).then(|| Self(name.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SnapshotName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a snapshot is taken for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotRequest {
    /// The remediation incident.
    pub incident: Uuid,
    /// The step of the incident about to run a change.
    pub step: u32,
}

impl SnapshotRequest {
    pub fn name(&self) -> SnapshotName {
        SnapshotName::for_step(self.incident, self.step)
    }

    /// Stored with the snapshot on the platform, for its operators.
    pub fn description(&self) -> String {
        format!(
            "MonitorFlow, before step {} of remediation incident {}",
            self.step, self.incident
        )
    }
}

/// A snapshot of a server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Snapshot {
    pub name: String,
    pub description: String,
    pub created_at: Option<DateTime<Utc>>,
    /// MonitorFlow took it, so it may roll back to it and retention may
    /// delete it.
    pub managed: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("{0}")]
    Unsupported(Unsupported),
    /// The API refused the token: 401, or 403 for missing privileges.
    #[error("Proxmox refused the API token (HTTP {status})")]
    Auth { status: u16 },
    #[error("Proxmox API error (HTTP {status}): {message}")]
    Api { status: u16, message: String },
    #[error("Proxmox task failed: {0}")]
    TaskFailed(String),
    /// The task still ran when the timeout expired; it may finish later.
    #[error("Proxmox task {upid} did not finish in time")]
    Timeout { upid: String },
    #[error("cannot reach Proxmox: {0}")]
    Transport(String),
    #[error("unexpected Proxmox answer: {0}")]
    InvalidResponse(String),
    /// Rollback to a snapshot MonitorFlow did not take, or no longer has.
    #[error("no snapshot {0} taken by MonitorFlow")]
    UnknownSnapshot(String),
    /// A snapshot with the name exists on the VM but is not ours.
    #[error("snapshot {0} exists but was not taken by MonitorFlow")]
    NameTaken(String),
    #[error("Proxmox configuration: {0}")]
    Config(String),
    #[error("database: {0}")]
    Database(#[from] sqlx::Error),
}

/// Takes snapshots of servers before changes, and rolls back to them.
#[async_trait]
pub trait Snapshotter: Send + Sync {
    /// Whether `server` can be snapshotted. Without it, rollback is
    /// limited to what the command itself backs up.
    async fn support(&self, server: Uuid) -> Result<Support, SnapshotError>;

    /// Takes [`SnapshotRequest::name`] of `server` and waits for it, then
    /// applies retention. Returns the snapshot already taken under that
    /// name, if any, without taking another.
    async fn take(&self, server: Uuid, request: SnapshotRequest)
        -> Result<Snapshot, SnapshotError>;

    /// Snapshots of the server, MonitorFlow's and others, oldest first.
    async fn list(&self, server: Uuid) -> Result<Vec<Snapshot>, SnapshotError>;

    /// Rolls the server back to `name`, which MonitorFlow must have taken.
    async fn rollback(&self, server: Uuid, name: &SnapshotName) -> Result<(), SnapshotError>;
}

/// Used when no platform is configured: every server is unsupported.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoSnapshots;

#[async_trait]
impl Snapshotter for NoSnapshots {
    async fn support(&self, _server: Uuid) -> Result<Support, SnapshotError> {
        Ok(Support::Unavailable(Unsupported::Disabled))
    }

    async fn take(&self, _: Uuid, _: SnapshotRequest) -> Result<Snapshot, SnapshotError> {
        Err(SnapshotError::Unsupported(Unsupported::Disabled))
    }

    async fn list(&self, _server: Uuid) -> Result<Vec<Snapshot>, SnapshotError> {
        Err(SnapshotError::Unsupported(Unsupported::Disabled))
    }

    async fn rollback(&self, _: Uuid, _: &SnapshotName) -> Result<(), SnapshotError> {
        Err(SnapshotError::Unsupported(Unsupported::Disabled))
    }
}

/// `text` cut to [`MAX_MESSAGE`] bytes on a character boundary, control
/// characters replaced, for error messages from the remote side.
fn bounded(text: &str) -> String {
    let mut end = text.len().min(MAX_MESSAGE);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end]
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_owned()
}
