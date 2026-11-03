//! Disk I/O per block device: operations, bytes, busy time, wait and
//! queue, from `/proc/diskstats` on Linux. See ADR-0092.
//!
//! Metrics (prefixed with the configured name, `diskio` by default), with
//! the device as instance (`sda`, `nvme0n1`, `md0`, or the device-mapper
//! name of `dm-0`, like `vg0-root`):
//! - `reads_per_second`, `writes_per_second`: completed operations;
//! - `read_bytes_per_second`, `written_bytes_per_second`;
//! - `busy_ratio`: share of the time with at least one I/O in flight;
//! - `wait_seconds`: average time an I/O took, queue included; the whole
//!   interval when I/Os are in flight and none completed (a stuck disk);
//! - `queue_depth`: average number of I/Os in flight.
//!
//! All are averages over the time since the previous collection, so the
//! first collection only primes the state. Whole disks, md and dm devices
//! are reported; partitions, loop, RAM, zram, floppy and optical devices
//! are not, nor devices that never did any I/O since boot. A device that
//! appears is primed, one that disappears is forgotten, and one whose
//! counters went back or whose number changed is primed again. Counters
//! that wrap at 2^32 (32-bit kernels, and the millisecond fields on every
//! kernel) are unwrapped.
//!
//! On Windows the counters come from `IOCTL_DISK_PERFORMANCE` on each
//! physical drive, see [`windows`](self).

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use crate::procfs::{self, field};
#[cfg(unix)]
use crate::Sensor;
use crate::{Sample, SensorError};

const DISKSTATS: &str = "/proc/diskstats";
/// One line per block device; thousands of devices still fit.
#[cfg(unix)]
const DISKSTATS_CAP: usize = 4 << 20;
/// `/proc/diskstats` counts 512-byte sectors whatever the device's own
/// sector size.
pub const SECTOR: u64 = 512;
/// Most devices reported per collection, to bound the series of one
/// agent (7 per device); the first ones in kernel order are kept.
pub const MAX_DEVICES: usize = 128;
/// Kernel name prefixes never reported: loop, RAM disks, compressed RAM
/// swap, floppy and optical drives.
pub const SKIPPED_PREFIXES: &[&str] = &["loop", "ram", "zram", "fd", "sr"];

/// The cumulative counters of one device.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    pub reads: u64,
    pub writes: u64,
    /// In 512-byte sectors, see [`SECTOR`].
    pub read_sectors: u64,
    pub written_sectors: u64,
    /// Milliseconds with at least one I/O in flight.
    pub busy_ms: u64,
    /// Milliseconds completed I/Os took, summed.
    pub io_ms: u64,
    /// Milliseconds weighted by the number of I/Os in flight.
    pub queue_ms: u64,
    /// I/Os in flight now; a gauge, not a counter.
    pub in_flight: u64,
}

/// One device as read from the system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// What the device is called on screen and in the instance.
    pub name: String,
    /// Identity of the device behind the name (`major:minor` on Linux):
    /// when it changes, the counters belong to another device.
    pub id: String,
    pub counters: Counters,
}

/// The increase of a counter, unwrapping one 32-bit wrap; `None` when it
/// went back from beyond 32 bits (a reset).
pub fn increase(prev: u64, cur: u64) -> Option<u64> {
    if cur >= prev {
        Some(cur - prev)
    } else if prev <= u64::from(u32::MAX) {
        Some(cur + (1 << 32) - prev)
    } else {
        None
    }
}

/// What one device did between two readings `elapsed_ms` apart.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rates {
    pub reads_per_second: f64,
    pub writes_per_second: f64,
    pub read_bytes_per_second: f64,
    pub written_bytes_per_second: f64,
    pub busy_ratio: f64,
    pub wait_seconds: f64,
    pub queue_depth: f64,
}

/// The rates between `prev` and `cur`, or `None` when nothing elapsed or
/// the counters do not follow each other (a reset, or a wrap that does
/// not fit the elapsed time: a device replaced under the same name).
pub fn rates(prev: &Counters, cur: &Counters, elapsed_ms: u64) -> Option<Rates> {
    if elapsed_ms == 0 {
        return None;
    }
    let busy = increase(prev.busy_ms, cur.busy_ms)?;
    // Kernels count busy time in jiffies; allow some slack.
    if busy > elapsed_ms + elapsed_ms / 4 + 1000 {
        return None;
    }
    let reads = increase(prev.reads, cur.reads)?;
    let writes = increase(prev.writes, cur.writes)?;
    let read = increase(prev.read_sectors, cur.read_sectors)?;
    let written = increase(prev.written_sectors, cur.written_sectors)?;
    let io = increase(prev.io_ms, cur.io_ms)?;
    let queue = increase(prev.queue_ms, cur.queue_ms)?;
    let seconds = elapsed_ms as f64 / 1000.0;
    let done = reads + writes;
    let wait_seconds = if done > 0 {
        io as f64 / done as f64 / 1000.0
    } else if cur.in_flight > 0 {
        // Nothing completed while I/Os wait: they waited all along.
        seconds
    } else {
        0.0
    };
    Some(Rates {
        reads_per_second: reads as f64 / seconds,
        writes_per_second: writes as f64 / seconds,
        read_bytes_per_second: (read * SECTOR) as f64 / seconds,
        written_bytes_per_second: (written * SECTOR) as f64 / seconds,
        busy_ratio: (busy as f64 / elapsed_ms as f64).clamp(0.0, 1.0),
        wait_seconds,
        queue_depth: queue as f64 / elapsed_ms as f64,
    })
}

impl Rates {
    pub fn samples(&self, instance: &str) -> Vec<Sample> {
        [
            ("reads_per_second", self.reads_per_second),
            ("writes_per_second", self.writes_per_second),
            ("read_bytes_per_second", self.read_bytes_per_second),
            ("written_bytes_per_second", self.written_bytes_per_second),
            ("busy_ratio", self.busy_ratio),
            ("wait_seconds", self.wait_seconds),
            ("queue_depth", self.queue_depth),
        ]
        .into_iter()
        .map(|(metric, value)| Sample::metric(metric, value).with_instance(instance))
        .collect()
    }
}

/// One line of `/proc/diskstats`, before selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stat {
    pub major: u32,
    pub minor: u32,
    /// The kernel name, like `sda` or `dm-0`.
    pub name: String,
    pub counters: Counters,
}

/// Parses `/proc/diskstats` (see the kernel's `iostats.rst`): major,
/// minor, name, then reads completed, merged, sectors read, ms reading,
/// writes completed, merged, sectors written, ms writing, I/Os in flight,
/// ms doing I/O, weighted ms, and on newer kernels discard and flush
/// fields, which are not used. Lines with fewer than the 11 fields (the
/// partitions of 2.6 kernels) are skipped.
pub fn parse_diskstats(text: &str) -> Result<Vec<Stat>, SensorError> {
    let mut stats = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let (Some(major), Some(minor), Some(name)) = (
            field::<u32>(line, 0),
            field::<u32>(line, 1),
            line.split_ascii_whitespace().nth(2),
        ) else {
            return Err(procfs::malformed(DISKSTATS, "odd line"));
        };
        let values: Vec<u64> = (3..14).map_while(|i| field(line, i)).collect();
        if values.len() < 11 {
            continue;
        }
        stats.push(Stat {
            major,
            minor,
            name: name.to_owned(),
            counters: Counters {
                reads: values[0],
                read_sectors: values[2],
                writes: values[4],
                written_sectors: values[6],
                io_ms: values[3].saturating_add(values[7]),
                in_flight: values[8],
                busy_ms: values[9],
                queue_ms: values[10],
            },
        });
    }
    Ok(stats)
}

/// Whether a kernel name looks like a partition, for hosts without
/// `/sys/block`: `sda1`, `vdb2`, `xvda1`, `nvme0n1p1`, `mmcblk0p1`,
/// `md0p1`.
pub fn looks_like_partition(name: &str) -> bool {
    let base = name.trim_end_matches(|c: char| c.is_ascii_digit());
    if base.len() == name.len() || base.is_empty() {
        return false;
    }
    let lettered = ["sd", "hd", "vd", "xvd"].iter().any(|p| {
        base.strip_prefix(p)
            .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_lowercase()))
    });
    lettered
        || base
            .strip_suffix('p')
            .is_some_and(|rest| rest.ends_with(|c: char| c.is_ascii_digit()))
}

/// The devices to report: whole devices (the names under `/sys/block`
/// when known, `whole`), not skipped by prefix, that did I/O since boot,
/// at most [`MAX_DEVICES`]. `alias` gives a device-mapper device its name.
pub fn select(
    stats: Vec<Stat>,
    whole: Option<&HashSet<String>>,
    alias: impl Fn(&str) -> Option<String>,
) -> Vec<Device> {
    let mut names = HashSet::new();
    let devices = stats
        .into_iter()
        .filter(|s| !SKIPPED_PREFIXES.iter().any(|p| s.name.starts_with(p)))
        .filter(|s| match whole {
            Some(whole) => whole.contains(&s.name),
            None => !looks_like_partition(&s.name),
        })
        .filter_map(|s| {
            let name = alias(&s.name)
                .filter(|a| !a.is_empty() && !names.contains(a))
                .unwrap_or(s.name);
            names.insert(name.clone()).then(|| Device {
                name,
                id: format!("{}:{}", s.major, s.minor),
                counters: s.counters,
            })
        })
        .collect();
    select_drives(devices)
}

/// The devices that did I/O since boot, at most [`MAX_DEVICES`].
pub fn select_drives(devices: Vec<Device>) -> Vec<Device> {
    devices
        .into_iter()
        .filter(|d| d.counters.reads > 0 || d.counters.writes > 0)
        .take(MAX_DEVICES)
        .collect()
}

#[derive(Debug, Default)]
pub struct DiskIoSensor {
    /// Devices of the previous collection by name, and when it was taken.
    previous: Mutex<Option<(Duration, BTreeMap<String, Device>)>>,
    /// Whether [`MAX_DEVICES`] was hit, to log it once.
    capped: AtomicBool,
}

impl DiskIoSensor {
    /// Samples for one collection of `devices` taken at `at`, a monotonic
    /// time; keeps them as the state for the next one.
    pub fn round(&self, at: Duration, devices: Vec<Device>) -> Vec<Sample> {
        if devices.len() >= MAX_DEVICES && !self.capped.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                max = MAX_DEVICES,
                "too many block devices, reporting disk I/O for the first ones only"
            );
        }
        let mut samples = Vec::new();
        // A panic while holding the lock cannot leave the map half-updated.
        let mut previous = self.previous.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((then, before)) = previous.as_ref() {
            let elapsed = at.saturating_sub(*then).as_millis() as u64;
            for device in &devices {
                let rates = before
                    .get(&device.name)
                    .filter(|prev| prev.id == device.id)
                    .and_then(|prev| rates(&prev.counters, &device.counters, elapsed));
                if let Some(rates) = rates {
                    samples.extend(rates.samples(&device.name));
                }
            }
        }
        let devices = devices.into_iter().map(|d| (d.name.clone(), d)).collect();
        *previous = Some((at, devices));
        samples
    }
}

/// The names under `/sys/block`, which lists whole devices only; `None`
/// when it cannot be read. Names with a `/` (`cciss/c0d0`) appear there
/// with a `!`.
#[cfg(unix)]
fn whole_devices() -> Option<HashSet<String>> {
    let entries = std::fs::read_dir("/sys/block").ok()?;
    Some(
        entries
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().replace('!', "/"))
            .collect(),
    )
}

/// The device-mapper name of `dm-N` (an LVM volume, a LUKS mapping).
#[cfg(unix)]
fn dm_name(kernel: &str) -> Option<String> {
    if !kernel.starts_with("dm-") {
        return None;
    }
    let text = procfs::read_capped(format!("/sys/block/{kernel}/dm/name"), 4096).ok()?;
    Some(text.trim().to_owned())
}

#[cfg(unix)]
#[async_trait::async_trait]
impl Sensor for DiskIoSensor {
    async fn collect(&self, _deadline: tokio::time::Instant) -> Result<Vec<Sample>, SensorError> {
        // /proc and /sys are generated in memory, cheap to read here.
        let stats = parse_diskstats(&procfs::read_capped(DISKSTATS, DISKSTATS_CAP)?)?;
        let devices = select(stats, whole_devices().as_ref(), dm_name);
        Ok(self.round(monotonic(), devices))
    }
}

/// Time since the first call, from a monotonic clock.
#[cfg(any(unix, windows))]
fn monotonic() -> Duration {
    static ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    ORIGIN.get_or_init(std::time::Instant::now).elapsed()
}

#[cfg(windows)]
mod windows;

#[cfg(test)]
mod tests;
