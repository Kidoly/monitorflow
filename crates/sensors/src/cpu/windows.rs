//! CPU usage on Windows: `GetSystemTimes` for the total and
//! `NtQuerySystemInformation(SystemProcessorPerformanceInformation)` per
//! core. Windows has no load average, so `load1`, `load5` and `load15` are
//! not reported. See ADR-0048.

use std::collections::BTreeMap;
use std::io;

use windows_sys::Wdk::System::SystemInformation::{
    NtQuerySystemInformation, SystemProcessorPerformanceInformation,
};
use windows_sys::Win32::Foundation::FILETIME;
use windows_sys::Win32::System::Threading::GetSystemTimes;
use windows_sys::Win32::System::WindowsProgramming::SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION;

use super::{CpuSensor, CpuTimes, TOTAL};
use crate::{Sample, Sensor, SensorError};

/// Most cores of one processor group, which is what the per-core query
/// returns for the calling thread.
const MAX_CORES: usize = 64;

/// Counters in 100 ns units. Windows counts idle time inside kernel time.
pub fn times(idle: u64, kernel: u64, user: u64) -> CpuTimes {
    CpuTimes {
        busy: kernel.saturating_sub(idle).saturating_add(user),
        idle,
    }
}

fn ticks(t: FILETIME) -> u64 {
    (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)
}

fn total() -> Result<CpuTimes, SensorError> {
    let (mut idle, mut kernel, mut user) = Default::default();
    // SAFETY: three valid out pointers.
    if unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } == 0 {
        let error = io::Error::last_os_error();
        return Err(SensorError::Transient(format!("GetSystemTimes: {error}")));
    }
    Ok(times(ticks(idle), ticks(kernel), ticks(user)))
}

fn cores() -> Result<Vec<CpuTimes>, SensorError> {
    let mut info = vec![SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION::default(); MAX_CORES];
    let size = std::mem::size_of_val(info.as_slice()) as u32;
    let mut used = 0;
    // SAFETY: the buffer and its size in bytes match.
    let status = unsafe {
        NtQuerySystemInformation(
            SystemProcessorPerformanceInformation,
            info.as_mut_ptr().cast(),
            size,
            &mut used,
        )
    };
    if status < 0 {
        return Err(SensorError::Transient(format!(
            "NtQuerySystemInformation: status {status:#x}"
        )));
    }
    let count = used as usize / std::mem::size_of::<SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION>();
    let counter = |v: i64| u64::try_from(v).unwrap_or(0);
    Ok(info[..count.min(MAX_CORES)]
        .iter()
        .map(|c| {
            times(
                counter(c.IdleTime),
                counter(c.KernelTime),
                counter(c.UserTime),
            )
        })
        .collect())
}

#[async_trait::async_trait]
impl Sensor for CpuSensor {
    async fn collect(&self, _deadline: tokio::time::Instant) -> Result<Vec<Sample>, SensorError> {
        // Two system calls, as cheap as the /proc reads on Linux.
        let mut stat = BTreeMap::from([(TOTAL.to_owned(), total()?)]);
        if self.per_core {
            for (index, core) in cores()?.into_iter().enumerate() {
                stat.insert(index.to_string(), core);
            }
        }
        Ok(self.usage_round(stat))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_time_includes_idle() {
        assert_eq!(times(60, 100, 20), CpuTimes { busy: 60, idle: 60 });
        assert_eq!(times(100, 50, 0), CpuTimes { busy: 0, idle: 100 });
        let ft = FILETIME {
            dwLowDateTime: 2,
            dwHighDateTime: 1,
        };
        assert_eq!(ticks(ft), (1 << 32) + 2);
    }

    #[tokio::test]
    async fn collects_on_this_host() {
        let cores = cores().unwrap();
        assert!(!cores.is_empty() && cores.len() <= MAX_CORES);
        let sensor = CpuSensor::new(true);
        let deadline = tokio::time::Instant::now();
        assert!(sensor.collect(deadline).await.unwrap().is_empty());
        std::thread::sleep(std::time::Duration::from_millis(100));
        let second = sensor.collect(deadline).await.unwrap();
        assert!(second.iter().any(|s| s.instance == TOTAL));
        assert!(second
            .iter()
            .all(|s| s.sensor == "usage_ratio" && (0.0..=1.0).contains(&s.value)));
    }
}
