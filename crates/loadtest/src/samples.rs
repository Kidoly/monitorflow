//! Synthetic sensor output, shaped like what the real sensors send
//! (`monitorflow-agent sensors probe`): the built-in `cpu`, `memory`,
//! `disk`, `diskio` and `net`, and the `systemd` and `port` checks the
//! interface adds, with the same metrics, instances and labels, so the
//! server stores and evaluates the same series as for a real fleet. Values
//! drift slowly per host; during an alert storm the total CPU usage of
//! every host goes above 90 %.

use std::f64::consts::TAU;

use monitorflow_common::proto::v1::{Sample, SensorSpec};

/// What a simulated host looks like.
#[derive(Debug, Clone)]
pub struct Host {
    pub cores: u32,
    pub memory_bytes: f64,
    /// Mount point, device, filesystem type, size in bytes.
    pub mounts: Vec<(&'static str, &'static str, &'static str, f64)>,
    /// Spreads the hosts' waves apart.
    pub phase: f64,
}

impl Host {
    /// Host number `n` of the fleet: 2 to 16 cores, 4 to 64 GiB, a root
    /// filesystem and, on every other host, a data volume.
    pub fn nth(n: usize) -> Self {
        const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
        let cores = [2, 4, 4, 8, 8, 16][n % 6];
        let mut mounts = vec![("/", "/dev/sda1", "ext4", 80.0 * GIB)];
        if n.is_multiple_of(2) {
            mounts.push(("/var/lib/data", "/dev/sdb1", "xfs", 500.0 * GIB));
        }
        Self {
            cores,
            memory_bytes: f64::from(cores) * 4.0 * GIB,
            mounts,
            phase: (n as f64 * 0.618_034).fract(),
        }
    }

    /// One collection of the sensor `spec` at `at_ms`. Services are active
    /// and ports open, so they never alert; other kinds report one value.
    pub fn collect(&self, spec: &SensorSpec, at_ms: i64, breach: bool) -> Vec<Sample> {
        // A slow wave per host: one period every 30 minutes.
        let wave = (TAU * (at_ms as f64 / 1_800_000.0 + self.phase)).sin();
        let name = &spec.name;
        let sample = |metric: &str, instance: &str, value: f64| Sample {
            sensor: format!("{name}_{metric}"),
            // The agent gives samples without one the sensor's instance.
            instance: if instance.is_empty() {
                spec.instance.clone()
            } else {
                instance.to_owned()
            },
            timestamp_unix_ms: at_ms,
            value,
            labels: Default::default(),
        };
        match spec.kind.as_str() {
            "cpu" => {
                let total = if breach { 0.97 } else { 0.25 + 0.1 * wave };
                let mut samples: Vec<Sample> = (0..self.cores)
                    .map(|core| {
                        let skew = 0.05 * f64::from(core % 3);
                        sample("usage_ratio", &core.to_string(), (total + skew).min(1.0))
                    })
                    .collect();
                samples.push(sample("usage_ratio", "total", total));
                let load = total * f64::from(self.cores);
                for (metric, factor) in [("load1", 1.0), ("load5", 0.9), ("load15", 0.8)] {
                    samples.push(sample(metric, "", load * factor));
                }
                samples
            }
            "memory" => {
                let ratio = 0.45 + 0.1 * wave;
                let available = self.memory_bytes * ratio;
                vec![
                    sample("total_bytes", "", self.memory_bytes),
                    sample("available_bytes", "", available),
                    sample("used_bytes", "", self.memory_bytes - available),
                    sample("available_ratio", "", ratio),
                    sample("swap_total_bytes", "", 2.0e9),
                    sample("swap_free_bytes", "", 1.9e9),
                ]
            }
            "disk" => self
                .mounts
                .iter()
                .flat_map(|&(mount, device, fstype, size)| {
                    let used = 0.5 + 0.05 * wave;
                    let labelled = |metric: &str, value: f64| {
                        let mut s = sample(metric, mount, value);
                        s.labels.insert("device".into(), device.into());
                        s.labels.insert("fstype".into(), fstype.into());
                        s
                    };
                    [
                        labelled("total_bytes", size),
                        labelled("free_bytes", size * (1.0 - used)),
                        labelled("available_bytes", size * (0.95 - used)),
                        labelled("used_ratio", used),
                        labelled("inodes", 5.0e6),
                        labelled("inodes_free", 4.0e6),
                    ]
                })
                .collect(),
            "diskio" => self
                .mounts
                .iter()
                .enumerate()
                .flat_map(|(n, _)| {
                    let device = ["sda", "sdb"][n.min(1)];
                    let busy = 0.05 + 0.03 * wave;
                    [
                        ("reads_per_second", 40.0 * (1.0 + wave)),
                        ("writes_per_second", 25.0),
                        ("read_bytes_per_second", 1.6e6),
                        ("written_bytes_per_second", 9.0e5),
                        ("busy_ratio", busy),
                        ("wait_seconds", 0.002),
                        ("queue_depth", busy * 2.0),
                    ]
                    .map(|(metric, value)| sample(metric, device, value))
                })
                .collect(),
            "net" => [
                ("receive_bytes_per_second", 2.0e5 * (1.5 + wave)),
                ("transmit_bytes_per_second", 1.0e5 * (1.5 + wave)),
                ("receive_errors_per_second", 0.0),
                ("transmit_errors_per_second", 0.0),
                ("receive_drops_per_second", 0.0),
                ("transmit_drops_per_second", 0.0),
            ]
            .map(|(metric, value)| sample(metric, "eth0", value))
            .to_vec(),
            "systemd" => spec
                .params
                .get("units")
                .map(String::as_str)
                .unwrap_or_default()
                .split(',')
                .filter(|unit| !unit.is_empty())
                .flat_map(|unit| {
                    let unit = if unit.contains('.') {
                        unit.to_owned()
                    } else {
                        format!("{unit}.service")
                    };
                    [
                        sample("loaded", &unit, 1.0),
                        sample("active", &unit, 1.0),
                        sample("failed", &unit, 0.0),
                        sample("restarts", &unit, 0.0),
                        sample("state_age_seconds", &unit, 86_400.0 + at_ms as f64 / 1e3),
                    ]
                })
                .collect(),
            "port" => {
                let host = spec.params.get("host").map_or("127.0.0.1", String::as_str);
                let port = spec.params.get("port").map_or("0", String::as_str);
                let target = format!("{host}:{port}");
                [("up", 1.0), ("connect_seconds", 0.000_2)]
                    .map(|(metric, value)| {
                        let mut s = sample(metric, "", value);
                        s.labels.insert("target".into(), target.clone());
                        s
                    })
                    .to_vec()
            }
            _ => vec![sample("up", "", 1.0)],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(kind: &str, name: &str, params: &[(&str, &str)]) -> SensorSpec {
        SensorSpec {
            kind: kind.into(),
            name: name.into(),
            params: params
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            ..SensorSpec::default()
        }
    }

    fn builtin(kind: &str) -> SensorSpec {
        spec(kind, kind, &[])
    }

    fn names(samples: &[Sample]) -> Vec<(&str, &str)> {
        samples
            .iter()
            .map(|s| (s.sensor.as_str(), s.instance.as_str()))
            .collect()
    }

    #[test]
    fn cpu_has_one_ratio_per_core_a_total_and_the_load() {
        let host = Host::nth(0);
        let samples = host.collect(&builtin("cpu"), 1_000, false);
        assert_eq!(samples.len(), host.cores as usize + 4);
        let names = names(&samples);
        assert!(names.contains(&("cpu_usage_ratio", "0")));
        assert!(names.contains(&("cpu_usage_ratio", "total")));
        assert!(names.contains(&("cpu_load15", "")));
        assert!(samples
            .iter()
            .filter(|s| s.sensor == "cpu_usage_ratio")
            .all(|s| (0.0..=1.0).contains(&s.value)));
    }

    #[test]
    fn a_breach_puts_every_host_above_the_cpu_threshold() {
        for n in 0..20 {
            let samples = Host::nth(n).collect(&builtin("cpu"), n as i64 * 77_000, true);
            let total = samples.iter().find(|s| s.instance == "total").unwrap();
            assert!(total.value > 0.9, "{n}: {}", total.value);
            let samples = Host::nth(n).collect(&builtin("cpu"), n as i64 * 77_000, false);
            let total = samples.iter().find(|s| s.instance == "total").unwrap();
            assert!(total.value < 0.9, "{n}: {}", total.value);
        }
    }

    #[test]
    fn disk_reports_each_mount_with_its_labels() {
        let samples = Host::nth(0).collect(&builtin("disk"), 0, false);
        assert_eq!(samples.len(), 12);
        let data = samples
            .iter()
            .find(|s| s.instance == "/var/lib/data" && s.sensor == "disk_used_ratio")
            .unwrap();
        assert_eq!(data.labels["fstype"], "xfs");
        assert_eq!(Host::nth(1).collect(&builtin("disk"), 0, false).len(), 6);
    }

    #[test]
    fn memory_stays_away_from_its_thresholds() {
        let samples = Host::nth(3).collect(&builtin("memory"), 123_456, false);
        let ratio = samples
            .iter()
            .find(|s| s.sensor == "memory_available_ratio")
            .unwrap();
        assert!(ratio.value > 0.3);
    }

    #[test]
    fn other_kinds_send_one_value_named_after_the_sensor() {
        let samples = Host::nth(0).collect(&spec("script", "backup", &[]), 5, false);
        assert_eq!(names(&samples), [("backup_up", "")]);
        assert_eq!(samples[0].timestamp_unix_ms, 5);
    }

    #[test]
    fn diskio_and_net_report_per_device_and_interface() {
        let samples = Host::nth(0).collect(&builtin("diskio"), 0, false);
        assert_eq!(samples.len(), 14);
        assert!(names(&samples).contains(&("diskio_busy_ratio", "sdb")));
        assert_eq!(Host::nth(1).collect(&builtin("diskio"), 0, false).len(), 7);
        let samples = Host::nth(0).collect(&builtin("net"), 0, false);
        assert_eq!(samples.len(), 6);
        assert!(names(&samples).contains(&("net_receive_bytes_per_second", "eth0")));
    }

    #[test]
    fn a_service_is_active_with_the_unit_as_instance() {
        let check = spec("systemd", "service_app_01", &[("units", "app-01")]);
        let samples = Host::nth(0).collect(&check, 0, false);
        assert_eq!(samples.len(), 5);
        let active = samples
            .iter()
            .find(|s| s.sensor == "service_app_01_active")
            .unwrap();
        assert_eq!(active.instance, "app-01.service");
        assert_eq!(active.value, 1.0);
    }

    #[test]
    fn a_port_is_up_with_its_target_and_the_sensor_instance() {
        let mut check = spec("port", "port_10001", &[("port", "10001")]);
        check.instance = "web".into();
        let samples = Host::nth(0).collect(&check, 0, false);
        assert_eq!(
            names(&samples),
            [
                ("port_10001_up", "web"),
                ("port_10001_connect_seconds", "web")
            ]
        );
        assert_eq!(samples[0].labels["target"], "127.0.0.1:10001");
        assert_eq!(samples[0].value, 1.0);
    }
}
