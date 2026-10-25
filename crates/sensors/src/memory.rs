//! Memory and swap from `/proc/meminfo`.
//!
//! Metrics (prefixed with the configured name, `memory` by default):
//! `total_bytes`, `available_bytes`, `used_bytes` (total minus available,
//! like procps `free` 4), `available_ratio`, `swap_total_bytes` and
//! `swap_free_bytes`. No parameters.
//!
//! On Windows the values come from `GlobalMemoryStatusEx`, see
//! [`windows`](self) and ADR-0048.

use crate::procfs::{self, field};
#[cfg(any(unix, test))]
use crate::Sensor;
use crate::{Sample, SensorError};

const MEMINFO: &str = "/proc/meminfo";
/// About 1.5 KiB today; generous room for new fields.
#[cfg(unix)]
const MEMINFO_CAP: usize = 64 << 10;

/// The `/proc/meminfo` fields the sensor reports, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemInfo {
    pub total: u64,
    pub available: u64,
    pub swap_total: u64,
    pub swap_free: u64,
}

/// Parses `/proc/meminfo`. Values are in KiB (`kB`); `MemAvailable`
/// exists since Linux 3.14.
pub fn parse_meminfo(text: &str) -> Result<MemInfo, SensorError> {
    let get = |key: &str| -> Result<u64, SensorError> {
        let line = text
            .lines()
            .find(|l| l.split(':').next() == Some(key))
            .ok_or_else(|| procfs::malformed(MEMINFO, format_args!("no {key}")))?;
        let kib: u64 = field(line, 1)
            .filter(|_| field::<String>(line, 2).as_deref() == Some("kB"))
            .ok_or_else(|| procfs::malformed(MEMINFO, format_args!("odd {key} line")))?;
        kib.checked_mul(1024)
            .ok_or_else(|| procfs::malformed(MEMINFO, format_args!("{key} overflows")))
    };
    Ok(MemInfo {
        total: get("MemTotal")?,
        available: get("MemAvailable")?,
        swap_total: get("SwapTotal")?,
        swap_free: get("SwapFree")?,
    })
}

impl MemInfo {
    pub fn samples(&self) -> Vec<Sample> {
        // Container views of meminfo can report more available than total.
        let available = self.available.min(self.total);
        let mut samples = vec![
            Sample::metric("total_bytes", self.total as f64),
            Sample::metric("available_bytes", available as f64),
            Sample::metric("used_bytes", (self.total - available) as f64),
            Sample::metric("swap_total_bytes", self.swap_total as f64),
            Sample::metric("swap_free_bytes", self.swap_free as f64),
        ];
        if self.total > 0 {
            let ratio = available as f64 / self.total as f64;
            samples.insert(3, Sample::metric("available_ratio", ratio));
        }
        samples
    }
}

#[derive(Debug, Default)]
pub struct MemorySensor;

#[cfg(unix)]
#[async_trait::async_trait]
impl Sensor for MemorySensor {
    async fn collect(&self, _deadline: tokio::time::Instant) -> Result<Vec<Sample>, SensorError> {
        let info = parse_meminfo(&procfs::read_capped(MEMINFO, MEMINFO_CAP)?)?;
        Ok(info.samples())
    }
}

#[cfg(windows)]
mod windows;

#[cfg(test)]
mod tests {
    use super::*;

    const KIB: u64 = 1024;

    #[test]
    fn parses_debian_and_rhel_meminfo() {
        let debian = parse_meminfo(include_str!("../tests/fixtures/debian12/meminfo")).unwrap();
        assert_eq!(
            debian,
            MemInfo {
                total: 4005096 * KIB,
                available: 3529208 * KIB,
                swap_total: 998396 * KIB,
                swap_free: 998396 * KIB,
            }
        );
        let rhel = parse_meminfo(include_str!("../tests/fixtures/rhel9/meminfo")).unwrap();
        assert_eq!(rhel.available, 11268412 * KIB);
        assert_eq!(rhel.swap_free, 8156924 * KIB);
    }

    #[test]
    fn refuses_missing_or_odd_fields() {
        let ok = "MemTotal: 4 kB\nMemAvailable: 2 kB\nSwapTotal: 0 kB\nSwapFree: 0 kB\n";
        assert!(parse_meminfo(ok).is_ok());
        let old_kernel = ok.replace("MemAvailable", "MemFree");
        let no_unit = ok.replace("4 kB", "4");
        let huge = ok.replace("4 kB", &format!("{} kB", u64::MAX));
        // A prefix of the key is not the key.
        let prefix = ok.replace("SwapTotal", "SwapTotalX");
        for bad in [old_kernel, no_unit, huge, prefix, String::new()] {
            assert!(parse_meminfo(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn samples_are_bytes_and_a_ratio() {
        let info = MemInfo {
            total: 4000,
            available: 1000,
            swap_total: 50,
            swap_free: 20,
        };
        let got: Vec<(String, f64)> = info
            .samples()
            .into_iter()
            .map(|s| (s.sensor, s.value))
            .collect();
        let want = [
            ("total_bytes", 4000.0),
            ("available_bytes", 1000.0),
            ("used_bytes", 3000.0),
            ("available_ratio", 0.25),
            ("swap_total_bytes", 50.0),
            ("swap_free_bytes", 20.0),
        ];
        assert_eq!(got, want.map(|(n, v)| (n.to_owned(), v)));
    }

    #[test]
    fn odd_container_values_stay_consistent() {
        let info = MemInfo {
            total: 0,
            available: 10,
            swap_total: 0,
            swap_free: 0,
        };
        let samples = info.samples();
        assert!(samples.iter().all(|s| s.sensor != "available_ratio"));
        assert!(samples.iter().all(|s| s.value == 0.0));
    }

    #[tokio::test]
    async fn collects_on_this_host() {
        if !std::path::Path::new(MEMINFO).exists() {
            return;
        }
        let samples = MemorySensor
            .collect(tokio::time::Instant::now())
            .await
            .unwrap();
        assert_eq!(samples.len(), 6);
        assert!(samples[0].value > 0.0);
    }
}
