//! Memory on Windows, from `GlobalMemoryStatusEx`. See ADR-0048.
//!
//! Windows has no swap partition: `swap_total_bytes` is the page files'
//! size, the commit limit minus physical memory, and `swap_free_bytes`
//! the commit still available beyond the physical memory available.
//! Both are estimates; their sum with RAM matches the commit charge.

use std::io;

use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

use super::{MemInfo, MemorySensor};
use crate::{Sample, Sensor, SensorError};

/// What the sensor reports from `GlobalMemoryStatusEx`.
pub fn mem_info(status: &MEMORYSTATUSEX) -> MemInfo {
    let swap_total = status.ullTotalPageFile.saturating_sub(status.ullTotalPhys);
    MemInfo {
        total: status.ullTotalPhys,
        available: status.ullAvailPhys,
        swap_total,
        swap_free: status
            .ullAvailPageFile
            .saturating_sub(status.ullAvailPhys)
            .min(swap_total),
    }
}

#[async_trait::async_trait]
impl Sensor for MemorySensor {
    async fn collect(&self, _deadline: tokio::time::Instant) -> Result<Vec<Sample>, SensorError> {
        let mut status = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        // SAFETY: a valid structure with its length set.
        if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
            let error = io::Error::last_os_error();
            return Err(SensorError::Transient(format!(
                "GlobalMemoryStatusEx: {error}"
            )));
        }
        Ok(mem_info(&status).samples())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_files_are_the_swap() {
        let status = MEMORYSTATUSEX {
            ullTotalPhys: 8000,
            ullAvailPhys: 3000,
            ullTotalPageFile: 10000,
            ullAvailPageFile: 4500,
            ..Default::default()
        };
        let info = mem_info(&status);
        assert_eq!(
            info,
            MemInfo {
                total: 8000,
                available: 3000,
                swap_total: 2000,
                swap_free: 1500,
            }
        );
        // No page file: the commit limit can be just under RAM.
        let none = MEMORYSTATUSEX {
            ullTotalPageFile: 7900,
            ullAvailPageFile: 2900,
            ..status
        };
        assert_eq!(
            (mem_info(&none).swap_total, mem_info(&none).swap_free),
            (0, 0)
        );
    }

    #[tokio::test]
    async fn collects_on_this_host() {
        let samples = MemorySensor
            .collect(tokio::time::Instant::now())
            .await
            .unwrap();
        assert_eq!(samples.len(), 6);
        assert_eq!(samples[0].sensor, "total_bytes");
        assert!(samples[0].value > 0.0);
    }
}
