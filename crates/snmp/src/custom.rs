//! Custom SNMP checks (ADR-0157): up to ten channels, each an OID a
//! person picked, or the columns of a table with one instance per row.
//!
//! Parameters, `N` from 1 to [`MAX_CHANNELS`]: `oidN` (required),
//! `nameN`, `keyN` (the metric suffix, from the name by default), `typeN`
//! (`gauge`, `counter` or `text`), `unitN`, `multiplyN`, `divideN`,
//! `lookupN` (a reference the server expands into `lookupN_map`, a
//! [`Lookup`] as JSON), and for text `matchN`, `expectN` and `severityN`.
//! A table check adds `table` (the table or its entry) and `name_column`;
//! every `oidN` is then a column under `table`.
//!
//! The parsing and evaluation here are pure, so the server previews a
//! check with the same code from what a read returned.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use async_snmp::Value;
use monitorflow_sensors::{Sample, Sensor, SensorError};
use serde::{Deserialize, Serialize};
use tokio::time::{timeout_at, Instant};

use crate::mibs::Val;
use crate::session::Session;
use crate::CREDENTIAL_PARAMS;

/// The kind of custom checks.
pub const CUSTOM_KIND: &str = "snmp_custom";
/// Most channels of one check.
pub const MAX_CHANNELS: usize = 10;
/// Most rows a table check follows.
pub const MAX_TABLE_ROWS: usize = 500;
/// Most entries of a lookup.
pub const MAX_LOOKUP_ENTRIES: usize = 100;
/// Longest name of a channel, a lookup entry or a lookup.
pub const MAX_NAME: usize = 64;
/// Longest unit.
pub const MAX_UNIT: usize = 16;
/// Longest expected text or pattern.
pub const MAX_EXPECT: usize = 256;
/// Longest text kept in a `_state` sample's `text` label.
pub const MAX_TEXT_LABEL: usize = 64;
/// Longest row name of a table.
const MAX_ROW_NAME: usize = 128;
/// Most arcs of an OID.
const MAX_ARCS: usize = 128;

/// The per-channel parameter names, without their number.
pub const CHANNEL_PARAMS: [&str; 12] = [
    "oid",
    "name",
    "key",
    "type",
    "unit",
    "multiply",
    "divide",
    "lookup",
    "lookup_map",
    "match",
    "expect",
    "severity",
];

/// How a channel reads its object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ValueKind {
    /// The value as read, scaled.
    #[default]
    Gauge,
    /// The difference per second between two collections, scaled.
    Counter,
    /// A text, shown and possibly checked.
    Text,
}

impl ValueKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gauge => "gauge",
            Self::Counter => "counter",
            Self::Text => "text",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "gauge" => Self::Gauge,
            "counter" => Self::Counter,
            "text" => Self::Text,
            _ => return None,
        })
    }
}

/// The state a lookup entry or an expected text gives, written as the
/// value of a `_state` series.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default, Hash,
)]
#[serde(rename_all = "lowercase")]
pub enum State {
    #[default]
    Ok,
    Warning,
    Critical,
}

impl State {
    pub fn value(self) -> f64 {
        match self {
            Self::Ok => 0.0,
            Self::Warning => 1.0,
            Self::Critical => 2.0,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "ok" => Self::Ok,
            "warning" => Self::Warning,
            "critical" => Self::Critical,
            _ => return None,
        })
    }
}

/// One entry of a lookup: an integer, a range of integers (`value` to
/// `max`), or an exact text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LookupEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<i64>,
    /// With `value`: the last integer of a range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub name: String,
    pub state: State,
}

/// Names and states for the values of an enumerated object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Lookup {
    pub entries: Vec<LookupEntry>,
    /// The state of a value no entry names.
    #[serde(default = "warning")]
    pub otherwise: State,
}

fn warning() -> State {
    State::Warning
}

fn clean_name(what: &str, value: &str, max: usize) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{what} must not be empty"));
    }
    if value.chars().count() > max || value.chars().any(char::is_control) {
        return Err(format!(
            "{what} must be at most {max} characters without control characters"
        ));
    }
    Ok(())
}

impl Lookup {
    /// Checks the entries: at most [`MAX_LOOKUP_ENTRIES`], each one form,
    /// ranges in order, names short and printable.
    pub fn validate(&self) -> Result<(), String> {
        if self.entries.is_empty() {
            return Err("a lookup needs at least one entry".into());
        }
        if self.entries.len() > MAX_LOOKUP_ENTRIES {
            return Err(format!("a lookup has at most {MAX_LOOKUP_ENTRIES} entries"));
        }
        for (i, entry) in self.entries.iter().enumerate() {
            let n = i + 1;
            clean_name(&format!("the name of entry {n}"), &entry.name, MAX_NAME)?;
            match (&entry.value, &entry.max, &entry.text) {
                (Some(_), None, None) | (None, None, Some(_)) => {}
                (Some(low), Some(high), None) if high >= low => {}
                (Some(_), Some(_), None) => {
                    return Err(format!("entry {n}: the range ends before it starts"))
                }
                _ => {
                    return Err(format!(
                        "entry {n} must have a `value`, a `value` and a `max`, or a `text`"
                    ))
                }
            }
            if let Some(text) = &entry.text {
                if text.chars().count() > MAX_NAME || text.chars().any(char::is_control) {
                    return Err(format!(
                        "entry {n}: the text must be at most {MAX_NAME} characters"
                    ));
                }
            }
        }
        Ok(())
    }

    /// The entry naming `reading`: a text entry by its text, then integer
    /// entries by the value, or by the text read as an integer.
    pub fn find(&self, reading: &Val) -> Option<&LookupEntry> {
        let text = display(reading);
        let integer = match reading {
            Val::Int(v) => Some(*v),
            Val::Uint(v) => i64::try_from(*v).ok(),
            Val::Text(t) => t.trim().parse::<i64>().ok(),
            Val::Oid(_) => None,
        };
        self.entries
            .iter()
            .find(|e| e.text.as_deref() == Some(text.as_str()))
            .or_else(|| {
                let v = integer?;
                self.entries.iter().find(|e| match (e.value, e.max) {
                    (Some(low), Some(high)) => (low..=high).contains(&v),
                    (Some(exact), None) => exact == v,
                    _ => false,
                })
            })
    }
}

/// How a text is compared with what is expected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchMode {
    Contains,
    NotContains,
    Equals,
    NotEquals,
    Matches,
    NotMatches,
}

impl MatchMode {
    pub const ALL: [Self; 6] = [
        Self::Contains,
        Self::NotContains,
        Self::Equals,
        Self::NotEquals,
        Self::Matches,
        Self::NotMatches,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Contains => "contains",
            Self::NotContains => "not_contains",
            Self::Equals => "equals",
            Self::NotEquals => "not_equals",
            Self::Matches => "matches",
            Self::NotMatches => "not_matches",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == value)
    }
}

/// What a text channel expects, and the state when it is not met.
#[derive(Debug, Clone)]
pub struct Expect {
    pub mode: MatchMode,
    pub text: String,
    regex: Option<regex::Regex>,
    pub severity: State,
}

impl PartialEq for Expect {
    fn eq(&self, other: &Self) -> bool {
        self.mode == other.mode && self.text == other.text && self.severity == other.severity
    }
}

impl Expect {
    pub fn new(mode: MatchMode, text: String, severity: State) -> Result<Self, String> {
        if text.len() > MAX_EXPECT || text.chars().any(|c| c.is_control() && c != '\t') {
            return Err(format!(
                "the expected text must be at most {MAX_EXPECT} bytes without control characters"
            ));
        }
        if severity == State::Ok {
            return Err("an unexpected text is a `warning` or `critical`".into());
        }
        let regex = match mode {
            MatchMode::Matches | MatchMode::NotMatches => Some(
                regex::RegexBuilder::new(&text)
                    .size_limit(1 << 20)
                    .dfa_size_limit(1 << 20)
                    .nest_limit(32)
                    .build()
                    .map_err(|e| format!("the pattern does not read: {e}"))?,
            ),
            _ => None,
        };
        Ok(Self {
            mode,
            text,
            regex,
            severity,
        })
    }

    /// Whether `text` is as expected.
    pub fn met(&self, text: &str) -> bool {
        match self.mode {
            MatchMode::Contains => text.contains(&self.text),
            MatchMode::NotContains => !text.contains(&self.text),
            MatchMode::Equals => text == self.text,
            MatchMode::NotEquals => text != self.text,
            MatchMode::Matches => self.regex.as_ref().is_some_and(|r| r.is_match(text)),
            MatchMode::NotMatches => !self.regex.as_ref().is_some_and(|r| r.is_match(text)),
        }
    }
}

/// One channel of a check.
#[derive(Debug, Clone, PartialEq)]
pub struct Channel {
    pub oid: Vec<u32>,
    pub name: String,
    pub key: String,
    pub kind: ValueKind,
    pub unit: String,
    pub multiply: f64,
    pub divide: f64,
    pub lookup: Option<Lookup>,
    pub expect: Option<Expect>,
}

/// A table to follow: its OID, and the column naming its rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableSpec {
    pub oid: Vec<u32>,
    pub name_column: Option<Vec<u32>>,
}

/// A checked custom check.
#[derive(Debug, Clone, PartialEq)]
pub struct Custom {
    pub channels: Vec<Channel>,
    pub table: Option<TableSpec>,
}

/// Reads a numeric OID, with or without a leading dot.
pub fn parse_oid(value: &str) -> Option<Vec<u32>> {
    let value = value.trim().strip_prefix('.').unwrap_or(value.trim());
    let arcs: Vec<u32> = value
        .split('.')
        .map(|a| {
            (!a.is_empty() && a.bytes().all(|b| b.is_ascii_digit()))
                .then(|| a.parse().ok())
                .flatten()
        })
        .collect::<Option<_>>()?;
    (arcs.len() >= 2 && arcs.len() <= MAX_ARCS && arcs[0] <= 2).then_some(arcs)
}

/// An OID as dotted numbers.
pub fn oid_text(arcs: &[u32]) -> String {
    arcs.iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(".")
}

/// The metric suffix a channel name gives: lowercase ASCII letters,
/// digits and underscores, starting with a letter, at most 32 bytes.
pub fn key_of(name: &str) -> String {
    let mut key = String::new();
    for c in name.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            key.push(c);
        } else if !key.ends_with('_') && !key.is_empty() {
            key.push('_');
        }
    }
    let mut key = key.trim_end_matches('_').to_owned();
    if key.starts_with(|c: char| c.is_ascii_digit()) {
        key.insert(0, 'v');
    }
    key.truncate(32);
    key.trim_end_matches('_').to_owned()
}

/// Whether `key` may name a channel's metric.
pub fn valid_key(key: &str) -> bool {
    let bytes = key.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 32
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
        && !key.ends_with("_state")
        && !key.ends_with('_')
}

fn factor(params: &BTreeMap<String, String>, name: &str) -> Result<f64, String> {
    match params.get(name).map(|v| v.trim()).filter(|v| !v.is_empty()) {
        None => Ok(1.0),
        Some(v) => v
            .parse::<f64>()
            .ok()
            .filter(|f| f.is_finite() && *f > 0.0)
            .ok_or_else(|| format!("parameter `{name}` must be a number above 0")),
    }
}

/// The number of a per-channel parameter, like 3 for `oid3` and
/// `lookup3_map`.
fn channel_number(key: &str) -> Option<(usize, &'static str)> {
    let (key, map) = match key.strip_suffix("_map") {
        Some(key) => (key, true),
        None => (key, false),
    };
    let (base, digits) = key.split_at(key.find(|c: char| c.is_ascii_digit())?);
    if digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let name = if map {
        (base == "lookup").then_some("lookup_map")?
    } else {
        CHANNEL_PARAMS
            .into_iter()
            .find(|p| *p == base && *p != "lookup_map")?
    };
    Some((digits.parse().ok()?, name))
}

/// Reads and checks the parameters of a `snmp_custom` check, apart from
/// the device. Errors never quote a secret.
pub fn parse(params: &BTreeMap<String, String>) -> Result<Custom, String> {
    let mut numbers = BTreeSet::new();
    for key in params.keys() {
        if ["host", "port", "table", "name_column"].contains(&key.as_str())
            || CREDENTIAL_PARAMS.contains(&key.as_str())
        {
            continue;
        }
        match channel_number(key) {
            Some((n, _)) if (1..=MAX_CHANNELS).contains(&n) => {
                numbers.insert(n);
            }
            Some(_) => {
                return Err(format!(
                    "a check has at most {MAX_CHANNELS} channels: `{}`",
                    key.escape_debug()
                ))
            }
            None => return Err(format!("unknown parameter `{}`", key.escape_debug())),
        }
    }
    if numbers.is_empty() {
        return Err("a custom SNMP check needs at least one channel: set `oid1`".into());
    }
    let get = |name: String| {
        params
            .get(&name)
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    };
    let table = match get("table".into()) {
        None => {
            if params.contains_key("name_column") {
                return Err("`name_column` is for a table: set `table`".into());
            }
            None
        }
        Some(t) => {
            let oid = parse_oid(&t).ok_or("parameter `table` must be a numeric OID")?;
            let name_column = match get("name_column".into()) {
                None => None,
                Some(c) => {
                    let column = parse_oid(&c)
                        .filter(|c| c.len() > oid.len() && c.starts_with(&oid))
                        .ok_or("parameter `name_column` must be a column under `table`")?;
                    Some(column)
                }
            };
            Some(TableSpec { oid, name_column })
        }
    };
    let mut channels: Vec<Channel> = Vec::new();
    for n in numbers {
        let p = |base: &str| format!("{base}{n}");
        let oid_text = get(p("oid")).ok_or_else(|| format!("channel {n} needs `oid{n}`"))?;
        let oid = parse_oid(&oid_text).ok_or_else(|| {
            format!("parameter `oid{n}` must be a numeric OID like 1.3.6.1.2.1.1.3.0")
        })?;
        if let Some(table) = &table {
            if oid.len() <= table.oid.len() || !oid.starts_with(&table.oid) {
                return Err(format!("parameter `oid{n}` must be a column under `table`"));
            }
        }
        let name = get(p("name")).unwrap_or_else(|| format!("Channel {n}"));
        clean_name(&format!("parameter `name{n}`"), &name, MAX_NAME)?;
        let key = match get(p("key")) {
            Some(key) => key,
            None => {
                // "RAID state" is the channel `raid`, whose state series is
                // `raid_state`.
                let derived = key_of(&name);
                let derived = derived
                    .strip_suffix("_state")
                    .map_or(derived.clone(), str::to_owned);
                if valid_key(&derived) {
                    derived
                } else {
                    format!("channel{n}")
                }
            }
        };
        if !valid_key(&key) {
            return Err(format!(
                "parameter `key{n}` must be lowercase letters, digits and underscores, start with a letter, be at most 32 bytes and not end with `_state`"
            ));
        }
        if channels.iter().any(|c| c.key == key) {
            return Err(format!("two channels have the key `{key}`"));
        }
        let kind = match get(p("type")) {
            None => ValueKind::Gauge,
            Some(t) => ValueKind::parse(&t).ok_or_else(|| {
                format!("parameter `type{n}` must be `gauge`, `counter` or `text`")
            })?,
        };
        let unit = get(p("unit")).unwrap_or_default();
        if unit.chars().count() > MAX_UNIT || unit.chars().any(char::is_control) {
            return Err(format!(
                "parameter `unit{n}` must be at most {MAX_UNIT} characters"
            ));
        }
        let multiply = factor(params, &p("multiply"))?;
        let divide = factor(params, &p("divide"))?;
        if kind == ValueKind::Text && (multiply != 1.0 || divide != 1.0) {
            return Err(format!("channel {n} is a text: it has no scaling"));
        }
        let lookup = match (get(p("lookup")), params.get(&format!("lookup{n}_map"))) {
            (_, Some(map)) => {
                let lookup: Lookup = serde_json::from_str(map)
                    .map_err(|e| format!("parameter `lookup{n}_map` does not read: {e}"))?;
                lookup
                    .validate()
                    .map_err(|e| format!("lookup of channel {n}: {e}"))?;
                Some(lookup)
            }
            (Some(_), None) => {
                return Err(format!(
                    "the lookup of channel {n} was not sent with the check"
                ))
            }
            (None, None) => None,
        };
        if lookup.is_some() && kind == ValueKind::Counter {
            return Err(format!(
                "channel {n} is a counter: a lookup names states, not rates"
            ));
        }
        let expect = match get(p("match")) {
            None => {
                if params.contains_key(&p("expect")) || params.contains_key(&p("severity")) {
                    return Err(format!("set `match{n}` to compare channel {n} with a text"));
                }
                None
            }
            Some(mode) => {
                if kind != ValueKind::Text {
                    return Err(format!(
                        "channel {n} compares a text: set `type{n}` to `text`"
                    ));
                }
                let mode = MatchMode::parse(&mode).ok_or_else(|| {
                    format!("parameter `match{n}` must be `contains`, `not_contains`, `equals`, `not_equals`, `matches` or `not_matches`")
                })?;
                // An empty text is a valid expectation of `equals`.
                let text = params.get(&p("expect")).cloned().unwrap_or_default();
                let severity = match get(p("severity")) {
                    None => State::Critical,
                    Some(s) => State::parse(&s)
                        .filter(|s| *s != State::Ok)
                        .ok_or_else(|| {
                            format!("parameter `severity{n}` must be `warning` or `critical`")
                        })?,
                };
                Some(Expect::new(mode, text, severity).map_err(|e| format!("channel {n}: {e}"))?)
            }
        };
        channels.push(Channel {
            oid,
            name,
            key,
            kind,
            unit,
            multiply,
            divide,
            lookup,
            expect,
        });
    }
    Ok(Custom { channels, table })
}

/// The SNMP type of a value, as Test shows it.
pub fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Integer(_) => "Integer",
        Value::OctetString(_) => "OctetString",
        Value::Null => "Null",
        Value::ObjectIdentifier(_) => "ObjectIdentifier",
        Value::IpAddress(_) => "IpAddress",
        Value::Counter32(_) => "Counter32",
        Value::Gauge32(_) => "Gauge32",
        Value::UInteger32(_) => "UInteger32",
        Value::TimeTicks(_) => "TimeTicks",
        Value::Opaque(_) => "Opaque",
        Value::Counter64(_) => "Counter64",
        Value::NoSuchObject => "noSuchObject",
        Value::NoSuchInstance => "noSuchInstance",
        Value::EndOfMibView => "endOfMibView",
        _ => "Unknown",
    }
}

/// An octet string as text: itself when it is printable UTF-8, its bytes
/// in hexadecimal otherwise (a MAC address, a binary flag set).
pub fn octets(bytes: &[u8]) -> String {
    // Many agents end their strings with a NUL.
    let end = bytes.iter().rposition(|b| *b != 0).map_or(0, |i| i + 1);
    let trimmed = &bytes[..end];
    match std::str::from_utf8(trimmed) {
        Ok(text)
            if !text
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\t' | '\r' | '\n')) =>
        {
            text.to_owned()
        }
        _ => bytes
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(":"),
    }
}

/// A value as a channel reads it; exceptions, Null and Opaque are none.
pub fn reading(value: &Value) -> Option<Val> {
    Some(match value {
        Value::Integer(v) => Val::Int(i64::from(*v)),
        Value::Counter32(v) | Value::Gauge32(v) | Value::UInteger32(v) | Value::TimeTicks(v) => {
            Val::Uint(u64::from(*v))
        }
        Value::Counter64(v) => Val::Uint(*v),
        Value::OctetString(bytes) => Val::Text(octets(bytes)),
        Value::ObjectIdentifier(o) => Val::Oid(o.arcs().to_vec()),
        Value::IpAddress(ip) => Val::Text(std::net::Ipv4Addr::from(*ip).to_string()),
        _ => return None,
    })
}

/// A value as text: numbers in decimal, OIDs dotted.
pub fn display(value: &Val) -> String {
    match value {
        Val::Int(v) => v.to_string(),
        Val::Uint(v) => v.to_string(),
        Val::Text(t) => t.clone(),
        Val::Oid(arcs) => oid_text(arcs),
    }
}

/// A value as a number: integers, and texts that hold one.
pub fn number(value: &Val) -> Option<f64> {
    match value {
        Val::Int(v) => Some(*v as f64),
        Val::Uint(v) => Some(*v as f64),
        Val::Text(t) => t.trim().parse::<f64>().ok().filter(|v| v.is_finite()),
        Val::Oid(_) => None,
    }
}

/// Text fit for a label: no control characters, at most `max` characters.
pub fn label_text(text: &str, max: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// The last raw value of a counter, and when it was read.
pub type Previous = (u64, i64);

/// What one channel writes for one reading: its samples, and the counter
/// to keep for the next collection. `previous` is the counter kept from
/// the last one; `now_ms` the time of this one.
pub fn evaluate(
    channel: &Channel,
    value: &Val,
    previous: Option<Previous>,
    now_ms: i64,
) -> (Vec<Sample>, Option<Previous>) {
    let mut samples = Vec::new();
    let mut kept = None;
    let scale = |v: f64| v * channel.multiply / channel.divide;
    match channel.kind {
        ValueKind::Gauge => {
            if let Some(v) = number(value) {
                samples.push(Sample::metric(channel.key.clone(), scale(v)));
            }
        }
        ValueKind::Counter => {
            let raw = match value {
                Val::Uint(v) => Some(*v),
                Val::Int(v) => u64::try_from(*v).ok(),
                Val::Text(t) => t.trim().parse::<u64>().ok(),
                Val::Oid(_) => None,
            };
            if let Some(raw) = raw {
                kept = Some((raw, now_ms));
                if let Some((before, at)) = previous {
                    // A counter that went back (a reboot, a wrap) gives
                    // no rate this time, like the interfaces group.
                    if raw >= before && now_ms > at {
                        let rate = (raw - before) as f64 / ((now_ms - at) as f64 / 1000.0);
                        samples.push(Sample::metric(channel.key.clone(), scale(rate)));
                    }
                }
            }
        }
        ValueKind::Text => {}
    }
    if channel.lookup.is_some() || channel.expect.is_some() || channel.kind == ValueKind::Text {
        let text = display(value);
        let (mut state, shown) = match &channel.lookup {
            Some(lookup) => match lookup.find(value) {
                Some(entry) => (entry.state, entry.name.clone()),
                None => (lookup.otherwise, text.clone()),
            },
            None => (State::Ok, text.clone()),
        };
        if let Some(expect) = &channel.expect {
            if !expect.met(&text) {
                state = state.max(expect.severity);
            }
        }
        samples.push(
            Sample::metric(format!("{}_state", channel.key), state.value())
                .with_label("text", label_text(&shown, MAX_TEXT_LABEL)),
        );
    }
    (samples, kept)
}

/// The instance name of each row of a table, by index: its name column's
/// text, or its index; a name two rows share gets the index added. At
/// most [`MAX_TABLE_ROWS`] rows, in index order.
pub fn row_names(
    indexes: &BTreeSet<Vec<u32>>,
    names: &BTreeMap<Vec<u32>, String>,
) -> BTreeMap<Vec<u32>, String> {
    let named: Vec<(Vec<u32>, String)> = indexes
        .iter()
        .take(MAX_TABLE_ROWS)
        .map(|index| {
            let name = names
                .get(index)
                .map(|n| label_text(n, MAX_ROW_NAME))
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| oid_text(index));
            (index.clone(), name)
        })
        .collect();
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, name) in &named {
        *seen.entry(name.as_str()).or_default() += 1;
    }
    let shared: BTreeSet<String> = seen
        .into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(name, _)| name.to_owned())
        .collect();
    named
        .into_iter()
        .map(|(index, name)| {
            let name = if shared.contains(&name) {
                format!("{name} ({})", oid_text(&index))
            } else {
                name
            };
            (index, name)
        })
        .collect()
}

/// The sensor of a `snmp_custom` check.
pub struct CustomSensor {
    session: Session,
    custom: Custom,
    /// Counters of the last collection, by channel and instance.
    counters: Mutex<BTreeMap<(usize, String), Previous>>,
}

impl std::fmt::Debug for CustomSensor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CustomSensor")
            .field("session", &self.session)
            .field("custom", &self.custom)
            .finish_non_exhaustive()
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

impl CustomSensor {
    pub fn new(device: crate::Device, custom: Custom) -> Self {
        Self {
            session: Session::new(device.host, device.port, device.credentials),
            custom,
            counters: Mutex::default(),
        }
    }

    /// Each channel's value: one row, `""`, for scalars; a row per index
    /// for a table.
    async fn read(
        &self,
        client: &async_snmp::Client,
    ) -> Result<Vec<(String, Vec<Option<Val>>)>, SensorError> {
        let channels = &self.custom.channels;
        let Some(table) = &self.custom.table else {
            let oids: Vec<Vec<u32>> = channels.iter().map(|c| c.oid.clone()).collect();
            let values = self.session.get_raw(client, &oids).await?;
            let row = channels
                .iter()
                .map(|c| values.get(&c.oid).and_then(reading))
                .collect();
            return Ok(vec![(String::new(), row)]);
        };
        let mut columns: Vec<BTreeMap<Vec<u32>, Val>> = Vec::new();
        let mut indexes = BTreeSet::new();
        for channel in channels {
            let (rows, cut) = self
                .session
                .walk_raw(client, &channel.oid, MAX_TABLE_ROWS)
                .await?;
            if cut {
                tracing::warn!(host = %self.session.host, "a table check was cut at {MAX_TABLE_ROWS} rows");
            }
            let column: BTreeMap<Vec<u32>, Val> = rows
                .into_iter()
                .filter_map(|(oid, v)| {
                    Some((
                        oid.strip_prefix(channel.oid.as_slice())?.to_vec(),
                        reading(&v)?,
                    ))
                })
                .filter(|(index, _)| !index.is_empty())
                .collect();
            indexes.extend(column.keys().cloned());
            columns.push(column);
        }
        let names: BTreeMap<Vec<u32>, String> = match &table.name_column {
            None => BTreeMap::new(),
            Some(column) => self
                .session
                .walk_raw(client, column, crate::session::MAX_ROWS)
                .await
                .map(|(rows, _)| rows)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|(oid, v)| {
                    Some((
                        oid.strip_prefix(column.as_slice())?.to_vec(),
                        display(&reading(&v)?),
                    ))
                })
                .collect(),
        };
        Ok(row_names(&indexes, &names)
            .into_iter()
            .map(|(index, name)| {
                let row = columns.iter().map(|c| c.get(&index).cloned()).collect();
                (name, row)
            })
            .collect())
    }
}

#[async_trait::async_trait]
impl Sensor for CustomSensor {
    async fn collect(&self, deadline: Instant) -> Result<Vec<Sample>, SensorError> {
        let client = self.session.client(deadline).await?;
        let rows = match timeout_at(deadline, self.read(&client)).await {
            Ok(rows) => rows?,
            Err(_) => {
                return Err(SensorError::Transient(
                    "the device did not answer in time".into(),
                ))
            }
        };
        let now = now_ms();
        let mut samples = Vec::new();
        let mut kept = self.counters.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = BTreeMap::new();
        for (instance, row) in rows {
            for (i, (channel, value)) in self.custom.channels.iter().zip(row).enumerate() {
                let Some(value) = value else { continue };
                let key = (i, instance.clone());
                let (written, counter) = evaluate(channel, &value, kept.get(&key).copied(), now);
                if let Some(counter) = counter {
                    next.insert(key, counter);
                }
                samples.extend(written.into_iter().map(|mut s| {
                    s.instance.clone_from(&instance);
                    s
                }));
            }
        }
        *kept = next;
        if samples.is_empty()
            && self
                .custom
                .channels
                .iter()
                .all(|c| c.kind != ValueKind::Counter)
        {
            let missing: Vec<String> = self
                .custom
                .channels
                .iter()
                .map(|c| oid_text(&c.oid))
                .collect();
            return Err(SensorError::Transient(format!(
                "the device has no object at {}",
                missing.join(", ")
            )));
        }
        Ok(samples)
    }
}

/// Builds the sensor of a `snmp_custom` check.
pub fn build(params: &BTreeMap<String, String>) -> Result<std::sync::Arc<dyn Sensor>, String> {
    let device = crate::device(params)?;
    let custom = parse(params)?;
    Ok(std::sync::Arc::new(CustomSensor::new(device, custom)))
}

#[cfg(test)]
#[path = "custom_tests.rs"]
mod tests;
