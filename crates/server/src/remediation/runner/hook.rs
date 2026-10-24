//! The seam for rollback (architecture §8): what runs before a change-tier
//! command is queued. [`NoSnapshot`] only records that rollback is
//! limited; `snapshot::SnapshotHook` takes Proxmox snapshots (ADR-0046).

use std::fmt;

use async_trait::async_trait;
use uuid::Uuid;

use crate::remediation::risk::Tier;

/// The command about to run.
#[derive(Debug, Clone, Copy)]
pub struct Change<'a> {
    pub incident_id: Uuid,
    pub server_id: Uuid,
    /// The command's number in the incident, from 1: the commands queued
    /// before it plus one, so a resumed incident asks for the same step.
    pub step: u32,
    /// The exact command line, after classification.
    pub command: &'a str,
    /// `LowImpact` or `HighImpact`; forbidden commands never get here.
    pub tier: Tier,
}

/// What the hook did, recorded as the incident's `snapshot` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// `proxmox`, `lvm`, ..., or `none` when the host has no support.
    pub method: String,
    /// The snapshot's name, if one was taken.
    pub name: Option<String>,
    pub detail: String,
}

/// Called once per change-tier command, before it is queued. An error
/// stops the incident, escalated, without running the command. The call is
/// bounded by the incident's deadline.
#[async_trait]
pub trait ChangeHook: Send + Sync + fmt::Debug {
    async fn before_change(&self, change: &Change<'_>) -> Result<Snapshot, String>;
}

/// Takes no snapshot.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoSnapshot;

#[async_trait]
impl ChangeHook for NoSnapshot {
    async fn before_change(&self, _change: &Change<'_>) -> Result<Snapshot, String> {
        Ok(Snapshot {
            method: "none".into(),
            name: None,
            detail: "no snapshot support; rollback is limited".into(),
        })
    }
}
