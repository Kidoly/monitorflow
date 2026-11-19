//! PRTG's custom SNMP sensors (ADR-0157): SNMP Custom, Custom Advanced,
//! Custom String, Custom String Lookup and Custom Table, and those the
//! SNMP Library makes, which are of the same types. Each becomes one
//! custom SNMP check of its device, its channels with their OIDs, units,
//! factors, lookups and limits.

use std::sync::LazyLock;

use crate::args;
use crate::model::{
    Fidelity, Num, Severity, SnmpChannel, SnmpCustom, SnmpLimits, SnmpLookup, SnmpLookupEntry,
    SnmpValue, Watch,
};
use crate::notes::Note;

use super::sensors::{lost, Mapped};
use super::tree::{Channel, Object};

/// Most channels of one check, as MonitorFlow takes them.
const MAX_CHANNELS: usize = 10;

/// Names of the text conditions of SNMP Custom String, which PRTG does not
/// document: the usual ones, each with its mode and severity.
const STRING_CONDITIONS: [(&str, &str, Severity); 8] = [
    ("mustinclude", "contains", Severity::Critical),
    ("includemust", "contains", Severity::Critical),
    ("mustnotinclude", "not_contains", Severity::Critical),
    ("excludemust", "not_contains", Severity::Critical),
    ("warningmustinclude", "contains", Severity::Warning),
    ("includewarning", "contains", Severity::Warning),
    ("warningmustnotinclude", "not_contains", Severity::Warning),
    ("excludewarning", "not_contains", Severity::Warning),
];

/// The settings the importer reads of these sensors.
pub fn settings() -> &'static [&'static str] {
    static NAMES: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
        let mut names: Vec<&'static str> = vec![
            "oid",
            "channel",
            "unit",
            "factorm",
            "factord",
            "snmptype",
            "lookups",
            "tableoid",
            "identcolumn",
            "valuelookup",
            "searchmethod",
            "regex",
            "maxlength",
            "interpretresult",
            "extractnumber",
        ];
        names.extend(STRING_CONDITIONS.iter().map(|c| c.0));
        for n in 1..=MAX_CHANNELS + 2 {
            for field in [
                "oid",
                "name",
                "snmptype",
                "unit",
                "customunit",
                "valuelookup",
                "column",
            ] {
                names.push(Box::leak(format!("channel{n}{field}").into_boxed_str()));
            }
            names.push(Box::leak(format!("usechannel{n}").into_boxed_str()));
        }
        names
    });
    &NAMES
}

/// PRTG writes an OID with a leading dot sometimes, and a cell of names.
fn oid(value: &str) -> Option<String> {
    let value = value.trim().trim_start_matches('.');
    let ok = value.split('.').count() >= 2
        && value
            .split('.')
            .all(|a| !a.is_empty() && a.bytes().all(|b| b.is_ascii_digit()));
    ok.then(|| value.to_owned())
}

/// `abs`, `sign` and `float` are values as read; `diff` a delta.
fn value_of(snmptype: Option<&str>) -> SnmpValue {
    match snmptype.map(str::to_ascii_lowercase).as_deref() {
        Some("diff" | "delta" | "difference") => SnmpValue::Counter,
        _ => SnmpValue::Gauge,
    }
}

/// The unit of an advanced or table channel: its kind, or its own text.
fn unit_of(kind: Option<&str>, custom: Option<&str>) -> Option<String> {
    let lower = kind.map(str::to_ascii_lowercase);
    let unit = match lower.as_deref() {
        Some("percent") => "%",
        Some("temperature") => "°C",
        Some("timeseconds") => "s",
        Some("custom") => custom.unwrap_or(""),
        Some(other) if !matches!(other, "count" | "valuelookup" | "") => other,
        _ => "",
    };
    let unit: String = unit.chars().take(16).collect();
    (!unit.trim().is_empty()).then(|| unit.trim().to_owned())
}

fn factor(value: Option<f64>) -> Option<Num> {
    value
        .filter(|f| f.is_finite() && *f > 0.0 && *f != 1.0)
        .map(Num)
}

fn limits_of(channel: Option<&Channel>) -> Option<SnmpLimits> {
    let l = channel?.limits.as_ref()?;
    if l.max_warning.is_some() || l.max_error.is_some() {
        Some(SnmpLimits {
            above: true,
            warning: l.max_warning.map(Num),
            critical: l.max_error.map(Num),
        })
    } else if l.min_warning.is_some() || l.min_error.is_some() {
        Some(SnmpLimits {
            above: false,
            warning: l.min_warning.map(Num),
            critical: l.min_error.map(Num),
        })
    } else {
        None
    }
}

/// A lookup the sensor names, with its values when a file had them.
fn lookup(sensor: &Object, id: Option<&str>) -> Option<SnmpLookup> {
    let id = id?.trim().to_ascii_lowercase();
    if id.is_empty() || id == "none" {
        return None;
    }
    Some(sensor.lookups.get(&id).cloned().unwrap_or(SnmpLookup {
        name: id.chars().take(64).collect(),
        builtin: None,
        entries: Vec::new(),
        otherwise: crate::model::SnmpState::Warning,
    }))
}

/// The channel of the sensor named `name`, which holds its limits.
fn channel_named<'a>(sensor: &'a Object, name: &str) -> Option<&'a Channel> {
    sensor
        .channels
        .iter()
        .find(|c| c.name.trim().eq_ignore_ascii_case(name.trim()))
}

/// PRTG's own channels, not a value of the device.
fn own_channel(channel: &Channel) -> bool {
    let n = channel.name.to_ascii_lowercase();
    n == "downtime" || n.contains("response time") || n.contains("execution time")
}

/// The first data channel, for a sensor of one value.
fn data_channel(sensor: &Object) -> Option<&Channel> {
    sensor.channels.iter().find(|c| !own_channel(c))
}

/// Maps a custom SNMP sensor of type `sensor_type`.
pub fn map(sensor: &Object, sensor_type: &str) -> Mapped {
    let t = sensor_type.to_ascii_lowercase();
    let mut notes: Vec<Note> = Vec::new();
    let mut table = None;
    let mut name_column = None;
    let mut channels = Vec::new();
    if t.contains("table") || sensor.setting("tableoid").is_some() {
        table = sensor.setting("tableoid").and_then(oid);
        name_column = sensor
            .setting("identcolumn")
            .filter(|c| !c.eq_ignore_ascii_case("table_index"))
            .and_then(oid);
        numbered(sensor, "column", &mut channels, &mut notes);
    } else if t.contains("advanced") || sensor.setting("channel1oid").is_some() {
        numbered(sensor, "oid", &mut channels, &mut notes);
    } else if t.contains("string") || t == "snmpcustomlookup" {
        string(sensor, &t, &mut channels, &mut notes);
    } else if let Some(o) = sensor.setting("oid").and_then(oid) {
        let data = data_channel(sensor);
        let name = sensor
            .setting("channel")
            .map(str::to_owned)
            .or_else(|| data.map(|c| c.name.clone()))
            .unwrap_or_else(|| sensor.name.clone());
        let holder = channel_named(sensor, &name).or(data);
        channels.push(SnmpChannel {
            oid: o,
            name,
            value: value_of(sensor.setting("snmptype")),
            unit: sensor
                .setting("unit")
                .map(|u| u.chars().take(16).collect::<String>())
                .or_else(|| {
                    holder
                        .map(|c| c.unit.clone())
                        .filter(|u| !u.is_empty() && u != "#")
                }),
            multiply: factor(sensor.setting("factorm").and_then(super::api::number)),
            divide: factor(sensor.setting("factord").and_then(super::api::number)),
            lookup: lookup(
                sensor,
                holder
                    .and_then(|c| c.lookup.as_ref())
                    .map(|l| l.name.as_str()),
            ),
            expect: None,
            limits: limits_of(holder),
        });
    }
    if channels.is_empty() || (t.contains("table") && table.is_none()) {
        return Mapped::Not(Note::plain("snmp_no_oid"));
    }
    for channel in &mut channels {
        if channel.value == SnmpValue::Counter {
            // A lookup names states, not rates.
            channel.lookup = None;
        }
        if channel
            .lookup
            .as_ref()
            .is_some_and(|l| l.entries.is_empty())
        {
            notes.push(Note::new(
                "snmp_lookup_missing",
                args! {"name" => channel.lookup.as_ref().map_or("", |l| l.name.as_str())},
            ));
        }
    }
    let used: Vec<i64> = sensor
        .channels
        .iter()
        .filter(|c| {
            channels
                .iter()
                .any(|ch| ch.name.eq_ignore_ascii_case(&c.name))
        })
        .map(|c| c.id)
        .collect();
    let fidelity = match notes.into_iter().next() {
        None => Fidelity::Exact,
        Some(note) => Fidelity::Approximated(note),
    };
    Mapped::Watch {
        watch: Watch::SnmpCustom(Box::new(SnmpCustom {
            table,
            name_column,
            channels,
        })),
        alert: None,
        fidelity,
        limits: Vec::new(),
        lost: lost(sensor, &used)
            .into_iter()
            .filter(|c| !c.eq_ignore_ascii_case("downtime"))
            .collect(),
    }
}

/// The numbered channels of an advanced or table sensor: `channelNoid` or
/// `channelNcolumn`, used unless `usechannelN` is 0.
fn numbered(sensor: &Object, field: &str, out: &mut Vec<SnmpChannel>, notes: &mut Vec<Note>) {
    let mut found = 0;
    for n in 1..=MAX_CHANNELS + 2 {
        let get = |f: &str| sensor.setting(&format!("channel{n}{f}"));
        if sensor.setting(&format!("usechannel{n}")) == Some("0") {
            continue;
        }
        let Some(o) = get(field).and_then(oid) else {
            continue;
        };
        found += 1;
        if out.len() >= MAX_CHANNELS {
            continue;
        }
        let name = get("name")
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Channel {n}"));
        let holder = channel_named(sensor, &name);
        let kind = get("unit");
        let lookup_id = get("valuelookup").or_else(|| {
            holder
                .and_then(|c| c.lookup.as_ref())
                .map(|l| l.name.as_str())
        });
        out.push(SnmpChannel {
            oid: o,
            name,
            value: value_of(get("snmptype")),
            unit: unit_of(kind, get("customunit")),
            multiply: factor(holder.and_then(|c| c.factor_m)),
            divide: factor(holder.and_then(|c| c.factor_d)),
            lookup: lookup(sensor, lookup_id),
            expect: None,
            limits: limits_of(holder),
        });
    }
    if found > MAX_CHANNELS {
        notes.push(Note::new(
            "snmp_custom_more",
            args! {"count" => found - MAX_CHANNELS},
        ));
    }
}

/// SNMP Custom String and String Lookup: one text channel.
fn string(sensor: &Object, t: &str, out: &mut Vec<SnmpChannel>, notes: &mut Vec<Note>) {
    let Some(o) = sensor.setting("oid").and_then(oid) else {
        return;
    };
    let regex = sensor
        .setting("searchmethod")
        .is_some_and(|m| m == "1" || m.eq_ignore_ascii_case("regex"))
        || sensor.setting("regex") == Some("1");
    let mut expect = None;
    let mut more = false;
    for (setting, mode, severity) in STRING_CONDITIONS {
        if let Some(text) = sensor.setting(setting) {
            let mode = match (mode, regex) {
                ("contains", true) => "matches",
                ("not_contains", true) => "not_matches",
                (mode, _) => mode,
            };
            if expect.is_none() {
                expect = Some((mode.to_owned(), text.to_owned(), severity));
            } else {
                more = true;
            }
        }
    }
    if more {
        notes.push(Note::plain("snmp_string_one"));
    }
    if sensor.setting("maxlength").is_some()
        || sensor.setting("interpretresult").is_some()
        || sensor.setting("extractnumber").is_some()
    {
        notes.push(Note::plain("snmp_string_length"));
    }
    let data = data_channel(sensor);
    // A string lookup maps the text itself: its entries match texts.
    let lookup = lookup(
        sensor,
        sensor.setting("lookups").or_else(|| {
            data.and_then(|c| c.lookup.as_ref())
                .map(|l| l.name.as_str())
        }),
    )
    .map(|mut l| {
        if t.contains("lookup") {
            for e in &mut l.entries {
                *e = SnmpLookupEntry {
                    value: None,
                    max: None,
                    text: Some(e.name.clone()),
                    name: e.name.clone(),
                    state: e.state,
                };
            }
        }
        l
    });
    out.push(SnmpChannel {
        oid: o,
        name: data.map_or_else(|| sensor.name.clone(), |c| c.name.clone()),
        value: SnmpValue::Text,
        unit: None,
        multiply: None,
        divide: None,
        lookup,
        expect,
        limits: None,
    });
}

#[cfg(test)]
#[path = "snmp_tests.rs"]
mod tests;
