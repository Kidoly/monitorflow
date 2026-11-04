//! The remediation loop's [`ChangeHook`] over a [`Snapshotter`]
//! (ADR-0046): a snapshot before every change-tier command where the
//! server supports one, "rollback is limited" where it does not. A server
//! without a Proxmox VM gets a local LVM or ZFS snapshot when its system
//! volumes allow one (ADR-0094).

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;

use super::choice::{self, Choice};
use super::local::{self, LocalPlan, LocalSnapshots, LocalUnavailable};
use super::{SnapshotError, SnapshotRequest, Snapshotter, Support};
use crate::remediation::runner::hook::{self, Change, ChangeHook};

/// Snapshots through `snapshotter` before each change.
///
/// A server without support (integration off, not mapped, cluster not
/// configured, a guest Proxmox cannot snapshot) gets `method: "none"` and the command runs, as the approver
/// was shown. A server with support whose snapshot fails gets an error, so
/// the incident escalates without running the command.
#[derive(Clone)]
pub struct SnapshotHook {
    snapshotter: Arc<dyn Snapshotter>,
    /// Recorded as the `snapshot` event's `method`, `proxmox` for instance.
    method: &'static str,
    /// Where `snapshotter` has no support for a server.
    local: Option<LocalSnapshots>,
}

impl SnapshotHook {
    pub fn new(snapshotter: Arc<dyn Snapshotter>, method: &'static str) -> Self {
        Self {
            snapshotter,
            method,
            local: None,
        }
    }

    /// Falls back to local snapshots on servers `snapshotter` does not
    /// support (ADR-0094).
    pub fn with_local(mut self, local: LocalSnapshots) -> Self {
        self.local = Some(local);
        self
    }

    fn unavailable(reason: impl fmt::Display) -> hook::Snapshot {
        hook::Snapshot {
            method: "none".into(),
            name: None,
            detail: format!("{reason}; rollback is limited"),
        }
    }

    /// Takes the local snapshot `plan` describes.
    async fn take_local(
        local: &LocalSnapshots,
        change: &Change<'_>,
        plan: LocalPlan,
    ) -> Result<hook::Snapshot, String> {
        let name = local::incident_name(change.incident_id, change.step);
        local.take(change.server_id, &name).await?;
        Ok(hook::Snapshot {
            method: plan.method.into(),
            detail: format!(
                "{}, before a {} command",
                plan.summary,
                change.tier.as_str()
            ),
            name: Some(name),
        })
    }
}

impl fmt::Debug for SnapshotHook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotHook")
            .field("method", &self.method)
            .field("local", &self.local.is_some())
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl ChangeHook for SnapshotHook {
    async fn before_change(&self, change: &Change<'_>) -> Result<hook::Snapshot, String> {
        let server = change.server_id;
        let proxmox = self
            .snapshotter
            .support(server)
            .await
            .map_err(|e| e.to_string())?;
        // Proxmox first; a local snapshot only where Proxmox has no say
        // (ADR-0094).
        let local = match (&self.local, &proxmox) {
            (Some(local), Support::Unavailable(reason)) if !choice::proxmox_decides(*reason) => {
                let plan = local::support(local.pool(), server)
                    .await
                    .map_err(|e| e.to_string())?;
                Some((local, plan))
            }
            _ => None,
        };
        let (local, plan) = match (local, &proxmox) {
            (Some((local, plan)), _) => (Some(local), plan),
            (None, Support::Unavailable(reason)) => return Ok(Self::unavailable(reason)),
            (None, Support::Available(_)) => (None, Err(LocalUnavailable::NotReported)),
        };
        let vm = match choice::choose(proxmox, || plan) {
            Choice::Proxmox(vm) => vm,
            Choice::Limited(why) => return Ok(Self::unavailable(why)),
            Choice::Local(plan) => match local {
                Some(local) => return Self::take_local(local, change, plan).await,
                None => return Ok(Self::unavailable("no local snapshots")),
            },
        };
        let request = SnapshotRequest {
            incident: change.incident_id,
            step: change.step,
        };
        match self.snapshotter.take(server, request).await {
            Ok(snapshot) => Ok(hook::Snapshot {
                method: self.method.into(),
                detail: format!(
                    "{} snapshot {} of {} {} on {}/{}, before a {} command",
                    self.method,
                    snapshot.name,
                    vm.kind.noun(),
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
