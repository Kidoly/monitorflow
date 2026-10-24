//! The remediation loop's [`ChangeHook`] over a [`Snapshotter`]
//! (ADR-0046): a snapshot before every change-tier command where the
//! server supports one, "rollback is limited" where it does not.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;

use super::{SnapshotError, SnapshotRequest, Snapshotter, Support, Unsupported};
use crate::remediation::runner::hook::{self, Change, ChangeHook};

/// Snapshots through `snapshotter` before each change.
///
/// A server without support (integration off, not mapped, cluster not
/// configured) gets `method: "none"` and the command runs, as the approver
/// was shown. A server with support whose snapshot fails gets an error, so
/// the incident escalates without running the command.
#[derive(Clone)]
pub struct SnapshotHook {
    snapshotter: Arc<dyn Snapshotter>,
    /// Recorded as the `snapshot` event's `method`, `proxmox` for instance.
    method: &'static str,
}

impl SnapshotHook {
    pub fn new(snapshotter: Arc<dyn Snapshotter>, method: &'static str) -> Self {
        Self {
            snapshotter,
            method,
        }
    }

    fn unavailable(reason: Unsupported) -> hook::Snapshot {
        hook::Snapshot {
            method: "none".into(),
            name: None,
            detail: format!("{reason}; rollback is limited"),
        }
    }
}

impl fmt::Debug for SnapshotHook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotHook")
            .field("method", &self.method)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl ChangeHook for SnapshotHook {
    async fn before_change(&self, change: &Change<'_>) -> Result<hook::Snapshot, String> {
        let server = change.server_id;
        let vm = match self.snapshotter.support(server).await {
            Ok(Support::Available(vm)) => vm,
            Ok(Support::Unavailable(reason)) => return Ok(Self::unavailable(reason)),
            Err(error) => return Err(error.to_string()),
        };
        let request = SnapshotRequest {
            incident: change.incident_id,
            step: change.step,
        };
        match self.snapshotter.take(server, request).await {
            Ok(snapshot) => Ok(hook::Snapshot {
                method: self.method.into(),
                detail: format!(
                    "{} snapshot {} of VM {} on {}/{}, before a {} command",
                    self.method,
                    snapshot.name,
                    vm.vmid,
                    vm.cluster,
                    vm.node,
                    change.tier.as_str()
                ),
                name: Some(snapshot.name),
            }),
            // The mapping went away since `support`.
            Err(SnapshotError::Unsupported(reason)) => Ok(Self::unavailable(reason)),
            Err(error) => Err(error.to_string()),
        }
    }
}
