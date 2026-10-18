//! Space and inodes per mounted filesystem, from `/proc/self/mountinfo`
//! and `statvfs`.
//!
//! Metrics (prefixed with the configured name, `disk` by default), with
//! the mount point as instance and labels `fstype` and `device`:
//! `total_bytes`, `free_bytes`, `available_bytes` (for unprivileged
//! users), `used_ratio` (used over used plus available, like `df`), and
//! `inodes`, `inodes_free` when the filesystem has an inode limit.
//!
//! Parameters, comma-separated lists:
//! - `fstypes_include`: types to report even though they are skipped by
//!   default, like `tmpfs`;
//! - `fstypes_exclude`: more types to skip;
//! - `mounts_exclude`: mount points to skip, with everything below them.
//!
//! `statvfs` can hang on a dead network filesystem: each call runs on the
//! blocking pool and is bounded by the deadline, and a mount whose call is
//! still running is skipped until it returns, so a hung mount holds one
//! thread, not one per tick.
//!
//! TODO: I/O counters from `/proc/diskstats`.

use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::task::JoinSet;
use tokio::time::{timeout_at, Instant};

use crate::procfs;
use crate::registry::{BuildError, Params};
use crate::{Sample, Sensor, SensorError};

const MOUNTINFO: &str = "/proc/self/mountinfo";
/// Thousands of mounts on a busy container host still fit.
const MOUNTINFO_CAP: usize = 4 << 20;
/// Time kept before the deadline to return the samples gathered so far.
const MARGIN: Duration = Duration::from_millis(200);

/// Pseudo, virtual and read-only image filesystems, skipped by default.
pub const SKIPPED_FSTYPES: &[&str] = &[
    "autofs",
    "binfmt_misc",
    "bpf",
    "cgroup",
    "cgroup2",
    "configfs",
    "debugfs",
    "devpts",
    "devtmpfs",
    "efivarfs",
    "erofs",
    "fuse.gvfsd-fuse",
    "fuse.lxcfs",
    "fuse.portal",
    "fusectl",
    "hugetlbfs",
    "mqueue",
    "nfsd",
    "nsfs",
    "overlay",
    "proc",
    "pstore",
    "ramfs",
    "rootfs",
    "rpc_pipefs",
    "securityfs",
    "selinuxfs",
    "squashfs",
    "sysfs",
    "tmpfs",
    "tracefs",
];

/// One line of `/proc/self/mountinfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    /// `major:minor` of the filesystem.
    pub device_id: String,
    pub mount_point: String,
    pub fstype: String,
    /// The mount source, like `/dev/sda1` or `server:/export`.
    pub source: String,
}

/// Parses `/proc/self/mountinfo` (see proc(5)): ID, parent ID,
/// `major:minor`, root, mount point, options, optional fields, `-`, type,
/// source, super options. Paths escape space, tab, newline and backslash
/// as octal (`\040`).
pub fn parse_mountinfo(text: &str) -> Result<Vec<Mount>, SensorError> {
    let mut mounts = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let fields: Vec<&str> = line.split(' ').collect();
        let tail = fields.iter().position(|f| *f == "-").filter(|&i| i >= 6);
        let (Some(tail), Some(device_id), Some(mount_point)) = (tail, fields.get(2), fields.get(4))
        else {
            return Err(procfs::malformed(MOUNTINFO, "odd line"));
        };
        let (Some(fstype), Some(source)) = (fields.get(tail + 1), fields.get(tail + 2)) else {
            return Err(procfs::malformed(MOUNTINFO, "line without type or source"));
        };
        mounts.push(Mount {
            device_id: (*device_id).to_owned(),
            mount_point: unescape(mount_point),
            fstype: (*fstype).to_owned(),
            source: unescape(source),
        });
    }
    Ok(mounts)
}

/// Undoes the kernel's `\ooo` escapes; anything else is kept as is.
fn unescape(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let octal = bytes.get(i + 1..i + 4).filter(|d| {
            bytes[i] == b'\\' && d.iter().all(|b| (b'0'..=b'7').contains(b)) && d[0] <= b'3'
        });
        match octal {
            Some(d) => {
                out.push((d[0] - b'0') * 64 + (d[1] - b'0') * 8 + (d[2] - b'0'));
                i += 4;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Which mounts to report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    skipped_fstypes: BTreeSet<String>,
    excluded_mounts: Vec<String>,
}

impl Default for Filter {
    fn default() -> Self {
        Self {
            skipped_fstypes: SKIPPED_FSTYPES.iter().map(|t| t.to_string()).collect(),
            excluded_mounts: Vec::new(),
        }
    }
}

impl Filter {
    pub fn new(include: &[String], exclude: &[String], mounts_exclude: &[String]) -> Self {
        let mut filter = Self::default();
        for fstype in include {
            filter.skipped_fstypes.remove(fstype);
        }
        filter.skipped_fstypes.extend(exclude.iter().cloned());
        filter.excluded_mounts = mounts_exclude
            .iter()
            .map(|m| m.trim_end_matches('/').to_owned())
            .collect();
        filter
    }

    fn excluded(&self, mount_point: &str) -> bool {
        self.excluded_mounts.iter().any(|m| {
            mount_point
                .strip_prefix(m.as_str())
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/') || m.is_empty())
        })
    }

    /// Mounts to report, in mountinfo order. A filesystem mounted more
    /// than once (bind mounts) is reported at its first mount point.
    pub fn select(&self, mounts: Vec<Mount>) -> Vec<Mount> {
        let mut devices = HashSet::new();
        mounts
            .into_iter()
            .filter(|m| !self.skipped_fstypes.contains(&m.fstype))
            .filter(|m| !self.excluded(&m.mount_point))
            .filter(|m| devices.insert(m.device_id.clone()))
            .collect()
    }
}

/// What `statvfs` says about one filesystem, in bytes and inodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub total: u64,
    pub free: u64,
    pub available: u64,
    pub inodes: u64,
    pub inodes_free: u64,
}

impl From<rustix::fs::StatVfs> for Usage {
    fn from(s: rustix::fs::StatVfs) -> Self {
        let bytes = |blocks: u64| blocks.saturating_mul(s.f_frsize);
        Self {
            total: bytes(s.f_blocks),
            free: bytes(s.f_bfree),
            available: bytes(s.f_bavail),
            inodes: s.f_files,
            inodes_free: s.f_ffree,
        }
    }
}

impl Usage {
    pub fn samples(&self, mount: &Mount) -> Vec<Sample> {
        let sample = |metric: &str, value: f64| {
            Sample::metric(metric, value)
                .with_instance(&mount.mount_point)
                .with_label("fstype", &mount.fstype)
                .with_label("device", &mount.source)
        };
        let mut samples = vec![
            sample("total_bytes", self.total as f64),
            sample("free_bytes", self.free as f64),
            sample("available_bytes", self.available as f64),
        ];
        let used = self.total.saturating_sub(self.free);
        if let Some(usable) = used.checked_add(self.available).filter(|u| *u > 0) {
            samples.push(sample("used_ratio", used as f64 / usable as f64));
        }
        // btrfs and some network filesystems have no inode limit.
        if self.inodes > 0 {
            samples.push(sample("inodes", self.inodes as f64));
            samples.push(sample("inodes_free", self.inodes_free as f64));
        }
        samples
    }
}

#[derive(Debug, Default)]
pub struct DiskSensor {
    filter: Filter,
    /// Mount points whose `statvfs` call is still running.
    in_flight: Arc<Mutex<HashSet<String>>>,
    /// Mount points reported as stuck, to log recovery once.
    stuck: Mutex<BTreeSet<String>>,
}

/// Removes a mount point from the in-flight set when its blocking call
/// returns, or when the task is dropped before it ran.
struct InFlight {
    set: Arc<Mutex<HashSet<String>>>,
    mount_point: String,
}

impl Drop for InFlight {
    fn drop(&mut self) {
        lock(&self.set).remove(&self.mount_point);
    }
}

/// The sets are always left consistent, so a poisoned lock is still fine.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

impl DiskSensor {
    pub fn new(filter: Filter) -> Self {
        Self {
            filter,
            ..Self::default()
        }
    }

    pub(crate) fn from_params(params: &mut Params<'_>) -> Result<Self, BuildError> {
        let include = params.list("fstypes_include")?.unwrap_or_default();
        let exclude = params.list("fstypes_exclude")?.unwrap_or_default();
        let mounts = params.list("mounts_exclude")?.unwrap_or_default();
        if mounts.iter().any(|m| !m.starts_with('/')) {
            return Err(params.bad("mounts_exclude", "must list absolute paths"));
        }
        Ok(Self::new(Filter::new(&include, &exclude, &mounts)))
    }

    /// Runs `stat` for each mount off the runtime and returns the samples
    /// of those that answered before `cutoff`.
    async fn stat_all<F>(&self, mounts: Vec<Mount>, cutoff: Instant, stat: F) -> Vec<Sample>
    where
        F: Fn(&str) -> std::io::Result<Usage> + Clone + Send + 'static,
    {
        let mut tasks = JoinSet::new();
        let mut pending = BTreeSet::new();
        for mount in mounts {
            if !lock(&self.in_flight).insert(mount.mount_point.clone()) {
                continue; // Still stuck since an earlier round.
            }
            let guard = InFlight {
                set: self.in_flight.clone(),
                mount_point: mount.mount_point.clone(),
            };
            pending.insert(mount.mount_point.clone());
            let stat = stat.clone();
            tasks.spawn_blocking(move || {
                let _guard = guard;
                let usage = stat(&mount.mount_point);
                (mount, usage)
            });
        }
        let mut samples = Vec::new();
        while let Ok(Some(joined)) = timeout_at(cutoff, tasks.join_next()).await {
            let Ok((mount, usage)) = joined else { continue };
            pending.remove(&mount.mount_point);
            match usage {
                Ok(usage) => samples.extend(usage.samples(&mount)),
                // Unmounted since mountinfo was read, or not accessible.
                Err(error) => {
                    tracing::debug!(mount = %mount.mount_point, %error, "statvfs failed")
                }
            }
        }
        // Calls still running are detached; their guards clean up.
        tasks.detach_all();
        let in_flight = lock(&self.in_flight).clone();
        let mut stuck = lock(&self.stuck);
        for mount in pending.iter().filter(|m| !stuck.contains(*m)) {
            tracing::warn!(mount = %mount, "statvfs did not answer before the deadline, skipping this mount until it does");
        }
        stuck.extend(pending);
        stuck.retain(|m| {
            let still = in_flight.contains(m);
            if !still {
                tracing::info!(mount = %m, "statvfs answers again");
            }
            still
        });
        samples
    }
}

fn statvfs(path: &str) -> std::io::Result<Usage> {
    Ok(rustix::fs::statvfs(path)?.into())
}

#[async_trait::async_trait]
impl Sensor for DiskSensor {
    async fn collect(&self, deadline: Instant) -> Result<Vec<Sample>, SensorError> {
        let mounts = parse_mountinfo(&procfs::read_capped(MOUNTINFO, MOUNTINFO_CAP)?)?;
        let mounts = self.filter.select(mounts);
        let now = Instant::now();
        let margin = MARGIN.min(deadline.saturating_duration_since(now) / 2);
        Ok(self.stat_all(mounts, deadline - margin, statvfs).await)
    }
}

#[cfg(test)]
mod tests;
