//! Disk space on Windows. See ADR-0048.
//!
//! Volumes are the fixed drives with a letter (`GetLogicalDrives`,
//! `GetDriveTypeW`): removable, optical, network and RAM drives are
//! skipped, and so are volumes mounted only in a folder. The instance is
//! the drive root (`C:\`), `fstype` the file system in lower case (`ntfs`,
//! `refs`) and `device` the volume GUID path. Space comes from
//! `GetDiskFreeSpaceExW`; `available_bytes` honours quotas of the agent's
//! account. NTFS has no inode limit, so no inode metrics.
//!
//! `fstypes_include`/`fstypes_exclude` apply to the lower-case type and
//! `mounts_exclude` to drives, `D:` or `D:\`, without case.

use std::collections::HashSet;
use std::io;

use tokio::time::{timeout_at, Instant};
use windows_sys::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
    GetVolumeNameForVolumeMountPointW,
};
use windows_sys::Win32::System::WindowsProgramming::DRIVE_FIXED;

use super::{DiskSensor, Filter, Mount, Usage, MARGIN};
use crate::win::{from_wide, wide};
use crate::{Sample, Sensor, SensorError};

/// The fixed drives, as mounts.
fn volumes() -> Vec<Mount> {
    // SAFETY: no arguments.
    let mask = unsafe { GetLogicalDrives() };
    let mut mounts = Vec::new();
    for letter in (b'A'..=b'Z').filter(|l| mask & (1 << (l - b'A')) != 0) {
        let root = format!("{}:\\", letter as char);
        let name = wide(&root);
        // SAFETY: a NUL-terminated root path.
        if unsafe { GetDriveTypeW(name.as_ptr()) } != DRIVE_FIXED {
            continue;
        }
        let mut fstype = [0u16; 64];
        // SAFETY: the buffer and its length match; other outputs are
        // optional.
        let ok = unsafe {
            GetVolumeInformationW(
                name.as_ptr(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                fstype.as_mut_ptr(),
                fstype.len() as u32,
            )
        };
        if ok == 0 {
            // A locked BitLocker volume, or one without a file system.
            let error = io::Error::last_os_error();
            tracing::debug!(volume = %root, %error, "GetVolumeInformationW failed");
            continue;
        }
        let mut guid = [0u16; 64];
        // SAFETY: the buffer and its length match.
        let source = if unsafe {
            GetVolumeNameForVolumeMountPointW(name.as_ptr(), guid.as_mut_ptr(), guid.len() as u32)
        } != 0
        {
            from_wide(&guid)
        } else {
            root.clone()
        };
        mounts.push(Mount {
            device_id: source.clone(),
            mount_point: root,
            fstype: from_wide(&fstype).to_lowercase(),
            source,
        });
    }
    mounts
}

/// The drive part of an excluded mount, `d:` for `D:\`; `None` for paths
/// that cannot name a drive root, like `/var` or `D:\data`.
fn drive(path: &str) -> Option<String> {
    let path = path.trim_end_matches('\\');
    let b = path.as_bytes();
    (b.len() == 2 && b[1] == b':').then(|| path.to_ascii_lowercase())
}

/// Volumes to report: types not skipped, drives not excluded, each volume
/// once.
pub fn select(filter: &Filter, mounts: Vec<Mount>) -> Vec<Mount> {
    let excluded: HashSet<String> = filter
        .excluded_mounts
        .iter()
        .filter_map(|m| drive(m))
        .collect();
    let mut devices = HashSet::new();
    mounts
        .into_iter()
        .filter(|m| !filter.skipped_fstypes.contains(&m.fstype))
        .filter(|m| !drive(&m.mount_point).is_some_and(|d| excluded.contains(&d)))
        .filter(|m| devices.insert(m.device_id.clone()))
        .collect()
}

fn free_space(root: &str) -> io::Result<Usage> {
    let name = wide(root);
    let (mut available, mut total, mut free) = (0, 0, 0);
    // SAFETY: a NUL-terminated path and three out pointers.
    if unsafe { GetDiskFreeSpaceExW(name.as_ptr(), &mut available, &mut total, &mut free) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Usage {
        total,
        free,
        available,
        inodes: 0,
        inodes_free: 0,
    })
}

#[async_trait::async_trait]
impl Sensor for DiskSensor {
    async fn collect(&self, deadline: Instant) -> Result<Vec<Sample>, SensorError> {
        let now = Instant::now();
        let cutoff = deadline - MARGIN.min(deadline.saturating_duration_since(now) / 2);
        // Volume queries go to the storage stack; keep them off the runtime.
        let mounts = match timeout_at(cutoff, tokio::task::spawn_blocking(volumes)).await {
            Ok(Ok(mounts)) => mounts,
            Ok(Err(e)) => return Err(SensorError::Transient(format!("listing volumes: {e}"))),
            Err(_) => {
                return Err(SensorError::Transient(
                    "listing volumes did not finish in time".into(),
                ))
            }
        };
        let mounts = select(&self.filter, mounts);
        Ok(self.stat_all(mounts, cutoff, free_space).await)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn volume(root: &str, fstype: &str, guid: &str) -> Mount {
        Mount {
            device_id: guid.into(),
            mount_point: root.into(),
            fstype: fstype.into(),
            source: guid.into(),
        }
    }

    #[test]
    fn drives_are_filtered_without_case() {
        let mounts = vec![
            volume("C:\\", "ntfs", "v1"),
            volume("D:\\", "ntfs", "v2"),
            volume("E:\\", "fat32", "v3"),
            volume("F:\\", "refs", "v1"),
        ];
        let roots = |filter: &Filter| -> Vec<String> {
            select(filter, mounts.clone())
                .into_iter()
                .map(|m| m.mount_point)
                .collect()
        };
        assert_eq!(roots(&Filter::default()), ["C:\\", "D:\\", "E:\\"]);
        let strings = |s: &[&str]| s.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let filter = Filter::new(
            &[],
            &strings(&["fat32"]),
            &strings(&["d:", "/var", "C:\\data"]),
        );
        assert_eq!(roots(&filter), ["C:\\"]);
        assert_eq!(drive("D:\\"), Some("d:".into()));
        assert_eq!(drive("D:\\x"), None);
    }

    #[tokio::test]
    async fn collects_on_this_host() {
        let mounts = volumes();
        let system = mounts
            .iter()
            .find(|m| m.mount_point == "C:\\")
            .expect("drive C: is fixed");
        assert!(system.source.starts_with("\\\\?\\Volume{"), "{system:?}");
        let deadline = Instant::now() + Duration::from_secs(5);
        let samples = DiskSensor::default().collect(deadline).await.unwrap();
        let used = samples
            .iter()
            .find(|s| s.instance == "C:\\" && s.sensor == "used_ratio")
            .unwrap();
        assert!((0.0..=1.0).contains(&used.value));
        assert_eq!(used.labels["fstype"], "ntfs");
        assert!(samples.iter().all(|s| !s.sensor.starts_with("inodes")));
    }
}
