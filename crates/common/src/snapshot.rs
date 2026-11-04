//! Local snapshots of a Linux host's system volumes on LVM or ZFS
//! (ADR-0094): the names the server gives, and the rules that tell whether
//! a host can be snapshotted now. The agent's root helper applies them
//! before taking a snapshot, the server to say in advance what a change
//! would get, so both always agree.

use std::collections::BTreeMap;
use std::fmt;

use serde::Serialize;

use crate::proto::v1::{Volume, VolumeKind};

/// Every name starts with this, so a snapshot can be told apart on the
/// host; ownership is still the `monitorflow` tag or property.
pub const PREFIX: &str = "monitorflow-";
/// Longest name, well within LVM's 127 bytes with the origin appended.
pub const MAX_NAME: usize = 64;
/// The mount points snapshotted: where the system, its packages and their
/// state live. Data (`/home`, `/srv`, databases) is left alone, as rolling
/// it back would lose what the change never touched.
pub const SYSTEM_MOUNTS: [&str; 5] = ["/", "/usr", "/var", "/opt", "/boot"];
/// A thin pool or ZFS dataset needs this much free space, in percent:
/// a full thin pool stops every volume in it.
pub const MIN_FREE_PERCENT: f64 = 10.0;
/// A classic LVM snapshot gets this share of its origin, in percent...
pub const CLASSIC_PERCENT: u64 = 20;
/// ...and at least this many bytes, 1 GiB.
pub const CLASSIC_MIN: u64 = 1 << 30;

/// `monitorflow-` then lowercase letters and digits in groups joined by
/// single `-`, at most [`MAX_NAME`] bytes. Neither LVM nor ZFS gives `-`
/// or digits any meaning, and no name can read as an option or a path.
pub fn valid_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(PREFIX) else {
        return false;
    };
    name.len() <= MAX_NAME
        && !rest.is_empty()
        && rest.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

/// Size of a classic LVM snapshot of an origin of `origin_bytes`: 20 %, at
/// least 1 GiB, at most the origin itself, which can never overflow it.
pub fn classic_size(origin_bytes: u64) -> u64 {
    (origin_bytes / 100 * CLASSIC_PERCENT)
        .max(CLASSIC_MIN)
        .min(origin_bytes)
}

/// Whether volumes of `kind` can be snapshotted.
pub fn snapshottable(kind: VolumeKind) -> bool {
    matches!(
        kind,
        VolumeKind::Lvm | VolumeKind::LvmThin | VolumeKind::Zfs
    )
}

/// Why a host cannot be snapshotted now.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Refusal {
    /// No `/` among the volumes: the agent could not read its mounts.
    NoRoot,
    /// `/` is on storage MonitorFlow does not snapshot.
    RootNotCovered { fs_type: String },
    /// A volume group lacks the space for its classic snapshots.
    NoSpace {
        mount_point: String,
        group: String,
        free_bytes: u64,
        needed_bytes: u64,
    },
    /// A thin pool or ZFS dataset is too full.
    PoolFull {
        mount_point: String,
        free_bytes: u64,
        pool_bytes: u64,
    },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRoot => f.write_str("the agent cannot tell where / is"),
            Self::RootNotCovered { fs_type } => match fs_type.as_str() {
                "btrfs" => f.write_str("/ is on Btrfs, which MonitorFlow does not snapshot yet"),
                "zfs" => f.write_str("/ is on a ZFS dataset the agent cannot reach"),
                _ => write!(f, "/ is on plain {fs_type}"),
            },
            Self::NoSpace {
                mount_point,
                group,
                free_bytes,
                needed_bytes,
            } => write!(
                f,
                "volume group {group} has {} free, and a snapshot of {mount_point} needs {}",
                bytes(*free_bytes),
                bytes(*needed_bytes)
            ),
            Self::PoolFull {
                mount_point,
                free_bytes,
                pool_bytes,
            } => write!(
                f,
                "the storage of {mount_point} has only {} free of {}",
                bytes(*free_bytes),
                bytes(*pool_bytes)
            ),
        }
    }
}

/// What a snapshot would cover.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan<'a> {
    /// Volumes snapshotted, in the order given.
    pub covered: Vec<&'a Volume>,
    /// System volumes left out: partitions, Btrfs, an unreachable pool.
    pub uncovered: Vec<&'a Volume>,
}

/// What snapshotting `volumes` now would do, or why it cannot. `/` must be
/// covered; other system volumes are covered when they can be, and their
/// space is checked like `/`'s.
pub fn plan(volumes: &[Volume]) -> Result<Plan<'_>, Refusal> {
    let root = volumes
        .iter()
        .find(|v| v.mount_point == "/")
        .ok_or(Refusal::NoRoot)?;
    if !snapshottable(root.kind()) {
        return Err(Refusal::RootNotCovered {
            fs_type: root.fs_type.clone(),
        });
    }
    let (covered, uncovered): (Vec<&Volume>, Vec<&Volume>) =
        volumes.iter().partition(|v| snapshottable(v.kind()));
    // Classic snapshots of one group share its free space.
    let mut groups: BTreeMap<&str, (u64, u64, &Volume)> = BTreeMap::new();
    for volume in &covered {
        match volume.kind() {
            VolumeKind::Lvm => {
                let group = volume.source.split('/').next().unwrap_or_default();
                let entry = groups
                    .entry(group)
                    .or_insert((volume.free_bytes, 0, volume));
                entry.1 += classic_size(volume.size_bytes);
            }
            VolumeKind::LvmThin | VolumeKind::Zfs => {
                let free = percent(volume.free_bytes, volume.pool_bytes);
                let metadata_free = 100.0 - volume.metadata_percent;
                if free < MIN_FREE_PERCENT || metadata_free < MIN_FREE_PERCENT {
                    return Err(Refusal::PoolFull {
                        mount_point: volume.mount_point.clone(),
                        free_bytes: volume.free_bytes,
                        pool_bytes: volume.pool_bytes,
                    });
                }
            }
            _ => {}
        }
    }
    for (group, (free, needed, volume)) in groups {
        if free < needed {
            return Err(Refusal::NoSpace {
                mount_point: volume.mount_point.clone(),
                group: group.to_owned(),
                free_bytes: free,
                needed_bytes: needed,
            });
        }
    }
    Ok(Plan { covered, uncovered })
}

fn percent(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        return 0.0;
    }
    part as f64 * 100.0 / whole as f64
}

/// `bytes` in binary units with one decimal, like `40.0 GiB`.
pub fn bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    fn volume(mount: &str, kind: VolumeKind, source: &str, size: u64, free: u64) -> Volume {
        Volume {
            mount_point: mount.into(),
            fs_type: if kind == VolumeKind::Zfs {
                "zfs"
            } else {
                "ext4"
            }
            .into(),
            kind: kind.into(),
            source: source.into(),
            size_bytes: size,
            free_bytes: free,
            pool_bytes: 100 * GIB,
            metadata_percent: 0.0,
        }
    }

    #[test]
    fn names() {
        for good in [
            "monitorflow-incident-1a2b3c4d5e6f-2",
            "monitorflow-campaign-0123456789ab",
            "monitorflow-x",
        ] {
            assert!(valid_name(good), "{good}");
        }
        let long = format!("monitorflow-{}", "a".repeat(60));
        for bad in [
            "",
            "monitorflow-",
            "monitorflow--a",
            "monitorflow-a-",
            "monitorflow-A",
            "monitorflow-a_b",
            "monitorflow-a/b",
            "monitorflow-a@b",
            "monitorflow-a b",
            "-monitorflow-a",
            "other-a",
            long.as_str(),
        ] {
            assert!(!valid_name(bad), "{bad}");
        }
    }

    #[test]
    fn classic_sizes() {
        assert_eq!(classic_size(100 * GIB), 20 * GIB);
        assert_eq!(classic_size(2 * GIB), GIB);
        assert_eq!(classic_size(GIB / 2), GIB / 2);
    }

    #[test]
    fn root_on_a_partition_is_refused() {
        let volumes = [volume("/", VolumeKind::Plain, "/dev/sda1", 20 * GIB, 0)];
        let refusal = plan(&volumes).unwrap_err();
        assert_eq!(refusal.to_string(), "/ is on plain ext4");
        assert_eq!(plan(&[]).unwrap_err(), Refusal::NoRoot);
    }

    #[test]
    fn covers_what_it_can() {
        let volumes = [
            volume("/", VolumeKind::LvmThin, "vg0/root", 20 * GIB, 40 * GIB),
            volume("/boot", VolumeKind::Plain, "/dev/sda1", GIB, 0),
            volume("/var", VolumeKind::Zfs, "rpool/var", 20 * GIB, 50 * GIB),
        ];
        let plan = plan(&volumes).unwrap();
        let mounts = |v: &[&Volume]| v.iter().map(|v| v.mount_point.clone()).collect::<Vec<_>>();
        assert_eq!(mounts(&plan.covered), ["/", "/var"]);
        assert_eq!(mounts(&plan.uncovered), ["/boot"]);
    }

    #[test]
    fn a_full_thin_pool_is_refused() {
        let mut root = volume("/", VolumeKind::LvmThin, "vg0/root", 20 * GIB, 9 * GIB);
        let refusal = plan(std::slice::from_ref(&root)).unwrap_err();
        assert_eq!(
            refusal.to_string(),
            "the storage of / has only 9.0 GiB free of 100.0 GiB"
        );
        root.free_bytes = 50 * GIB;
        root.metadata_percent = 95.0;
        assert!(plan(std::slice::from_ref(&root)).is_err());
        root.metadata_percent = 50.0;
        assert!(plan(std::slice::from_ref(&root)).is_ok());
    }

    #[test]
    fn classic_snapshots_share_their_group() {
        // 20 % of 40 GiB and 20 % of 20 GiB: 12 GiB needed from vg0.
        let volumes = |free| {
            [
                volume("/", VolumeKind::Lvm, "vg0/root", 40 * GIB, free),
                volume("/var", VolumeKind::Lvm, "vg0/var", 20 * GIB, free),
            ]
        };
        assert!(plan(&volumes(12 * GIB)).is_ok());
        let refusal = plan(&volumes(11 * GIB)).unwrap_err();
        assert_eq!(
            refusal.to_string(),
            "volume group vg0 has 11.0 GiB free, and a snapshot of / needs 12.0 GiB"
        );
        let refusal = plan(&volumes(0)).unwrap_err();
        assert!(matches!(refusal, Refusal::NoSpace { free_bytes: 0, .. }));
    }

    #[test]
    fn byte_units() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(40 * GIB), "40.0 GiB");
        assert_eq!(bytes(1536 << 20), "1.5 GiB");
    }
}
