//! snmp_exporter's `snmp.yml` (ADR-0157): the modules a target is walked
//! with, as the generator writes them. A module becomes custom SNMP
//! checks: its scalars as one check, each table as another, with their
//! types, scales, enumerations and the lookup that names a table's rows.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use crate::args;
use crate::model::{
    Fidelity, Num, SnmpChannel, SnmpCustom, SnmpLookup, SnmpLookupEntry, SnmpState, SnmpValue,
};
use crate::notes::Note;

/// Most channels of one check.
const MAX_CHANNELS: usize = 10;

/// A module of `snmp.yml`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct SnmpModule {
    pub walk: Vec<String>,
    pub get: Vec<String>,
    pub metrics: Vec<SnmpMetric>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct SnmpMetric {
    pub name: String,
    pub oid: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub indexes: Vec<Value>,
    pub lookups: Vec<SnmpMetricLookup>,
    pub enum_values: BTreeMap<String, String>,
    pub scale: Option<f64>,
    pub regex_extracts: Option<Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct SnmpMetricLookup {
    pub labelname: String,
    pub oid: String,
    #[serde(rename = "type")]
    pub kind: String,
}

/// Whether the modules of a `modules:` file are snmp_exporter's.
pub fn is_snmp(modules: &serde_json::Map<String, Value>) -> bool {
    modules
        .values()
        .any(|m| m.get("metrics").is_some() || m.get("walk").is_some())
}

fn numeric(oid: &str) -> Option<String> {
    let oid = oid.trim().trim_start_matches('.');
    (oid.split('.').count() >= 2
        && oid
            .split('.')
            .all(|a| !a.is_empty() && a.bytes().all(|b| b.is_ascii_digit())))
    .then(|| oid.to_owned())
}

fn value_of(kind: &str) -> SnmpValue {
    match kind {
        "counter" => SnmpValue::Counter,
        "DisplayString" | "OctetString" | "PhysAddress48" | "InetAddress" | "InetAddressIPv4"
        | "InetAddressIPv6" | "IpAddr" | "DateAndTime" | "Bits" => SnmpValue::Text,
        _ => SnmpValue::Gauge,
    }
}

fn lookup_of(metric: &SnmpMetric) -> Option<SnmpLookup> {
    if metric.enum_values.is_empty() {
        return None;
    }
    Some(SnmpLookup {
        name: format!("{} values", metric.name).chars().take(64).collect(),
        builtin: None,
        entries: metric
            .enum_values
            .iter()
            .filter_map(|(v, name)| {
                Some(SnmpLookupEntry {
                    value: Some(v.trim().parse().ok()?),
                    max: None,
                    text: None,
                    name: name.chars().take(64).collect(),
                    state: SnmpState::Ok,
                })
            })
            .take(100)
            .collect(),
        otherwise: SnmpState::Ok,
    })
}

fn channel(metric: &SnmpMetric, oid: String) -> SnmpChannel {
    let value = value_of(&metric.kind);
    SnmpChannel {
        oid,
        name: metric.name.chars().take(64).collect(),
        value,
        unit: None,
        multiply: metric
            .scale
            .filter(|s| s.is_finite() && *s > 0.0 && *s != 1.0 && value != SnmpValue::Text)
            .map(Num),
        divide: None,
        lookup: if value == SnmpValue::Counter {
            None
        } else {
            lookup_of(metric)
        },
        expect: None,
        limits: None,
    }
}

/// The checks of `module`: what it calls each one (`scalars`, or the
/// table's OID), the check, and how faithful it is.
pub fn checks(module: &SnmpModule) -> Vec<(String, SnmpCustom, Fidelity)> {
    let mut scalars = Vec::new();
    let mut tables: BTreeMap<String, Vec<&SnmpMetric>> = BTreeMap::new();
    for metric in &module.metrics {
        let Some(oid) = numeric(&metric.oid) else {
            continue;
        };
        if metric.indexes.is_empty() {
            scalars.push(channel(metric, format!("{oid}.0")));
        } else if let Some((table, _)) = oid.rsplit_once('.') {
            tables.entry(table.to_owned()).or_default().push(metric);
        }
    }
    let regex = module.metrics.iter().any(|m| m.regex_extracts.is_some());
    let fidelity = |count: usize| {
        if count > MAX_CHANNELS {
            Fidelity::Approximated(Note::new(
                "snmp_custom_more",
                args! {"count" => count - MAX_CHANNELS},
            ))
        } else if regex {
            Fidelity::Approximated(Note::plain("snmp_exporter_override"))
        } else {
            Fidelity::Exact
        }
    };
    let mut out = Vec::new();
    if !scalars.is_empty() {
        let count = scalars.len();
        scalars.truncate(MAX_CHANNELS);
        out.push((
            "scalars".to_owned(),
            SnmpCustom {
                table: None,
                name_column: None,
                channels: scalars,
            },
            fidelity(count),
        ));
    }
    for (table, metrics) in tables {
        // The first lookup of a text column of the same table names rows.
        let name_column = metrics
            .iter()
            .flat_map(|m| &m.lookups)
            .filter(|l| value_of(&l.kind) == SnmpValue::Text)
            .filter_map(|l| numeric(&l.oid))
            .find(|oid| oid.starts_with(&format!("{table}.")));
        let count = metrics.len();
        let channels = metrics
            .into_iter()
            .take(MAX_CHANNELS)
            .filter_map(|m| Some(channel(m, numeric(&m.oid)?)))
            .collect();
        out.push((
            table.clone(),
            SnmpCustom {
                table: Some(table),
                name_column,
                channels,
            },
            fidelity(count),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SNMP_YML: &str = r#"
modules:
  synology:
    walk: [1.3.6.1.4.1.6574.1, 1.3.6.1.4.1.6574.3]
    metrics:
    - name: systemStatus
      oid: 1.3.6.1.4.1.6574.1.1
      type: gauge
      enum_values: {1: Normal, 2: Failed}
    - name: temperature
      oid: 1.3.6.1.4.1.6574.1.2
      type: gauge
    - name: raidStatus
      oid: 1.3.6.1.4.1.6574.3.1.1.3
      type: gauge
      indexes: [{labelname: raidIndex, type: gauge}]
      lookups: [{labels: [raidIndex], labelname: raidName, oid: 1.3.6.1.4.1.6574.3.1.1.2, type: DisplayString}]
      enum_values: {1: Normal, 11: Degrade, 12: Crashed}
    - name: raidFreeSize
      oid: 1.3.6.1.4.1.6574.3.1.1.4
      type: counter
      indexes: [{labelname: raidIndex, type: gauge}]
      scale: 1024
"#;

    #[test]
    fn modules_become_a_check_of_scalars_and_one_per_table() {
        let value = crate::prometheus::config::parse_value(SNMP_YML).unwrap();
        let modules = value["modules"].as_object().unwrap();
        assert!(is_snmp(modules));
        let module: SnmpModule = serde_json::from_value(modules["synology"].clone()).unwrap();
        let checks = checks(&module);
        assert_eq!(checks.len(), 2);
        let (label, scalars, fidelity) = &checks[0];
        assert_eq!((label.as_str(), fidelity), ("scalars", &Fidelity::Exact));
        assert_eq!(scalars.channels[0].oid, "1.3.6.1.4.1.6574.1.1.0");
        assert_eq!(
            scalars.channels[0].lookup.as_ref().unwrap().entries[1].name,
            "Failed"
        );
        let (label, raid, _) = &checks[1];
        assert_eq!(label, "1.3.6.1.4.1.6574.3.1.1");
        assert_eq!(raid.table.as_deref(), Some("1.3.6.1.4.1.6574.3.1.1"));
        assert_eq!(
            raid.name_column.as_deref(),
            Some("1.3.6.1.4.1.6574.3.1.1.2")
        );
        assert_eq!(raid.channels[1].value, SnmpValue::Counter);
        assert_eq!(raid.channels[1].multiply, Some(Num(1024.0)));
        assert!(raid.channels[1].lookup.is_none(), "a counter has no lookup");
        let blackbox =
            crate::prometheus::config::parse_value("modules: {http_2xx: {prober: http}}").unwrap();
        assert!(!is_snmp(blackbox["modules"].as_object().unwrap()));
    }

    #[test]
    fn targets_walked_with_a_module_get_its_checks() {
        let prometheus = r#"
scrape_configs:
  - job_name: snmp
    scrape_interval: 2m
    metrics_path: /snmp
    params: {module: [synology]}
    static_configs:
      - targets: [192.0.2.41]
    relabel_configs:
      - {source_labels: [__address__], target_label: __param_target}
      - {source_labels: [__param_target], target_label: instance}
      - {target_label: __address__, replacement: "snmp-exporter:9116"}
"#;
        let file = |name: &str, text: &str| crate::source::File {
            name: name.into(),
            content: text.as_bytes().to_vec(),
        };
        let collected = crate::prometheus::files::collect(&[
            file("prometheus.yml", prometheus),
            file("snmp.yml", SNMP_YML),
        ])
        .unwrap();
        let inventory = collected.inventory();
        let custom: Vec<_> = inventory
            .checks
            .iter()
            .filter(|c| matches!(c.watch, crate::model::Watch::SnmpCustom(_)))
            .collect();
        assert_eq!(custom.len(), 2, "{:?}", inventory.checks);
        assert_eq!(custom[0].origin.name, "synology");
        assert_eq!(custom[0].interval_s, Some(120));
        assert!(inventory.checks.iter().any(|c| c.watch
            == crate::model::Watch::Snmp {
                group: "system".into()
            }));
    }
}
