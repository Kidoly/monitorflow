//! Server to VM mappings and the snapshots MonitorFlow took, in
//! `server_proxmox_vms` and `proxmox_snapshots` (ADR-0044).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgExecutor;
use uuid::Uuid;

/// Proxmox VE VMs IDs are 100 to 999,999,999.
pub const VMID_RANGE: std::ops::RangeInclusive<u32> = 100..=999_999_999;

/// A VM on a Proxmox cluster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmRef {
    /// A `[[proxmox.clusters]]` name.
    pub cluster: String,
    /// The node hosting the VM; the API addresses VMs through it.
    pub node: String,
    pub vmid: u32,
}

impl VmRef {
    /// Why the fields cannot name a VM, if they cannot. Same rules as the
    /// table's checks.
    pub fn check(&self) -> Result<(), &'static str> {
        if !monitorflow_common::config::valid_proxmox_cluster_name(&self.cluster) {
            return Err("cluster must be 1 to 64 letters, digits, `.`, `_` or `-`");
        }
        if !valid_node(&self.node) {
            return Err("node must be a host name of letters, digits and `-`");
        }
        if !VMID_RANGE.contains(&self.vmid) {
            return Err("vmid must be 100 to 999999999");
        }
        Ok(())
    }
}

/// A DNS label: 1 to 63 letters, digits and inner `-`.
fn valid_node(node: &str) -> bool {
    (1..=63).contains(&node.len())
        && node.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !node.starts_with('-')
        && !node.ends_with('-')
}

/// The VM of `server`, if mapped.
pub async fn mapping(db: impl PgExecutor<'_>, server: Uuid) -> Result<Option<VmRef>, sqlx::Error> {
    let row: Option<(String, String, i32)> =
        sqlx::query_as("SELECT cluster, node, vmid FROM server_proxmox_vms WHERE server_id = $1")
            .bind(server)
            .fetch_optional(db)
            .await?;
    Ok(row.map(|(cluster, node, vmid)| VmRef {
        cluster,
        node,
        vmid: vmid as u32,
    }))
}

/// When MonitorFlow took `name` of `vm`, if it did and retention has not
/// deleted it.
pub async fn recorded(
    db: impl PgExecutor<'_>,
    vm: &VmRef,
    name: &str,
) -> Result<Option<DateTime<Utc>>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT created_at FROM proxmox_snapshots
         WHERE cluster = $1 AND vmid = $2 AND name = $3 AND deleted_at IS NULL",
    )
    .bind(&vm.cluster)
    .bind(vm.vmid as i32)
    .bind(name)
    .fetch_optional(db)
    .await
}

/// Records that MonitorFlow took `name` of `vm` for `server`.
pub async fn record(
    db: impl PgExecutor<'_>,
    vm: &VmRef,
    server: Uuid,
    name: &str,
) -> Result<DateTime<Utc>, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO proxmox_snapshots (cluster, vmid, name, server_id)
         VALUES ($1, $2, $3, $4) RETURNING created_at",
    )
    .bind(&vm.cluster)
    .bind(vm.vmid as i32)
    .bind(name)
    .bind(server)
    .fetch_one(db)
    .await
}

/// Names of MonitorFlow's live snapshots of `vm`, oldest first.
pub async fn live(db: impl PgExecutor<'_>, vm: &VmRef) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT name FROM proxmox_snapshots
         WHERE cluster = $1 AND vmid = $2 AND deleted_at IS NULL
         ORDER BY created_at, id",
    )
    .bind(&vm.cluster)
    .bind(vm.vmid as i32)
    .fetch_all(db)
    .await
}

/// Marks `name` of `vm` deleted from Proxmox.
pub async fn mark_deleted(
    db: impl PgExecutor<'_>,
    vm: &VmRef,
    name: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE proxmox_snapshots SET deleted_at = now()
         WHERE cluster = $1 AND vmid = $2 AND name = $3 AND deleted_at IS NULL",
    )
    .bind(&vm.cluster)
    .bind(vm.vmid as i32)
    .bind(name)
    .execute(db)
    .await?;
    Ok(())
}

/// The oldest of `live` (oldest first) beyond the newest `keep`.
pub fn beyond_retention(live: &[String], keep: usize) -> &[String] {
    &live[..live.len().saturating_sub(keep)]
}
