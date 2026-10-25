//! Sensor trait and implementations. This crate has no transport code (no
//! gRPC, TLS or HTTP) so it can be reused by future agents on other
//! platforms; the [`port`] sensor's TCP connect is a measurement (ADR-0021).
//!
//! A [`Sensor`] collects [`Sample`]s; a [`Scheduler`] runs each configured
//! sensor on its own interval and puts the samples on a bounded channel.
//! Names and labels are checked by [`validate`] because they become
//! VictoriaMetrics label names and values on the server. See ADR-0012.
//!
//! The built-in sensors ([`cpu`], [`memory`], [`disk`], [`systemd`],
//! [`port`], [`script`], [`log`]) are built by kind with [`registry::build`].
//! One sensor emits several metrics by setting [`Sample::sensor`] to a
//! metric suffix, which the scheduler prefixes with the configured name
//! ([`metric_name`]). See ADR-0015.

use std::collections::BTreeMap;
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// Linux implementations; on Windows `registry` refuses these kinds until
// they have their own (ADR-0047).
#[cfg(unix)]
pub mod cpu;
#[cfg(unix)]
pub mod disk;
#[cfg(unix)]
pub mod log;
#[cfg(unix)]
pub mod memory;
pub mod port;
#[cfg(unix)]
mod procfs;
pub mod registry;
pub mod scheduler;
#[cfg(unix)]
pub mod script;
#[cfg(unix)]
pub mod systemd;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod validate;

pub use registry::BuildError;
pub use scheduler::{Scheduler, SchedulerHandle, SchedulerOptions, Stats};
pub use validate::ValidationError;

/// One reading produced by a sensor. Mirrors `monitorflow.v1.Sample`.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    /// Full sensor name, like `cpu_usage_ratio`. A sensor leaves it empty
    /// or sets a metric suffix (`usage_ratio`); the scheduler then sets it
    /// to [`metric_name`] of [`SensorConfig::name`] and that suffix.
    pub sensor: String,
    /// What the value is about: a mount point, a unit, a core index. Empty
    /// values are replaced by [`SensorConfig::instance`].
    pub instance: String,
    pub timestamp_unix_ms: i64,
    pub value: f64,
    pub labels: BTreeMap<String, String>,
}

impl Sample {
    /// A sample taken now, with no instance and no labels.
    pub fn now(value: f64) -> Self {
        Self {
            sensor: String::new(),
            instance: String::new(),
            timestamp_unix_ms: unix_ms(SystemTime::now()),
            value,
            labels: BTreeMap::new(),
        }
    }

    /// A sample of the metric `metric` taken now, see [`Sample::sensor`].
    pub fn metric(metric: impl Into<String>, value: f64) -> Self {
        Self {
            sensor: metric.into(),
            ..Self::now(value)
        }
    }

    pub fn with_instance(mut self, instance: impl Into<String>) -> Self {
        self.instance = instance.into();
        self
    }

    pub fn with_label(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.labels.insert(name.into(), value.into());
        self
    }
}

/// Prometheus text format without the timestamp, for CLI output:
/// `cpu_usage_ratio{instance="total"} 0.25`.
impl fmt::Display for Sample {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.sensor)?;
        let instance = (!self.instance.is_empty()).then_some(("instance", &self.instance));
        let labels = instance
            .into_iter()
            .chain(self.labels.iter().map(|(k, v)| (k.as_str(), v)));
        for (i, (name, value)) in labels.enumerate() {
            let sep = if i == 0 { "{" } else { "," };
            write!(f, "{sep}{name}=\"{}\"", value.escape_default())?;
        }
        if !self.instance.is_empty() || !self.labels.is_empty() {
            f.write_str("}")?;
        }
        write!(f, " {}", self.value)
    }
}

/// The full name of a metric: the configured sensor name, then `_` and
/// the suffix the sensor set, if any. `("cpu", "load1")` gives `cpu_load1`.
pub fn metric_name(sensor: &str, metric: &str) -> String {
    if metric.is_empty() {
        sensor.to_owned()
    } else {
        format!("{sensor}_{metric}")
    }
}

fn unix_ms(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Why a collection failed. The scheduler retries transient failures on
/// the next tick and stops a sensor after a permanent one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SensorError {
    /// May work next time: a file busy, a command that timed out.
    #[error("{0}")]
    Transient(String),
    /// Will not work until the configuration or host changes: an unknown
    /// parameter, a missing kernel interface.
    #[error("{0}")]
    Permanent(String),
}

impl SensorError {
    pub fn is_permanent(&self) -> bool {
        matches!(self, Self::Permanent(_))
    }
}

/// A source of samples, polled on an interval by the [`Scheduler`].
///
/// Implementations must not block the runtime: file reads under `/proc`
/// are fine, anything that can hang goes through `spawn_blocking` or an
/// async API. The scheduler cancels `collect` at `deadline` by dropping
/// the future, so work that outlives it (a child process, a blocking task)
/// must be bounded by `deadline` too, or killed on drop.
#[async_trait::async_trait]
pub trait Sensor: Send + Sync + 'static {
    /// Collect one round of samples.
    async fn collect(&self, deadline: tokio::time::Instant) -> Result<Vec<Sample>, SensorError>;
}

/// Shortest allowed collection interval.
pub const MIN_INTERVAL: Duration = Duration::from_secs(1);
/// Longest allowed collection interval.
pub const MAX_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// Collection timeout when the configuration has none, capped by the
/// interval.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// One configured sensor. `(name, instance)` is unique within a scheduler,
/// so two port checks are `port`/`22` and `port`/`443`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SensorConfig {
    /// Sensor name, like `cpu` or `disk`; see [`validate::sensor_name`].
    pub name: String,
    /// Default instance for samples that leave theirs empty; may be empty.
    pub instance: String,
    pub interval: Duration,
    /// Per-collection timeout; [`Self::timeout`] applies the default.
    pub timeout: Option<Duration>,
    /// Sensor-specific parameters, interpreted by the implementation.
    pub params: BTreeMap<String, String>,
}

impl SensorConfig {
    pub fn new(name: impl Into<String>, interval: Duration) -> Self {
        Self {
            name: name.into(),
            instance: String::new(),
            interval,
            timeout: None,
            params: BTreeMap::new(),
        }
    }

    /// The configured timeout, or [`DEFAULT_TIMEOUT`], never longer than
    /// the interval.
    pub fn timeout(&self) -> Duration {
        self.timeout.unwrap_or(DEFAULT_TIMEOUT).min(self.interval)
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        validate::sensor_name(&self.name)?;
        validate::instance(&self.instance)?;
        if !(MIN_INTERVAL..=MAX_INTERVAL).contains(&self.interval) {
            return Err(ValidationError::Interval(self.interval));
        }
        match self.timeout {
            Some(t) if t.is_zero() || t > self.interval => Err(ValidationError::Timeout(t)),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_builders() {
        let sample = Sample::now(1.5).with_instance("/").with_label("fs", "ext4");
        assert_eq!(sample.instance, "/");
        assert_eq!(sample.labels["fs"], "ext4");
        assert!(sample.timestamp_unix_ms > 1_700_000_000_000);
    }

    #[test]
    fn metric_names_and_display() {
        assert_eq!(metric_name("cpu", ""), "cpu");
        assert_eq!(metric_name("cpu", "load1"), "cpu_load1");
        let mut sample = Sample::metric("usage_ratio", 0.25);
        assert_eq!(sample.sensor, "usage_ratio");
        assert_eq!(sample.to_string(), "usage_ratio 0.25");
        sample = sample.with_instance("/a\"b").with_label("fs", "ext4");
        assert_eq!(
            sample.to_string(),
            r#"usage_ratio{instance="/a\"b",fs="ext4"} 0.25"#
        );
        assert_eq!(
            Sample::now(1.0).with_label("a", "b").to_string(),
            r#"{a="b"} 1"#
        );
    }

    #[test]
    fn config_timeout_defaults_and_bounds() {
        let mut config = SensorConfig::new("cpu", Duration::from_secs(30));
        assert_eq!(config.timeout(), DEFAULT_TIMEOUT);
        config.interval = Duration::from_secs(5);
        assert_eq!(config.timeout(), Duration::from_secs(5));
        assert!(config.validate().is_ok());

        config.timeout = Some(Duration::from_secs(6));
        assert_eq!(
            config.validate(),
            Err(ValidationError::Timeout(Duration::from_secs(6)))
        );
        config.timeout = Some(Duration::ZERO);
        assert!(config.validate().is_err());
        config.timeout = None;
        config.interval = Duration::from_millis(500);
        assert!(matches!(
            config.validate(),
            Err(ValidationError::Interval(_))
        ));
        config.interval = MAX_INTERVAL + Duration::from_secs(1);
        assert!(config.validate().is_err());
        let bad = SensorConfig::new("CPU", Duration::from_secs(5));
        assert!(matches!(
            bad.validate(),
            Err(ValidationError::SensorName(_))
        ));
    }

    #[test]
    fn permanent_errors_are_flagged() {
        assert!(SensorError::Permanent("x".into()).is_permanent());
        assert!(!SensorError::Transient("x".into()).is_permanent());
    }
}
