//! Disk I/O on Windows: `IOCTL_DISK_PERFORMANCE` on each physical drive
//! (`\\.\PhysicalDrive0`, …), the counters behind the PhysicalDisk
//! performance object, read without PDH and its localized counter names.
//! See ADR-0092.
//!
//! The instance is `PhysicalDrive0`, as Windows names the drive. Busy time
//! is the time since the previous reading minus idle time; the time reads
//! and writes took is both the wait (per I/O) and the queue (over the
//! interval), as PhysicalDisk's "Avg. Disk sec/Transfer" and "Avg. Disk
//! Queue Length" compute them.

use std::io;
use std::ptr;

use tokio::time::{timeout_at, Instant};
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Ioctl::{DISK_PERFORMANCE, IOCTL_DISK_PERFORMANCE};
use windows_sys::Win32::System::IO::DeviceIoControl;

use super::{monotonic, select_drives, Counters, Device, DiskIoSensor, SECTOR};
use crate::win::{wide, Owned};
use crate::{Sample, Sensor, SensorError};

/// Drive numbers tried; numbers can have gaps when a disk is removed.
const MAX_DRIVES: u32 = 64;
/// 100 ns units per millisecond.
const TICKS_PER_MS: i64 = 10_000;
/// Time kept before the deadline to give up on a stuck drive.
const MARGIN: std::time::Duration = std::time::Duration::from_millis(200);

/// The counters of one reading. Times are in 100 ns; `QueryTime` is the
/// system time of the reading and `IdleTime` the idle time so far, so
/// their difference grows by the busy time.
fn counters(p: &DISK_PERFORMANCE) -> Counters {
    let ms = |ticks: i64| u64::try_from(ticks / TICKS_PER_MS).unwrap_or(0);
    let bytes = |b: i64| u64::try_from(b).unwrap_or(0) / SECTOR;
    let io = ms(p.ReadTime.saturating_add(p.WriteTime));
    Counters {
        reads: u64::from(p.ReadCount),
        writes: u64::from(p.WriteCount),
        read_sectors: bytes(p.BytesRead),
        written_sectors: bytes(p.BytesWritten),
        busy_ms: ms(p.QueryTime.saturating_sub(p.IdleTime)),
        io_ms: io,
        queue_ms: io,
        in_flight: u64::from(p.QueueDepth),
    }
}

/// The performance counters of drive `n`; `Ok(None)` when it does not
/// exist.
fn drive(n: u32) -> io::Result<Option<DISK_PERFORMANCE>> {
    let path = wide(format!(r"\\.\PhysicalDrive{n}"));
    // SAFETY: a NUL-terminated path; no access rights are asked, which is
    // enough for this IOCTL (`FILE_ANY_ACCESS`).
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null(),
            OPEN_EXISTING,
            0,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        let error = io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(code) if code == ERROR_FILE_NOT_FOUND as i32 => Ok(None),
            _ => Err(error),
        };
    }
    let handle = Owned(handle);
    let mut perf = DISK_PERFORMANCE::default();
    let mut returned = 0;
    // SAFETY: the output buffer and its size match; no input, no overlap.
    let ok = unsafe {
        DeviceIoControl(
            handle.0,
            IOCTL_DISK_PERFORMANCE,
            ptr::null(),
            0,
            (&mut perf as *mut DISK_PERFORMANCE).cast(),
            std::mem::size_of::<DISK_PERFORMANCE>() as u32,
            &mut returned,
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Some(perf))
}

/// Every physical drive that answers.
fn drives() -> Vec<Device> {
    let mut devices = Vec::new();
    for n in 0..MAX_DRIVES {
        match drive(n) {
            Ok(Some(perf)) => devices.push(Device {
                name: format!("PhysicalDrive{n}"),
                id: n.to_string(),
                counters: counters(&perf),
            }),
            Ok(None) => {}
            Err(error) => tracing::debug!(drive = n, %error, "IOCTL_DISK_PERFORMANCE failed"),
        }
    }
    devices
}

#[async_trait::async_trait]
impl Sensor for DiskIoSensor {
    async fn collect(&self, deadline: Instant) -> Result<Vec<Sample>, SensorError> {
        let now = Instant::now();
        let cutoff = deadline - MARGIN.min(deadline.saturating_duration_since(now) / 2);
        // Drive I/O controls go to the storage stack; keep them off the
        // runtime.
        let devices = match timeout_at(cutoff, tokio::task::spawn_blocking(drives)).await {
            Ok(Ok(devices)) => devices,
            Ok(Err(e)) => return Err(SensorError::Transient(format!("reading drives: {e}"))),
            Err(_) => {
                return Err(SensorError::Transient(
                    "reading drives did not finish in time".into(),
                ))
            }
        };
        Ok(self.round(monotonic(), select_drives(devices)))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn counters_from_a_reading() {
        let perf = DISK_PERFORMANCE {
            BytesRead: 4096,
            BytesWritten: 1024,
            ReadTime: 30_000,
            WriteTime: 20_000,
            IdleTime: 7 * TICKS_PER_MS,
            ReadCount: 3,
            WriteCount: 2,
            QueueDepth: 1,
            QueryTime: 10 * TICKS_PER_MS,
            ..Default::default()
        };
        assert_eq!(
            counters(&perf),
            Counters {
                reads: 3,
                writes: 2,
                read_sectors: 8,
                written_sectors: 2,
                busy_ms: 3,
                io_ms: 5,
                queue_ms: 5,
                in_flight: 1,
            }
        );
    }

    #[tokio::test]
    async fn collects_on_this_host() {
        let devices = drives();
        let system = devices
            .iter()
            .find(|d| d.name == "PhysicalDrive0")
            .expect("drive 0 answers");
        assert!(system.counters.reads > 0, "{system:?}");
        let sensor = DiskIoSensor::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        assert!(sensor.collect(deadline).await.unwrap().is_empty());
        // Some I/O between the two readings.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f"), vec![1u8; 1 << 20]).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        let samples = sensor.collect(deadline).await.unwrap();
        let busy = samples
            .iter()
            .find(|s| s.instance == "PhysicalDrive0" && s.sensor == "busy_ratio")
            .expect("busy ratio of drive 0");
        assert!((0.0..=1.0).contains(&busy.value), "{busy:?}");
        assert!(samples
            .iter()
            .all(|s| s.value.is_finite() && s.value >= 0.0));
        assert_eq!(samples.len() % 7, 0);
    }
}
