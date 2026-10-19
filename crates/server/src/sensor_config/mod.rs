//! Sensor configuration pushed to agents (ADR-0016).
//!
//! The effective configuration of a server is three layers, keyed by
//! `(name, instance)`: [`builtin_defaults`], then its site's rows, then its
//! own rows. A later layer replaces a sensor of an earlier one and a
//! disabled row removes it. It is sent after every accepted agent `Hello`
//! and again by the [`watcher`] when the server's revision changes.

mod store;
pub mod watcher;

use std::collections::BTreeMap;

use monitorflow_common::proto::v1 as proto;

pub use store::SensorConfigStore;
pub use watcher::{SensorConfigs, WatcherSettings};

/// Interval of the built-in sensors.
pub const DEFAULT_INTERVAL_MS: u64 = 60_000;

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

    fn to_proto(&self) -> proto::SensorSpec {
        proto::SensorSpec {
            kind: self.kind.clone(),
            name: self.name.clone(),
            instance: self.instance.clone(),
            interval_ms: self.interval_ms,
            timeout_ms: self.timeout_ms.unwrap_or(0),
            params: self.params.clone().into_iter().collect(),
        }
    }
}

/// What every server runs unless its site or itself overrides it: cpu,
/// memory and disk every minute.
pub fn builtin_defaults() -> Vec<Spec> {
    ["cpu", "memory", "disk"]
        .into_iter()
        .map(|kind| Spec::new(kind, DEFAULT_INTERVAL_MS))
        .collect()
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

/// The built-in defaults overlaid with `rows`, site rows before server
/// rows, sorted by `(name, instance)`.
pub fn resolve(mut rows: Vec<Row>) -> Vec<Spec> {
    let mut sensors: BTreeMap<(String, String), Spec> = builtin_defaults()
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
        let resolved = resolve(Vec::new());
        assert_eq!(
            keys(&resolved),
            [
                ("cpu", "", 60_000),
                ("disk", "", 60_000),
                ("memory", "", 60_000)
            ]
        );
    }

    #[test]
    fn server_rows_win_over_site_rows_which_win_over_defaults() {
        let resolved = resolve(vec![
            // Given in the "wrong" order: the layer decides, not the order.
            row(Layer::Server, "cpu", "", 5_000, true),
            row(Layer::Site, "cpu", "", 30_000, true),
            row(Layer::Site, "port", "22", 10_000, true),
            row(Layer::Site, "port", "443", 10_000, true),
            row(Layer::Server, "port", "443", 10_000, false),
            row(Layer::Site, "memory", "", 60_000, false),
            row(Layer::Server, "memory", "", 20_000, true),
            row(Layer::Site, "disk", "", 60_000, false),
            row(Layer::Server, "service", "nginx", 15_000, true),
        ]);
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
    fn proto_uses_zero_for_the_default_timeout() {
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
        assert_eq!(config.sensors[0].timeout_ms, 0);
        assert_eq!(config.sensors[1].timeout_ms, 2_000);
        assert_eq!(config.sensors[0].params["port"], "22");
    }
}
