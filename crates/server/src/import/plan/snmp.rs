//! Imported custom SNMP checks (ADR-0157): a check's flat parameters, the
//! lookups to make with it, and the rules of its channels.

use monitorflow_import::model::{Severity, SnmpCustom, SnmpLookup, SnmpValue};
use monitorflow_snmp::library;
use serde_json::{json, Map, Value};

use super::super::ImportedLookup;

/// The longest name of an imported custom check, so its channels' series
/// stay within the 64 characters of a metric.
pub const MAX_NAME: usize = 24;

/// A built-in lookup of the same values, names and states, if one is.
pub fn builtin_of(lookup: &SnmpLookup) -> Option<&'static str> {
    let wanted = serde_json::to_value(&lookup.entries).ok()?;
    library::builtin_lookups().into_iter().find_map(|b| {
        let same = b.lookup.entries.len() == lookup.entries.len()
            && b.lookup
                .entries
                .iter()
                .zip(wanted.as_array()?)
                .all(|(e, w)| {
                    w.get("value").and_then(Value::as_i64) == e.value
                        && w.get("max").and_then(Value::as_i64) == e.max
                        && w.get("text").and_then(Value::as_str) == e.text.as_deref()
                        && w.get("state").and_then(Value::as_str) == Some(e.state.as_str())
                        && w.get("name")
                            .and_then(Value::as_str)
                            .is_some_and(|n| n.eq_ignore_ascii_case(&e.name))
                });
        same.then_some(b.id)
    })
}

/// The parameters of `custom`, and the lookups of its own to make, by
/// channel. A lookup without values is left out: the note says so.
pub fn params(custom: &SnmpCustom) -> (Map<String, Value>, Vec<ImportedLookup>) {
    let mut params = Map::new();
    let mut lookups = Vec::new();
    if let Some(table) = &custom.table {
        params.insert("table".into(), json!(table));
    }
    if let Some(column) = &custom.name_column {
        params.insert("name_column".into(), json!(column));
    }
    for (i, c) in custom
        .channels
        .iter()
        .take(monitorflow_snmp::custom::MAX_CHANNELS)
        .enumerate()
    {
        let n = i + 1;
        params.insert(format!("oid{n}"), json!(c.oid));
        let name: String = c
            .name
            .chars()
            .filter(|ch| !ch.is_control())
            .take(64)
            .collect();
        params.insert(format!("name{n}"), json!(name));
        match c.value {
            SnmpValue::Gauge => {}
            SnmpValue::Counter => {
                params.insert(format!("type{n}"), json!("counter"));
            }
            SnmpValue::Text => {
                params.insert(format!("type{n}"), json!("text"));
            }
        }
        if let Some(unit) = c.unit.as_ref().filter(|u| !u.trim().is_empty()) {
            params.insert(
                format!("unit{n}"),
                json!(unit.chars().take(16).collect::<String>()),
            );
        }
        if c.value != SnmpValue::Text {
            if let Some(m) = c.multiply {
                params.insert(format!("multiply{n}"), json!(m.0.to_string()));
            }
            if let Some(d) = c.divide {
                params.insert(format!("divide{n}"), json!(d.0.to_string()));
            }
        }
        if let Some(lookup) = c
            .lookup
            .as_ref()
            .filter(|l| !l.entries.is_empty() && c.value != SnmpValue::Counter)
        {
            match builtin_of(lookup) {
                Some(id) => {
                    params.insert(format!("lookup{n}"), json!(id));
                }
                None => lookups.push(ImportedLookup {
                    channel: n,
                    name: lookup.name.chars().take(64).collect(),
                    definition: json!({"entries": lookup.entries, "otherwise": lookup.otherwise}),
                }),
            }
        }
        if let Some((mode, text, severity)) =
            c.expect.as_ref().filter(|_| c.value == SnmpValue::Text)
        {
            params.insert(format!("match{n}"), json!(mode));
            params.insert(format!("expect{n}"), json!(text));
            let severity = match severity {
                Severity::Warning => "warning",
                Severity::Critical => "critical",
            };
            params.insert(format!("severity{n}"), json!(severity));
        }
    }
    (params, lookups)
}

/// The rules a check's channels come with, by metric suffix: each value's
/// limits (`>` or `<`, warning, critical), and a state's warning at 1 and
/// critical at 2.
pub fn rules(
    custom: &SnmpCustom,
    keys: &[String],
) -> Vec<(String, &'static str, Option<f64>, Option<f64>)> {
    let mut out = Vec::new();
    for (c, key) in custom.channels.iter().zip(keys) {
        if let Some(l) = c.limits.as_ref().filter(|_| c.value != SnmpValue::Text) {
            if l.warning.is_some() || l.critical.is_some() {
                let comparison = if l.above { ">" } else { "<" };
                out.push((
                    key.clone(),
                    comparison,
                    l.warning.map(|v| v.0),
                    l.critical.map(|v| v.0),
                ));
            }
        }
        let lookup = c.lookup.as_ref().is_some_and(|l| !l.entries.is_empty())
            && c.value != SnmpValue::Counter;
        if lookup || c.expect.is_some() {
            out.push((format!("{key}_state"), ">=", Some(1.0), Some(2.0)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use monitorflow_import::model::{Num, SnmpChannel, SnmpLimits, SnmpLookupEntry, SnmpState};

    use super::*;

    fn channel(name: &str) -> SnmpChannel {
        SnmpChannel {
            oid: "1.3.6.1.4.1.1.0".into(),
            name: name.into(),
            value: SnmpValue::Gauge,
            unit: None,
            multiply: None,
            divide: None,
            lookup: None,
            expect: None,
            limits: None,
        }
    }

    fn lookup(entries: &[(i64, &str, SnmpState)]) -> SnmpLookup {
        SnmpLookup {
            name: "oid.test.state".into(),
            builtin: None,
            entries: entries
                .iter()
                .map(|(v, n, s)| SnmpLookupEntry {
                    value: Some(*v),
                    max: None,
                    text: None,
                    name: (*n).into(),
                    state: *s,
                })
                .collect(),
            otherwise: SnmpState::Warning,
        }
    }

    #[test]
    fn channels_become_parameters_lookups_and_rules() {
        let truth = lookup(&[
            (1, "true", SnmpState::Ok),
            (2, "False", SnmpState::Critical),
        ]);
        assert_eq!(
            builtin_of(&lookup(&[
                (1, "Yes", SnmpState::Ok),
                (2, "No", SnmpState::Critical)
            ])),
            None
        );
        assert_eq!(builtin_of(&truth), Some("builtin:truth_value"));
        let raid = lookup(&[
            (1, "Normal", SnmpState::Ok),
            (12, "Crashed", SnmpState::Critical),
        ]);
        assert_eq!(builtin_of(&raid), None);
        let custom = SnmpCustom {
            table: Some("1.3.6.1.4.1.6574.3.1.1".into()),
            name_column: Some("1.3.6.1.4.1.6574.3.1.1.2".into()),
            channels: vec![
                SnmpChannel {
                    divide: Some(Num(10.0)),
                    limits: Some(SnmpLimits {
                        above: true,
                        warning: Some(Num(50.0)),
                        critical: None,
                    }),
                    unit: Some("°C".into()),
                    ..channel("Temperature")
                },
                SnmpChannel {
                    lookup: Some(truth),
                    ..channel("Fan")
                },
                SnmpChannel {
                    lookup: Some(raid),
                    ..channel("RAID")
                },
                SnmpChannel {
                    value: SnmpValue::Text,
                    expect: Some(("contains".into(), "ok".into(), Severity::Warning)),
                    divide: Some(Num(2.0)),
                    ..channel("Status")
                },
                SnmpChannel {
                    lookup: Some(lookup(&[])),
                    ..channel("Missing")
                },
            ],
        };
        let (params, lookups) = params(&custom);
        assert_eq!(params["table"], json!("1.3.6.1.4.1.6574.3.1.1"));
        assert_eq!(params["divide1"], json!("10"));
        assert_eq!(params["unit1"], json!("°C"));
        assert_eq!(params["lookup2"], json!("builtin:truth_value"));
        assert!(!params.contains_key("lookup3"), "made with the check");
        assert!(!params.contains_key("divide4"), "a text has no scaling");
        assert_eq!(
            (params["match4"].clone(), params["severity4"].clone()),
            (json!("contains"), json!("warning"))
        );
        assert!(!params.contains_key("lookup5"));
        assert_eq!(lookups.len(), 1);
        assert_eq!(lookups[0].channel, 3);
        assert_eq!(
            lookups[0].definition["entries"][1]["name"],
            json!("Crashed")
        );
        let keys: Vec<String> = ["temperature", "fan", "raid", "status", "missing"]
            .iter()
            .map(|k| k.to_string())
            .collect();
        let rules = rules(&custom, &keys);
        assert_eq!(rules[0], ("temperature".into(), ">", Some(50.0), None));
        let states: Vec<&str> = rules[1..].iter().map(|r| r.0.as_str()).collect();
        assert_eq!(states, ["fan_state", "raid_state", "status_state"]);
    }
}
