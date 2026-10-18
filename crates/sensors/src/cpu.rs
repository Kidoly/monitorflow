//! CPU usage from `/proc/stat` and load averages from `/proc/loadavg`.
//!
//! Metrics (prefixed with the configured name, `cpu` by default):
//! - `usage_ratio`, instance `total` and, with `per_core` (default), one
//!   per core: `0`, `1`, … Busy time over elapsed time since the previous
//!   collection, so the first collection only primes the state.
//! - `load1`, `load5`, `load15`, no instance of their own.
//!
//! A core that appears (hotplug) is primed on its first collection, one
//! that disappears is forgotten. A counter that went backwards (a reset)
//! skips that core for one round and primes it again.

use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::procfs::{self, field};
use crate::{Sample, Sensor, SensorError};

const STAT: &str = "/proc/stat";
const LOADAVG: &str = "/proc/loadavg";
/// `/proc/stat` grows with cores and interrupts; a few hundred KiB on
/// large hosts.
const STAT_CAP: usize = 4 << 20;
const LOADAVG_CAP: usize = 4 << 10;
/// Instance of the all-cores usage.
pub const TOTAL: &str = "total";

/// Jiffies a core spent busy and idle, summed from one `/proc/stat` line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuTimes {
    pub busy: u64,
    pub idle: u64,
}

/// Parses the `cpu` lines of `/proc/stat`, keyed by instance: [`TOTAL`]
/// for the `cpu` line, the core index for `cpuN`.
///
/// Fields are user, nice, system, idle, iowait, irq, softirq, steal,
/// guest, guest_nice; kernels before 2.6.33 have fewer. Idle is idle plus
/// iowait. Guest time is already counted in user and nice, so it is left
/// out.
pub fn parse_stat(text: &str) -> Result<BTreeMap<String, CpuTimes>, SensorError> {
    let mut cores = BTreeMap::new();
    for line in text.lines().filter(|l| l.starts_with("cpu")) {
        let name = line.split_ascii_whitespace().next().unwrap_or_default();
        let instance = match &name[3..] {
            "" => TOTAL.to_owned(),
            index if index.bytes().all(|b| b.is_ascii_digit()) => index.to_owned(),
            _ => return Err(procfs::malformed(STAT, format_args!("odd line `{name}`"))),
        };
        let values: Vec<u64> = (1..=8).map_while(|i| field(line, i)).collect::<Vec<u64>>();
        if values.len() < 4 {
            return Err(procfs::malformed(
                STAT,
                format_args!("`{name}` has fewer than 4 counters"),
            ));
        }
        let at = |i: usize| values.get(i).copied().unwrap_or(0);
        let times = CpuTimes {
            busy: [0, 1, 2, 5, 6, 7]
                .iter()
                .fold(0u64, |s, &i| s.saturating_add(at(i))),
            idle: at(3).saturating_add(at(4)),
        };
        cores.insert(instance, times);
    }
    if !cores.contains_key(TOTAL) {
        return Err(procfs::malformed(STAT, "no `cpu` line"));
    }
    Ok(cores)
}

/// Busy share of the time between `prev` and `cur`, or `None` when
/// nothing elapsed or the counters went backwards (a reset). Single
/// counters may dip slightly (iowait does on some kernels), so busy and
/// idle deltas saturate at zero.
pub fn usage(prev: CpuTimes, cur: CpuTimes) -> Option<f64> {
    let total = |t: CpuTimes| t.busy.saturating_add(t.idle);
    if total(cur) < total(prev) {
        return None;
    }
    let busy = cur.busy.saturating_sub(prev.busy);
    let idle = cur.idle.saturating_sub(prev.idle);
    let elapsed = busy + idle;
    (elapsed > 0).then(|| (busy as f64 / elapsed as f64).clamp(0.0, 1.0))
}

/// The three load averages of `/proc/loadavg`.
pub fn parse_loadavg(text: &str) -> Result<[f64; 3], SensorError> {
    let load = |i| {
        field::<f64>(text, i)
            .filter(|v| v.is_finite() && *v >= 0.0)
            .ok_or_else(|| procfs::malformed(LOADAVG, "expected three load averages"))
    };
    Ok([load(0)?, load(1)?, load(2)?])
}

#[derive(Debug)]
pub struct CpuSensor {
    per_core: bool,
    /// Counters of the previous collection, `None` before the first.
    previous: Mutex<Option<BTreeMap<String, CpuTimes>>>,
}

impl CpuSensor {
    /// `per_core` adds one usage sample per core to the total.
    pub fn new(per_core: bool) -> Self {
        Self {
            per_core,
            previous: Mutex::new(None),
        }
    }

    /// Samples for one collection given the parsed files; keeps `stat` as
    /// the state for the next one.
    pub fn round(&self, stat: BTreeMap<String, CpuTimes>, load: [f64; 3]) -> Vec<Sample> {
        let mut samples = Vec::with_capacity(stat.len() + 3);
        // A panic while holding the lock cannot leave the map half-updated.
        let mut previous = self.previous.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(previous) = previous.as_ref() {
            for (instance, cur) in &stat {
                if !self.per_core && instance != TOTAL {
                    continue;
                }
                let ratio = previous.get(instance).and_then(|prev| usage(*prev, *cur));
                if let Some(ratio) = ratio {
                    samples.push(Sample::metric("usage_ratio", ratio).with_instance(instance));
                }
            }
        }
        *previous = Some(stat);
        for (name, value) in ["load1", "load5", "load15"].into_iter().zip(load) {
            samples.push(Sample::metric(name, value));
        }
        samples
    }
}

#[async_trait::async_trait]
impl Sensor for CpuSensor {
    async fn collect(&self, _deadline: tokio::time::Instant) -> Result<Vec<Sample>, SensorError> {
        let stat = parse_stat(&procfs::read_capped(STAT, STAT_CAP)?)?;
        let load = parse_loadavg(&procfs::read_capped(LOADAVG, LOADAVG_CAP)?)?;
        Ok(self.round(stat, load))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEBIAN_STAT: &str = include_str!("../tests/fixtures/debian12/stat");
    const RHEL_STAT: &str = include_str!("../tests/fixtures/rhel9/stat");

    fn times(busy: u64, idle: u64) -> CpuTimes {
        CpuTimes { busy, idle }
    }

    fn summary(samples: &[Sample]) -> Vec<(String, String, f64)> {
        samples
            .iter()
            .map(|s| (s.sensor.clone(), s.instance.clone(), s.value))
            .collect()
    }

    #[test]
    fn parses_debian_and_rhel_stat() {
        let debian = parse_stat(DEBIAN_STAT).unwrap();
        assert_eq!(
            debian.keys().collect::<Vec<_>>(),
            ["0", "1", "2", "3", "total"]
        );
        // user + nice + system + irq + softirq + steal; idle + iowait.
        assert_eq!(
            debian["total"],
            times(4705 + 356 + 584 + 76, 3699176 + 2394)
        );
        let rhel = parse_stat(RHEL_STAT).unwrap();
        assert_eq!(rhel.len(), 3);
        assert_eq!(
            rhel["1"],
            times(
                912465 + 571 + 461243 + 180528 + 60314 + 10973,
                93362430 + 26308
            )
        );
    }

    #[test]
    fn parses_old_kernels_and_refuses_garbage() {
        let old = parse_stat("cpu 1 2 3 4\ncpu0 1 2 3 4 5\n").unwrap();
        assert_eq!(old["total"], times(6, 4));
        assert_eq!(old["0"], times(6, 9));
        for bad in [
            "",
            "intr 1 2\n",
            "cpu 1 2 3\n",
            "cpu 1 2 3 4\ncpux 1 2 3 4\n",
        ] {
            assert!(parse_stat(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn usage_handles_idle_ticks_and_resets() {
        assert_eq!(usage(times(100, 100), times(150, 250)), Some(0.25));
        assert_eq!(usage(times(100, 100), times(100, 100)), None);
        assert_eq!(usage(times(100, 100), times(10, 10)), None, "reset");
        // iowait dipped: idle went back by 2 while busy grew by 10.
        assert_eq!(usage(times(100, 100), times(110, 98)), Some(1.0));
    }

    #[test]
    fn parses_loadavg() {
        let debian = include_str!("../tests/fixtures/debian12/loadavg");
        assert_eq!(parse_loadavg(debian).unwrap(), [0.08, 0.03, 0.01]);
        let rhel = include_str!("../tests/fixtures/rhel9/loadavg");
        assert_eq!(parse_loadavg(rhel).unwrap(), [1.24, 0.97, 0.88]);
        for bad in ["", "1 2", "1 x 3", "1 NaN 3", "-1 0 0"] {
            assert!(parse_loadavg(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn first_round_primes_then_usage_per_core() {
        let sensor = CpuSensor::new(true);
        let stat = |t: (u64, u64), c0: (u64, u64), c1: (u64, u64)| {
            BTreeMap::from([
                (TOTAL.to_owned(), times(t.0, t.1)),
                ("0".to_owned(), times(c0.0, c0.1)),
                ("1".to_owned(), times(c1.0, c1.1)),
            ])
        };
        let first = sensor.round(stat((0, 0), (0, 0), (0, 0)), [1.0, 2.0, 3.0]);
        assert_eq!(
            summary(&first),
            [
                ("load1".into(), "".into(), 1.0),
                ("load5".into(), "".into(), 2.0),
                ("load15".into(), "".into(), 3.0)
            ]
        );
        let second = sensor.round(stat((60, 140), (50, 50), (10, 90)), [0.0; 3]);
        assert_eq!(
            summary(&second)[..3],
            [
                ("usage_ratio".into(), "0".into(), 0.5),
                ("usage_ratio".into(), "1".into(), 0.1),
                ("usage_ratio".into(), "total".into(), 0.3)
            ]
        );
    }

    #[test]
    fn hotplugged_cores_are_primed_and_forgotten() {
        let sensor = CpuSensor::new(true);
        let one = BTreeMap::from([(TOTAL.to_owned(), times(0, 0)), ("0".into(), times(0, 0))]);
        let two = BTreeMap::from([
            (TOTAL.to_owned(), times(10, 10)),
            ("0".into(), times(5, 5)),
            ("1".into(), times(100, 100)),
        ]);
        sensor.round(one.clone(), [0.0; 3]);
        let added = sensor.round(two, [0.0; 3]);
        let usage = |s: &[Sample]| -> Vec<String> {
            s.iter()
                .filter(|s| s.sensor == "usage_ratio")
                .map(|s| s.instance.clone())
                .collect()
        };
        assert_eq!(usage(&added), ["0", "total"], "core 1 is only primed");
        let removed = sensor.round(
            BTreeMap::from([(TOTAL.to_owned(), times(20, 20)), ("0".into(), times(9, 9))]),
            [0.0; 3],
        );
        assert_eq!(usage(&removed), ["0", "total"]);
    }

    #[test]
    fn per_core_can_be_turned_off() {
        let sensor = CpuSensor::new(false);
        let stat = parse_stat(DEBIAN_STAT).unwrap();
        sensor.round(stat.clone(), [0.0; 3]);
        let mut later = stat;
        for t in later.values_mut() {
            t.busy += 1;
            t.idle += 3;
        }
        let samples = sensor.round(later, [0.0; 3]);
        assert_eq!(
            summary(&samples)[0],
            ("usage_ratio".into(), "total".into(), 0.25)
        );
        assert_eq!(samples.len(), 4);
    }

    #[tokio::test]
    async fn collects_on_this_host() {
        if !std::path::Path::new(STAT).exists() {
            return;
        }
        let sensor = CpuSensor::new(true);
        let deadline = tokio::time::Instant::now();
        let first = sensor.collect(deadline).await.unwrap();
        assert_eq!(first.len(), 3);
        // Let a few jiffies pass so the total usually moves.
        std::thread::sleep(std::time::Duration::from_millis(30));
        let second = sensor.collect(deadline).await.unwrap();
        assert!(second
            .iter()
            .all(|s| (0.0..=1.0).contains(&s.value) || s.sensor.starts_with("load")));
    }
}
