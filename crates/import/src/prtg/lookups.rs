//! PRTG lookup files (`.ovl`, XML), which name the values of an
//! enumerated channel and give each a state (ADR-0157):
//!
//! ```xml
//! <ValueLookup id="oid.vendor.raidstatus" desiredValue="1" undefinedState="Warning">
//!   <Lookups>
//!     <SingleInt state="Ok" value="1">Normal</SingleInt>
//!     <Range state="Error" from="10" to="19">Degraded</Range>
//!   </Lookups>
//! </ValueLookup>
//! ```
//!
//! `SingleInt`, `Boolean`, `BitField` (read as its value) and `Range`
//! entries; states `Ok` and `None` are OK, `Warning` a warning, `Error` a
//! critical, as PRTG shows them. A sensor's channel names its lookup by
//! `id`, without case.

use std::collections::BTreeMap;

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use crate::model::{SnmpLookup, SnmpLookupEntry, SnmpState};

/// Most entries kept of one lookup, as MonitorFlow keeps.
const MAX_ENTRIES: usize = 100;
/// Longest name kept.
const MAX_NAME: usize = 64;

fn state(value: &str) -> SnmpState {
    let v = value.trim().to_ascii_lowercase();
    let v = v.strip_prefix("ols").unwrap_or(&v);
    match v {
        "error" | "down" => SnmpState::Critical,
        "warning" | "unknown" | "" => SnmpState::Warning,
        _ => SnmpState::Ok,
    }
}

fn attribute(e: &BytesStart, name: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref().eq_ignore_ascii_case(name))
        .map(|a| super::api::unescape(&a.value))
}

/// Reads one lookup file: its ID (lowercase) and its lookup, or why not.
pub fn read(xml: &str) -> Result<(String, SnmpLookup), String> {
    let mut reader = Reader::from_str(xml);
    let mut id = None;
    let mut otherwise = SnmpState::Warning;
    let mut entries = Vec::new();
    // The entry being read: its value, its upper bound, its state.
    let mut open: Option<(Option<i64>, Option<i64>, SnmpState)> = None;
    let mut text = String::new();
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let name = e.name().as_ref().to_ascii_lowercase();
                match name.as_str() {
                    "valuelookup" => {
                        id = attribute(e, "id").map(|v| v.trim().to_ascii_lowercase());
                        if let Some(s) = attribute(e, "undefinedState") {
                            otherwise = state(&s);
                        }
                    }
                    "singleint" | "boolean" | "bitfield" => {
                        let value = attribute(e, "value").and_then(|v| v.trim().parse().ok());
                        open = Some((
                            value,
                            None,
                            state(&attribute(e, "state").unwrap_or_default()),
                        ));
                        text.clear();
                    }
                    "range" => {
                        let from = attribute(e, "from").and_then(|v| v.trim().parse().ok());
                        let to = attribute(e, "to").and_then(|v| v.trim().parse().ok());
                        open = Some((from, to, state(&attribute(e, "state").unwrap_or_default())));
                        text.clear();
                    }
                    _ => {}
                }
            }
            Event::Text(ref t) => {
                if open.is_some() {
                    text.push_str(t);
                }
            }
            Event::GeneralRef(ref r) => {
                if open.is_some() {
                    let reference: &str = r;
                    text.push_str(&super::api::unescape(&format!("&{reference};")));
                }
            }
            Event::End(ref e) => {
                let name = e.name().as_ref().to_ascii_lowercase();
                if matches!(
                    name.as_str(),
                    "singleint" | "boolean" | "bitfield" | "range"
                ) {
                    if let Some((Some(value), max, state)) = open.take() {
                        let name: String = text
                            .trim()
                            .chars()
                            .filter(|c| !c.is_control())
                            .take(MAX_NAME)
                            .collect();
                        let name = if name.is_empty() {
                            value.to_string()
                        } else {
                            name
                        };
                        if entries.len() < MAX_ENTRIES && max.is_none_or(|m| m >= value) {
                            entries.push(SnmpLookupEntry {
                                value: Some(value),
                                max,
                                text: None,
                                name,
                                state,
                            });
                        }
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let id = id
        .filter(|i| !i.is_empty())
        .ok_or("it is not a PRTG lookup (ValueLookup with an id)")?;
    if entries.is_empty() {
        return Err("it names no value".into());
    }
    let name: String = id.chars().take(MAX_NAME).collect();
    Ok((
        id,
        SnmpLookup {
            name,
            builtin: None,
            entries,
            otherwise,
        },
    ))
}

/// The lookups of several files, by ID; files that do not read are left
/// out with why, by file name.
pub fn read_all(
    files: &[(String, String)],
) -> (BTreeMap<String, SnmpLookup>, Vec<(String, String)>) {
    let mut lookups = BTreeMap::new();
    let mut failed = Vec::new();
    for (name, xml) in files {
        match read(xml) {
            Ok((id, lookup)) => {
                lookups.insert(id, lookup);
            }
            Err(why) => failed.push((name.clone(), why)),
        }
    }
    (lookups, failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_files_name_values_and_states() {
        let (id, lookup) = read(
            r#"<?xml version="1.0" encoding="UTF-8"?>
            <ValueLookup id="Oid.Vendor.RaidStatus" desiredValue="1" undefinedState="olsWarning"
                xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:noNamespaceSchemaLocation="PaeValueLookup.xsd">
              <Lookups>
                <SingleInt state="Ok" value="1">Normal</SingleInt>
                <SingleInt state="None" value="2">Repairing &amp; checking</SingleInt>
                <Range state="Error" from="10" to="19">Degraded</Range>
                <SingleInt state="error" value="12"></SingleInt>
                <Range state="Error" from="30" to="20">Backwards</Range>
              </Lookups>
            </ValueLookup>"#,
        )
        .unwrap();
        assert_eq!(id, "oid.vendor.raidstatus");
        assert_eq!(lookup.otherwise, SnmpState::Warning);
        assert_eq!(lookup.entries.len(), 4, "a backwards range is left out");
        assert_eq!(lookup.entries[1].name, "Repairing & checking");
        assert_eq!(lookup.entries[1].state, SnmpState::Ok);
        assert_eq!(
            (lookup.entries[2].value, lookup.entries[2].max),
            (Some(10), Some(19))
        );
        assert_eq!(lookup.entries[2].state, SnmpState::Critical);
        assert_eq!(lookup.entries[3].name, "12", "an empty name is the value");
        assert!(read("<ValueLookup><Lookups/></ValueLookup>").is_err());
        assert!(read(r#"<ValueLookup id="x"><Lookups/></ValueLookup>"#)
            .unwrap_err()
            .contains("no value"));
        assert!(read("not xml <").is_err());
        let (all, failed) = read_all(&[
            ("a.ovl".into(), r#"<ValueLookup id="a"><Lookups><Boolean state="Ok" value="1">Yes</Boolean></Lookups></ValueLookup>"#.into()),
            ("b.ovl".into(), "x".into()),
        ]);
        assert_eq!(all.keys().collect::<Vec<_>>(), ["a"]);
        assert_eq!(failed[0].0, "b.ovl");
    }
}
