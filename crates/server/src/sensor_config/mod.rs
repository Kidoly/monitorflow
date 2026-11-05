//! Sensor configuration pushed to agents (ADR-0016).
//!
//! The effective configuration of a server is three layers, keyed by
//! `(name, instance)`: [`builtin_defaults`] at its site's check interval
//! (ADR-0097), then its site's rows, then its own rows. A later layer
//! replaces a sensor of an earlier one and a disabled row removes it. It is sent after every accepted agent `Hello`
//! and again by the [`watcher`] when the server's revision changes.

mod store;
pub mod watcher;

use std::collections::BTreeMap;

use monitorflow_common::proto::v1 as proto;

pub use store::SensorConfigStore;
pub use watcher::{SensorConfigs, WatcherSettings};

/// Interval of the built-in sensors, unless their site sets another.
pub const DEFAULT_INTERVAL_MS: u64 = 60_000;
/// Shortest interval the API accepts (ADR-0097). The agent accepts down
/// to 1 s, so rows written before keep running.
pub const MIN_CHECK_INTERVAL_MS: u64 = 10_000;
/// Longest interval: the agent's own limit, 24 h.
pub const MAX_CHECK_INTERVAL_MS: u64 = 86_400_000;

/// One configured sensor, as stored and as sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub kind: String,
    pub name: String,
    pub instance: String,
    pub interval_ms: u64,
    /// `None`: the agent's default timeout.
    pub timeout_ms: Option<u64>,
    pub params: BTreeMap<String, String>,
}

impl Spec {
    /// A sensor named after its kind, without instance or parameters.
    pub fn new(kind: &str, interval_ms: u64) -> Self {
        Self {
            kind: kind.to_owned(),
            name: kind.to_owned(),
            instance: String::new(),
            interval_ms,
            timeout_ms: None,
            params: BTreeMap::new(),
        }
    }

    fn key(&self) -> (String, String) {
        (self.name.clone(), self.instance.clone())
    }

    /// The timeout sent to the agent: the row's, or one that scales with
    /// the interval (ADR-0097). Sent explicitly so that agents released
    /// before the scaling, which default to 10 s, get it too.
    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms.unwrap_or_else(|| {
            let interval = std::time::Duration::from_millis(self.interval_ms);
            let timeout = monitorflow_sensors::default_timeout(interval);
            u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX)
        })
    }

    fn to_proto(&self) -> proto::SensorSpec {
        proto::SensorSpec {
            kind: self.kind.clone(),
            name: self.name.clone(),
            instance: self.instance.clone(),
            interval_ms: self.interval_ms,
            timeout_ms: self.timeout_ms(),
            params: self.params.clone().into_iter().collect(),
        }
    }
}

/// What every server runs unless its site or itself overrides it: cpu,
/// memory, disk and diskio every `interval_ms`, the site's check interval
/// or [`DEFAULT_INTERVAL_MS`].
pub fn builtin_defaults(interval_ms: u64) -> Vec<Spec> {
    ["cpu", "memory", "disk", "diskio"]
        .into_iter()
        .map(|kind| Spec::new(kind, interval_ms))
        .collect()
}

/// The interval of a site's built-in checks: its own, or the built-in one.
pub fn site_interval(check_interval_ms: Option<i64>) -> u64 {
    check_interval_ms
        .and_then(|ms| u64::try_from(ms).ok())
        .filter(|ms| *ms > 0)
        .unwrap_or(DEFAULT_INTERVAL_MS)
}

/// Where a stored row comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    Site,
    Server,
}

/// A stored row of `sensor_configs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub layer: Layer,
    pub spec: Spec,
    pub enabled: bool,
}

/// The built-in defaults every `builtin_interval_ms` overlaid with `rows`,
/// site rows before server rows, sorted by `(name, instance)`.
pub fn resolve(mut rows: Vec<Row>, builtin_interval_ms: u64) -> Vec<Spec> {
    let mut sensors: BTreeMap<(String, String), Spec> = builtin_defaults(builtin_interval_ms)
        .into_iter()
        .map(|spec| (spec.key(), spec))
        .collect();
    rows.sort_by_key(|row| row.layer);
    for row in rows {
        if row.enabled {
            sensors.insert(row.spec.key(), row.spec);
        } else {
            sensors.remove(&row.spec.key());
        }
    }
    sensors.into_values().collect()
}

/// The effective configuration of one server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub revision: u64,
    pub sensors: Vec<Spec>,
}

impl Resolved {
    pub fn to_proto(&self) -> proto::SensorConfig {
        proto::SensorConfig {
            revision: self.revision,
            sensors: self.sensors.iter().map(Spec::to_proto).collect(),
        }
    }
}

/// A JSON object of scalars as string parameters: strings as they are,
/// numbers and booleans in their JSON form. The schema refuses nested
/// values; any that slip through are passed as JSON text.
pub(crate) fn params(value: serde_json::Value) -> BTreeMap<String, String> {
    let serde_json::Value::Object(map) = value else {
        return BTreeMap::new();
    };
    map.into_iter()
        .map(|(key, value)| match value {
            serde_json::Value::String(s) => (key, s),
            other => (key, other.to_string()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(layer: Layer, name: &str, instance: &str, interval_ms: u64, enabled: bool) -> Row {
        Row {
            layer,
            spec: Spec {
                instance: instance.into(),
                ..Spec::new(name, interval_ms)
            },
            enabled,
        }
    }

    fn keys(specs: &[Spec]) -> Vec<(&str, &str, u64)> {
        specs
            .iter()
            .map(|s| (s.name.as_str(), s.instance.as_str(), s.interval_ms))
            .collect()
    }

    #[test]
    fn defaults_apply_without_rows() {
        let resolved = resolve(Vec::new(), DEFAULT_INTERVAL_MS);
        assert_eq!(
            keys(&resolved),
            [
                ("cpu", "", 60_000),
                ("disk", "", 60_000),
                ("diskio", "", 60_000),
                ("memory", "", 60_000)
            ]
        );
    }

    #[test]
    fn server_rows_win_over_site_rows_which_win_over_defaults() {
        let resolved = resolve(
            vec![
                // Given in the "wrong" order: the layer decides, not the order.
                row(Layer::Server, "cpu", "", 5_000, true),
                row(Layer::Site, "cpu", "", 30_000, true),
                row(Layer::Site, "port", "22", 10_000, true),
                row(Layer::Site, "port", "443", 10_000, true),
                row(Layer::Server, "port", "443", 10_000, false),
                row(Layer::Site, "memory", "", 60_000, false),
                row(Layer::Server, "memory", "", 20_000, true),
                row(Layer::Site, "disk", "", 60_000, false),
                row(Layer::Site, "diskio", "", 60_000, false),
                row(Layer::Server, "service", "nginx", 15_000, true),
            ],
            DEFAULT_INTERVAL_MS,
        );
        assert_eq!(
            keys(&resolved),
            [
                ("cpu", "", 5_000),
                ("memory", "", 20_000),
                ("port", "22", 10_000),
                ("service", "nginx", 15_000),
            ]
        );
    }

    #[test]
    fn the_site_interval_applies_to_builtin_checks_only() {
        let resolved = resolve(
            vec![
                row(Layer::Site, "port", "22", 30_000, true),
                row(Layer::Server, "disk", "", 10_000, true),
            ],
            site_interval(Some(3_600_000)),
        );
        assert_eq!(
            keys(&resolved),
            [
                ("cpu", "", 3_600_000),
                ("disk", "", 10_000),
                ("diskio", "", 3_600_000),
                ("memory", "", 3_600_000),
                ("port", "22", 30_000),
            ]
        );
        assert_eq!(site_interval(None), DEFAULT_INTERVAL_MS);
        assert_eq!(site_interval(Some(0)), DEFAULT_INTERVAL_MS);
        assert_eq!(site_interval(Some(-5)), DEFAULT_INTERVAL_MS);
    }

    #[test]
    fn params_are_strings() {
        let value = serde_json::json!({"port": 22, "host": "db", "tls": true, "odd": [1]});
        let params = params(value);
        assert_eq!(params["port"], "22");
        assert_eq!(params["host"], "db");
        assert_eq!(params["tls"], "true");
        assert_eq!(params["odd"], "[1]");
        assert!(super::params(serde_json::json!([1])).is_empty());
    }

    #[test]
    fn proto_sends_a_timeout_that_scales_with_the_interval() {
        let mut spec = Spec::new("port", 10_000);
        spec.params.insert("port".into(), "22".into());
        let resolved = Resolved {
            revision: 7,
            sensors: vec![
                spec.clone(),
                Spec {
                    timeout_ms: Some(2_000),
                    ..spec
                },
            ],
        };
        let config = resolved.to_proto();
        assert_eq!(config.revision, 7);
        assert_eq!(config.sensors[0].timeout_ms, 10_000);
        assert_eq!(config.sensors[1].timeout_ms, 2_000);
        let hourly = Spec::new("cpu", 3_600_000);
        assert_eq!(hourly.timeout_ms(), 60_000);
        assert_eq!(Spec::new("cpu", 300_000).timeout_ms(), 50_000);
        assert_eq!(config.sensors[0].params["port"], "22");
    }
}
