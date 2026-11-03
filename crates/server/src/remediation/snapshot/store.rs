//! Server to guest mappings and the snapshots MonitorFlow took, in
//! `server_proxmox_vms` and `proxmox_snapshots` (ADR-0044). A guest is a
//! VM or a container (ADR-0093).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgExecutor;
use uuid::Uuid;

/// Proxmox VE VMs IDs are 100 to 999,999,999.
pub const VMID_RANGE: std::ops::RangeInclusive<u32> = 100..=999_999_999;

/// What a Proxmox guest is, which picks its API path.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GuestKind {
    /// A VM: `/nodes/{node}/qemu/{vmid}`.
    #[default]
    Qemu,
    /// A container: `/nodes/{node}/lxc/{vmid}`.
    Lxc,
}

impl GuestKind {
    /// The API path segment, also stored in `server_proxmox_vms.kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Qemu => "qemu",
            Self::Lxc => "lxc",
        }
    }

    pub fn parse(kind: &str) -> Option<Self> {
        match kind {
            "qemu" => Some(Self::Qemu),
            "lxc" => Some(Self::Lxc),
            _ => None,
        }
    }

    /// "VM" or "container", in sentences.
    pub fn noun(self) -> &'static str {
        match self {
            Self::Qemu => "VM",
            Self::Lxc => "container",
        }
    }
}

/// A VM or container on a Proxmox cluster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmRef {
    /// A `[[proxmox.clusters]]` name.
    pub cluster: String,
    /// The node hosting the guest; the API addresses guests through it.
    pub node: String,
    pub vmid: u32,
    #[serde(default)]
    pub kind: GuestKind,
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

/// A server's guest, with what the last check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapped {
    pub vm: VmRef,
    /// Why Proxmox refuses snapshots of the guest, if it does.
    pub cannot_snapshot: Option<String>,
}

/// The guest of `server`, if mapped.
pub async fn mapping(db: impl PgExecutor<'_>, server: Uuid) -> Result<Option<Mapped>, sqlx::Error> {
    let row: Option<(String, String, i32, String, Option<String>)> = sqlx::query_as(
        "SELECT cluster, node, vmid, kind, cannot_snapshot
         FROM server_proxmox_vms WHERE server_id = $1",
    )
    .bind(server)
    .fetch_optional(db)
    .await?;
    Ok(
        row.map(|(cluster, node, vmid, kind, cannot_snapshot)| Mapped {
            vm: VmRef {
                cluster,
                node,
                vmid: vmid as u32,
                kind: GuestKind::parse(&kind).unwrap_or_default(),
            },
            cannot_snapshot,
        }),
    )
}

/// The customer and site of `server`, if it exists.
pub async fn owner_of(
    db: impl PgExecutor<'_>,
    server: Uuid,
) -> Result<Option<(Uuid, Uuid)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT si.customer_id, s.site_id FROM servers s JOIN sites si ON si.id = s.site_id
         WHERE s.id = $1",
    )
    .bind(server)
    .fetch_optional(db)
    .await
}

/// Moves `server`'s mapping of `vm` to `node`, where the guest now is.
pub async fn move_node(
    db: impl PgExecutor<'_>,
    server: Uuid,
    vm: &VmRef,
    node: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE server_proxmox_vms SET node = $4, updated_at = now()
         WHERE server_id = $1 AND cluster = $2 AND vmid = $3",
    )
    .bind(server)
    .bind(&vm.cluster)
    .bind(vm.vmid as i32)
    .bind(node)
    .execute(db)
    .await?;
    Ok(())
}

/// Mappings to check again: never checked, or not for `every`, on
/// `clusters`, at most `limit`, the oldest check first.
pub async fn due_checks(
    db: impl PgExecutor<'_>,
    clusters: &[String],
    every: std::time::Duration,
    limit: i64,
) -> Result<Vec<(Uuid, VmRef)>, sqlx::Error> {
    let rows: Vec<(Uuid, String, String, i32, String)> = sqlx::query_as(
        "SELECT server_id, cluster, node, vmid, kind FROM server_proxmox_vms
         WHERE cluster = ANY($1)
           AND (checked_at IS NULL OR checked_at < now() - make_interval(secs => $2))
         ORDER BY checked_at NULLS FIRST, server_id
         LIMIT $3",
    )
    .bind(clusters)
    .bind(every.as_secs_f64())
    .bind(limit)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(server, cluster, node, vmid, kind)| {
            let kind = GuestKind::parse(&kind).unwrap_or_default();
            (
                server,
                VmRef {
                    cluster,
                    node,
                    vmid: vmid as u32,
                    kind,
                },
            )
        })
        .collect())
}

/// What a check of `vm` found, for `server`. Ignored if the mapping
/// changed since.
pub async fn record_check(
    db: impl PgExecutor<'_>,
    server: Uuid,
    vm: &VmRef,
    cannot_snapshot: Option<&str>,
    left_out: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE server_proxmox_vms
         SET checked_at = now(), cannot_snapshot = $5, left_out = $6
         WHERE server_id = $1 AND cluster = $2 AND vmid = $3 AND kind = $4",
    )
    .bind(server)
    .bind(&vm.cluster)
    .bind(vm.vmid as i32)
    .bind(vm.kind.as_str())
    .bind(cannot_snapshot)
    .bind(left_out)
    .execute(db)
    .await?;
    Ok(())
}

/// Has the next pass check `server`'s guest again, after a failure.
pub async fn check_soon(db: impl PgExecutor<'_>, server: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE server_proxmox_vms SET checked_at = NULL WHERE server_id = $1")
        .bind(server)
        .execute(db)
        .await?;
    Ok(())
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
