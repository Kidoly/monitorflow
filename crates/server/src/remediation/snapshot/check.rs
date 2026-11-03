//! What Proxmox says about snapshotting a mapped guest, in words
//! (ADR-0093).
//!
//! Proxmox itself decides: `GET …/{qemu|lxc}/{vmid}/feature?feature=snapshot`
//! answers whether every volume of the guest can take a snapshot. When it
//! cannot, [`explain`] reads the guest's configuration to name the cause:
//! a bind or device mount point, or a volume on storage without snapshots
//! (`dir` for containers, raw files, plain LVM). A refusal is recorded on
//! the mapping, so the interface says "Rollback limited" before a change
//! rather than after a failed snapshot. [`run`] keeps the answers fresh,
//! after each discovery pass ([`super::discovery`]).

use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use serde_json::{Map, Value};
use tokio::time::sleep;

use super::proxmox::ProxmoxSnapshotter;
use super::store::GuestKind;

/// How often each mapping is checked again.
pub const EVERY: Duration = Duration::from_secs(3600);
/// Between two passes; a new mapping, or one whose snapshot failed, is
/// checked at the next.
pub const POLL: Duration = Duration::from_secs(60);

/// Storage types that snapshot every volume they hold.
const SNAPSHOT_STORAGE: [&str; 5] = ["zfspool", "zfs", "lvmthin", "rbd", "btrfs"];
/// File storage, where a VM disk takes snapshots as qcow2 only.
const FILE_STORAGE: [&str; 5] = ["dir", "nfs", "cifs", "glusterfs", "cephfs"];

/// The findings of one check.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Check {
    /// Why Proxmox refuses snapshots of the guest.
    pub cannot_snapshot: Option<String>,
    /// What a snapshot does not hold, though Proxmox takes one.
    pub left_out: Option<String>,
}

/// One volume or mount point of a guest's configuration.
#[derive(Debug, PartialEq, Eq)]
struct Volume<'a> {
    /// `rootfs`, `mp0`, `scsi0`…
    key: &'a str,
    /// `storage:name`, or a path for bind and device mounts.
    volume: &'a str,
    /// Where a container mounts it.
    path: Option<&'a str>,
}

impl Volume<'_> {
    fn storage(&self) -> Option<&str> {
        self.volume
            .split_once(':')
            .map(|(storage, _)| storage)
            .filter(|_| !self.volume.starts_with('/'))
    }

    /// "a bind mount" or "a device mount", for mount points that are
    /// host paths rather than volumes.
    fn host_path(&self) -> Option<&'static str> {
        if self.volume.starts_with("/dev/") {
            Some("a device mount")
        } else if self.volume.starts_with('/') {
            Some("a bind mount")
        } else {
            None
        }
    }

    fn at(&self) -> String {
        match self.path {
            Some(path) => format!("{} at {path}", self.key),
            None => self.key.to_owned(),
        }
    }
}

/// The volumes and mount points of a guest, in configuration order:
/// `rootfs` and `mpN` for containers, disks for VMs (CD-ROMs and unused
/// volumes left aside, as Proxmox does).
fn volumes(kind: GuestKind, config: &Map<String, Value>) -> Vec<Volume<'_>> {
    let mut found: Vec<Volume<'_>> = config
        .iter()
        .filter(|(key, _)| match kind {
            GuestKind::Lxc => *key == "rootfs" || numbered(key, "mp"),
            GuestKind::Qemu => {
                ["ide", "sata", "scsi", "virtio"]
                    .iter()
                    .any(|bus| numbered(key, bus))
                    || *key == "efidisk0"
                    || *key == "tpmstate0"
            }
        })
        .filter_map(|(key, value)| {
            let value = value.as_str()?;
            let mut volume = None;
            let mut path = None;
            for (i, part) in value.split(',').enumerate() {
                match part.split_once('=') {
                    Some(("volume" | "file", v)) => volume = Some(v),
                    Some(("mp", p)) => path = Some(p),
                    Some(("media", "cdrom")) => return None,
                    Some(_) => {}
                    None if i == 0 => volume = Some(part),
                    None => {}
                }
            }
            Some(Volume {
                key,
                volume: volume.filter(|v| !v.is_empty() && *v != "none")?,
                path,
            })
        })
        .collect();
    // `rootfs` first, then by key; a map's order is not the file's.
    found.sort_by_key(|v| (v.key != "rootfs", v.key.len(), v.key));
    found
}

/// `key` is `prefix` and a number, like `mp0`.
fn numbered(key: &str, prefix: &str) -> bool {
    key.strip_prefix(prefix)
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether `storage_type` can snapshot `volume` of a guest of `kind`;
/// `None` when the type is unknown.
fn storage_snapshots(kind: GuestKind, storage_type: &str, volume: &str) -> Option<bool> {
    if SNAPSHOT_STORAGE.contains(&storage_type) {
        return Some(true);
    }
    if FILE_STORAGE.contains(&storage_type) {
        return Some(kind == GuestKind::Qemu && volume.ends_with(".qcow2"));
    }
    match storage_type {
        "lvm" | "iscsi" | "iscsidirect" => Some(false),
        _ => None,
    }
}

/// Proxmox's answer `can` about a guest of `kind`, explained from its
/// `config` and the `storage_types` its node shows.
pub fn explain(
    kind: GuestKind,
    can: bool,
    config: &Map<String, Value>,
    storage_types: &HashMap<String, String>,
) -> Check {
    let volumes = volumes(kind, config);
    if can {
        // Proxmox versions differ on host paths: when one snapshots the
        // container anyway, the host path is not in the snapshot.
        let skipped: Vec<String> = volumes
            .iter()
            .filter_map(|v| v.host_path().map(|what| format!("{}, {what}", v.at())))
            .collect();
        return Check {
            cannot_snapshot: None,
            left_out: (!skipped.is_empty()).then(|| skipped.join("; ")),
        };
    }
    let host_path = volumes.iter().find_map(|v| {
        v.host_path()
            .map(|what| format!("{} is {what}, which Proxmox cannot snapshot", v.at()))
    });
    let storage = || {
        volumes.iter().find_map(|v| {
            let storage = v.storage()?;
            let kind_of = storage_types.get(storage)?;
            (storage_snapshots(kind, kind_of, v.volume) == Some(false)).then(|| {
                format!(
                    "{} is on storage {storage} ({kind_of}), which cannot take snapshots",
                    v.key
                )
            })
        })
    };
    let why = host_path.or_else(storage).unwrap_or_else(|| {
        format!(
            "Proxmox reports that the {}'s volumes cannot take snapshots",
            kind.noun()
        )
    });
    Check {
        cannot_snapshot: Some(super::bounded(&why)),
        left_out: None,
    }
}

/// Between two discovery passes (ADR-0093); the first runs at once.
pub const DISCOVER_EVERY: Duration = Duration::from_secs(600);

/// Discovers guests every [`DISCOVER_EVERY`], then checks mappings due,
/// every [`POLL`] until `stop`.
pub async fn run(snapshotter: ProxmoxSnapshotter, stop: impl Future<Output = ()>) {
    tokio::pin!(stop);
    let mut discovered: Option<tokio::time::Instant> = None;
    loop {
        if discovered.is_none_or(|at| at.elapsed() >= DISCOVER_EVERY) {
            discovered = Some(tokio::time::Instant::now());
            match snapshotter.discover().await {
                Ok(found) => tracing::info!(
                    guests = found.guests,
                    mapped = found.mapped,
                    unmapped = found.unmapped,
                    moved = found.moved,
                    ambiguous = found.ambiguous,
                    "Proxmox discovery pass"
                ),
                Err(error) => tracing::warn!(%error, "Proxmox discovery failed"),
            }
        }
        if let Err(error) = snapshotter.check_due(EVERY).await {
            tracing::warn!(%error, "cannot check Proxmox mappings");
        }
        tokio::select! {
            _ = &mut stop => return,
            _ = sleep(POLL) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn config(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    fn types(list: &[(&str, &str)]) -> HashMap<String, String> {
        list.iter()
            .map(|(s, t)| (s.to_string(), t.to_string()))
            .collect()
    }

    #[test]
    fn container_volumes_and_mount_points_are_read() {
        let c = config(json!({
            "mp1": "/mnt/media,mp=/media",
            "rootfs": "local-zfs:subvol-104-disk-0,size=8G",
            "mp0": "volume=local-zfs:subvol-104-disk-1,mp=/data,backup=1",
            "mp10": "/dev/sdb1,mp=/scratch",
            "hostname": "web-1",
            "net0": "name=eth0,bridge=vmbr0,hwaddr=BC:24:11:00:00:01",
        }));
        let found = volumes(GuestKind::Lxc, &c);
        let keys: Vec<_> = found.iter().map(|v| (v.key, v.volume, v.path)).collect();
        assert_eq!(
            keys,
            [
                ("rootfs", "local-zfs:subvol-104-disk-0", None),
                ("mp0", "local-zfs:subvol-104-disk-1", Some("/data")),
                ("mp1", "/mnt/media", Some("/media")),
                ("mp10", "/dev/sdb1", Some("/scratch")),
            ]
        );
    }

    #[test]
    fn vm_disks_leave_cdroms_and_unused_volumes_aside() {
        let c = config(json!({
            "scsi0": "local-lvm:vm-101-disk-0,size=32G",
            "ide2": "local:iso/debian.iso,media=cdrom",
            "ide0": "none,media=cdrom",
            "efidisk0": "local-lvm:vm-101-disk-1,size=4M",
            "unused0": "local-lvm:vm-101-disk-2",
            "scsihw": "virtio-scsi-pci",
        }));
        let keys: Vec<_> = volumes(GuestKind::Qemu, &c).iter().map(|v| v.key).collect();
        assert_eq!(keys, ["scsi0", "efidisk0"]);
    }

    #[test]
    fn a_container_on_dir_storage_cannot_be_snapshotted() {
        let c = config(json!({ "rootfs": "local:104/vm-104-disk-0.raw,size=8G" }));
        let found = explain(GuestKind::Lxc, false, &c, &types(&[("local", "dir")]));
        assert_eq!(
            found.cannot_snapshot.as_deref(),
            Some("rootfs is on storage local (dir), which cannot take snapshots")
        );
        assert_eq!(found.left_out, None);
    }

    #[test]
    fn a_bind_mount_is_named_before_storage() {
        let c = config(json!({
            "rootfs": "local:104/vm-104-disk-0.raw,size=8G",
            "mp0": "/srv/shared,mp=/shared",
        }));
        let found = explain(GuestKind::Lxc, false, &c, &types(&[("local", "dir")]));
        assert_eq!(
            found.cannot_snapshot.as_deref(),
            Some("mp0 at /shared is a bind mount, which Proxmox cannot snapshot")
        );
        let device = config(json!({ "rootfs": "tank:subvol-1,size=8G", "mp0": "/dev/sdb1,mp=/x" }));
        let found = explain(GuestKind::Lxc, false, &device, &HashMap::new());
        assert!(found.cannot_snapshot.unwrap().contains("a device mount"));
    }

    #[test]
    fn host_paths_a_snapshot_skips_are_left_out() {
        let c = config(json!({
            "rootfs": "local-zfs:subvol-104-disk-0,size=8G",
            "mp0": "/srv/shared,mp=/shared",
        }));
        let found = explain(GuestKind::Lxc, true, &c, &HashMap::new());
        assert_eq!(found.cannot_snapshot, None);
        assert_eq!(
            found.left_out.as_deref(),
            Some("mp0 at /shared, a bind mount")
        );
        let plain = config(json!({ "rootfs": "local-zfs:subvol-104-disk-0,size=8G" }));
        assert_eq!(
            explain(GuestKind::Lxc, true, &plain, &HashMap::new()),
            Check::default()
        );
    }

    #[test]
    fn vm_disks_on_file_storage_snapshot_as_qcow2_only() {
        let t = types(&[("local", "dir"), ("san", "lvm"), ("fast", "lvmthin")]);
        let qcow = config(json!({ "scsi0": "local:101/vm-101-disk-0.qcow2,size=32G" }));
        let raw = config(json!({ "scsi0": "local:101/vm-101-disk-0.raw,size=32G" }));
        let lvm =
            config(json!({ "virtio0": "fast:vm-101-disk-0", "virtio1": "san:vm-101-disk-1" }));
        // Proxmox said no, for a reason the configuration does not show.
        assert!(explain(GuestKind::Qemu, false, &qcow, &t)
            .cannot_snapshot
            .unwrap()
            .starts_with("Proxmox reports that the VM's"));
        assert_eq!(
            explain(GuestKind::Qemu, false, &raw, &t)
                .cannot_snapshot
                .as_deref(),
            Some("scsi0 is on storage local (dir), which cannot take snapshots")
        );
        assert_eq!(
            explain(GuestKind::Qemu, false, &lvm, &t)
                .cannot_snapshot
                .as_deref(),
            Some("virtio1 is on storage san (lvm), which cannot take snapshots")
        );
        // The VM's disks are not host paths, whatever Proxmox says.
        assert_eq!(explain(GuestKind::Qemu, true, &raw, &t), Check::default());
    }

    #[test]
    fn unknown_storage_types_fall_back_to_proxmox_s_word() {
        let c = config(json!({ "rootfs": "custom:vol,size=8G" }));
        let found = explain(
            GuestKind::Lxc,
            false,
            &c,
            &types(&[("custom", "someplugin")]),
        );
        assert_eq!(
            found.cannot_snapshot.as_deref(),
            Some("Proxmox reports that the container's volumes cannot take snapshots")
        );
    }
}
