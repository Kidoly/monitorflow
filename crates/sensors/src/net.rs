//! Traffic of the machine's network interfaces (ADR-0149), from
//! `/proc/net/dev` on Linux and the IP helper's interface table on
//! Windows.
//!
//! Metrics (prefixed with the configured name, `net` by default), with the
//! interface as instance (`eth0`, `enp3s0`, `Ethernet`):
//! `receive_bytes_per_second`, `transmit_bytes_per_second`,
//! `receive_errors_per_second`, `transmit_errors_per_second`,
//! `receive_drops_per_second`, `transmit_drops_per_second`.
//!
//! All are averages since the previous collection, so the first one only
//! primes the state; counters that go back give no rate for that
//! collection. Loopbacks, containers' veth pairs and interfaces that never
//! carried a byte are left out, and at most [`MAX_INTERFACES`] are
//! reported.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

#[cfg(unix)]
use crate::procfs;
#[cfg(unix)]
use crate::Sensor;
use crate::{Sample, SensorError};

/// Most interfaces reported per collection (6 series each).
pub const MAX_INTERFACES: usize = 64;
/// Name prefixes never reported: loopback and the host ends of
/// containers' pairs, which come and go with each container.
pub const SKIPPED_PREFIXES: &[&str] = &["lo", "veth"];
#[cfg(unix)]
const NET_DEV: &str = NET_DEV_NAME;

/// The counters of one interface: received then transmitted bytes,
/// errors and drops.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    pub receive_bytes: u64,
    pub receive_errors: u64,
    pub receive_drops: u64,
    pub transmit_bytes: u64,
    pub transmit_errors: u64,
    pub transmit_drops: u64,
}

/// Parses `/proc/net/dev`: two header lines, then `name: ` and sixteen
/// counters, the receive ones (bytes, packets, errs, drop, ...) first.
pub fn parse_net_dev(text: &str) -> Result<Vec<(String, Counters)>, SensorError> {
    let mut interfaces = Vec::new();
    for line in text.lines().skip(2).filter(|l| !l.trim().is_empty()) {
        let Some((name, rest)) = line.split_once(':') else {
            return Err(SensorError::Permanent(format!("{NET_DEV_NAME}: odd line")));
        };
        let values: Vec<u64> = rest
            .split_ascii_whitespace()
            .filter_map(|v| v.parse().ok())
            .collect();
        if values.len() < 16 {
            return Err(SensorError::Permanent(format!(
                "{NET_DEV_NAME}: short line"
            )));
        }
        interfaces.push((
            name.trim().to_owned(),
            Counters {
                receive_bytes: values[0],
                receive_errors: values[2],
                receive_drops: values[3],
                transmit_bytes: values[8],
                transmit_errors: values[10],
                transmit_drops: values[11],
            },
        ));
    }
    Ok(interfaces)
}

const NET_DEV_NAME: &str = "/proc/net/dev";

/// The interfaces to report: not skipped, that carried traffic, at most
/// [`MAX_INTERFACES`].
pub fn select(interfaces: Vec<(String, Counters)>) -> Vec<(String, Counters)> {
    interfaces
        .into_iter()
        .filter(|(name, _)| !SKIPPED_PREFIXES.iter().any(|p| name.starts_with(p)))
        .filter(|(_, c)| c.receive_bytes > 0 || c.transmit_bytes > 0)
        .take(MAX_INTERFACES)
        .collect()
}

#[derive(Debug, Default)]
pub struct NetSensor {
    previous: Mutex<Option<(Duration, BTreeMap<String, Counters>)>>,
}

impl NetSensor {
    /// The samples of one reading of `interfaces` at `at`, a monotonic
    /// time; keeps them for the next one.
    pub fn round(&self, at: Duration, interfaces: Vec<(String, Counters)>) -> Vec<Sample> {
        let mut samples = Vec::new();
        let mut previous = self.previous.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((then, before)) = previous.as_ref() {
            let seconds = at.saturating_sub(*then).as_secs_f64();
            for (name, now) in &interfaces {
                let (Some(was), true) = (before.get(name), seconds > 0.0) else {
                    continue;
                };
                let pairs = [
                    (
                        "receive_bytes_per_second",
                        was.receive_bytes,
                        now.receive_bytes,
                    ),
                    (
                        "transmit_bytes_per_second",
                        was.transmit_bytes,
                        now.transmit_bytes,
                    ),
                    (
                        "receive_errors_per_second",
                        was.receive_errors,
                        now.receive_errors,
                    ),
                    (
                        "transmit_errors_per_second",
                        was.transmit_errors,
                        now.transmit_errors,
                    ),
                    (
                        "receive_drops_per_second",
                        was.receive_drops,
                        now.receive_drops,
                    ),
                    (
                        "transmit_drops_per_second",
                        was.transmit_drops,
                        now.transmit_drops,
                    ),
                ];
                // 64-bit counters: one that goes back was reset.
                let rates: Option<Vec<(&str, u64)>> = pairs
                    .iter()
                    .map(|(m, a, b)| b.checked_sub(*a).map(|d| (*m, d)))
                    .collect();
                // A counter went back: the interface was reset.
                let Some(rates) = rates else { continue };
                samples.extend(
                    rates
                        .into_iter()
                        .map(|(m, d)| Sample::metric(m, d as f64 / seconds).with_instance(name)),
                );
            }
        }
        *previous = Some((at, interfaces.into_iter().collect()));
        samples
    }
}

fn monotonic() -> Duration {
    static ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    ORIGIN.get_or_init(std::time::Instant::now).elapsed()
}

#[cfg(unix)]
#[async_trait::async_trait]
impl Sensor for NetSensor {
    async fn collect(&self, _deadline: tokio::time::Instant) -> Result<Vec<Sample>, SensorError> {
        // Generated in memory by the kernel, cheap to read here.
        let text = procfs::read_capped(NET_DEV, 1 << 20)?;
        Ok(self.round(monotonic(), select(parse_net_dev(&text)?)))
    }
}

#[cfg(windows)]
mod windows;

#[cfg(test)]
mod tests {
    use super::*;

    const NET_DEV_TEXT: &str = "Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 1000      10    0    0    0     0          0         0     1000      10    0    0    0     0       0          0
  eth0: 5000000   4000  3    1    0     0          0         0   2000000    3000  0    2    0     0       0          0
vethab12: 900     9     0    0    0     0          0         0     900       9     0    0    0     0       0          0
 wlan0: 0         0     0    0    0     0          0         0     0         0     0    0    0     0       0          0
";

    #[test]
    fn proc_net_dev_is_read_and_selected() {
        let all = parse_net_dev(NET_DEV_TEXT).unwrap();
        assert_eq!(all.len(), 4);
        let eth0 = &all[1];
        assert_eq!(eth0.0, "eth0");
        assert_eq!(
            eth0.1,
            Counters {
                receive_bytes: 5_000_000,
                receive_errors: 3,
                receive_drops: 1,
                transmit_bytes: 2_000_000,
                transmit_errors: 0,
                transmit_drops: 2
            }
        );
        let names: Vec<_> = select(all).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["eth0"], "no loopback, veth or idle interface");
        assert!(parse_net_dev("a\nb\neth0: 1 2 3\n").is_err());
    }

    #[test]
    fn rates_come_from_two_readings() {
        let sensor = NetSensor::default();
        let at = |c: u64| Counters {
            receive_bytes: c,
            transmit_bytes: c / 2,
            ..Counters::default()
        };
        assert!(sensor
            .round(Duration::from_secs(10), vec![("eth0".into(), at(1_000))])
            .is_empty());
        let samples = sensor.round(Duration::from_secs(20), vec![("eth0".into(), at(101_000))]);
        let value = |m: &str| samples.iter().find(|s| s.sensor == m).unwrap().value;
        assert_eq!(value("receive_bytes_per_second"), 10_000.0);
        assert_eq!(value("transmit_bytes_per_second"), 5_000.0);
        assert_eq!(value("receive_errors_per_second"), 0.0);
        assert!(samples.iter().all(|s| s.instance == "eth0"));
        for sample in &samples {
            crate::validate::sample(sample).unwrap();
        }
        // A reset counter gives nothing for that reading.
        assert!(sensor
            .round(Duration::from_secs(30), vec![("eth0".into(), at(5))])
            .is_empty());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn this_machine_reports_its_interfaces() {
        let sensor = NetSensor::default();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        sensor.collect(deadline).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let samples = sensor.collect(deadline).await.unwrap();
        assert!(samples.iter().all(|s| !s.instance.starts_with("lo")));
    }
}
