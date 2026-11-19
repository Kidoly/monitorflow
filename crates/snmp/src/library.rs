//! What MonitorFlow knows without a MIB file (ADR-0157): built-in lookups
//! for common states, and a short library of common vendor objects. Facts
//! about objects (an OID, its name, what it reads), never a vendor's file.

use crate::custom::{Lookup, LookupEntry, State, ValueKind};

/// A built-in lookup: its ID (`builtin:<name>`), English name and entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinLookup {
    pub id: &'static str,
    pub name: &'static str,
    pub lookup: Lookup,
}

fn entries(items: &[(i64, &str, State)], otherwise: State) -> Lookup {
    Lookup {
        entries: items
            .iter()
            .map(|(value, name, state)| LookupEntry {
                value: Some(*value),
                max: None,
                text: None,
                name: (*name).to_owned(),
                state: *state,
            })
            .collect(),
        otherwise,
    }
}

/// Every built-in lookup.
pub fn builtin_lookups() -> Vec<BuiltinLookup> {
    use State::{Critical, Ok, Warning};
    vec![
        BuiltinLookup {
            id: "builtin:ok_warning_failed",
            name: "OK, warning, failed",
            lookup: entries(
                &[
                    (1, "OK", Ok),
                    (2, "Warning", Warning),
                    (3, "Failed", Critical),
                ],
                Warning,
            ),
        },
        BuiltinLookup {
            id: "builtin:truth_value",
            name: "True or false (TruthValue)",
            lookup: entries(&[(1, "True", Ok), (2, "False", Critical)], Warning),
        },
        BuiltinLookup {
            id: "builtin:if_status",
            name: "Interface status (IF-MIB)",
            lookup: entries(
                &[
                    (1, "Up", Ok),
                    (2, "Down", Critical),
                    (3, "Testing", Warning),
                    (4, "Unknown", Warning),
                    (5, "Dormant", Ok),
                    (6, "Not present", Critical),
                    (7, "Lower layer down", Critical),
                ],
                Warning,
            ),
        },
        BuiltinLookup {
            id: "builtin:device_status",
            name: "Device status (HOST-RESOURCES-MIB)",
            lookup: entries(
                &[
                    (1, "Unknown", Warning),
                    (2, "Running", Ok),
                    (3, "Warning", Warning),
                    (4, "Testing", Warning),
                    (5, "Down", Critical),
                ],
                Warning,
            ),
        },
        BuiltinLookup {
            id: "builtin:battery_status",
            name: "Battery status (UPS-MIB)",
            lookup: entries(
                &[
                    (1, "Unknown", Warning),
                    (2, "Normal", Ok),
                    (3, "Low", Critical),
                    (4, "Depleted", Critical),
                ],
                Warning,
            ),
        },
        BuiltinLookup {
            id: "builtin:enabled",
            name: "Enabled or disabled",
            lookup: entries(&[(1, "Enabled", Ok), (2, "Disabled", Warning)], Warning),
        },
    ]
}

/// A built-in lookup by ID.
pub fn builtin_lookup(id: &str) -> Option<BuiltinLookup> {
    builtin_lookups().into_iter().find(|l| l.id == id)
}

/// The state a name of an enumeration suggests, for lookups made from a
/// MIB: OK, critical, or warning for the rest.
pub fn state_of(name: &str) -> State {
    let n = name.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| n.contains(w));
    if has(&[
        "fail",
        "down",
        "crit",
        "error",
        "depleted",
        "crash",
        "fault",
        "broken",
        "dead",
        "lost",
        "absent",
        "notpresent",
        "missing",
        "bad",
    ]) {
        State::Critical
    } else if has(&[
        "ok", "normal", "up", "running", "good", "online", "active", "true", "enabled", "ready",
        "present", "healthy", "master", "standby", "backup", "idle", "on",
    ]) {
        State::Ok
    } else {
        State::Warning
    }
}

/// A lookup made from a MIB enumeration, its states guessed by name.
pub fn lookup_from(enumeration: &[(i64, String)]) -> Lookup {
    Lookup {
        entries: enumeration
            .iter()
            .take(crate::custom::MAX_LOOKUP_ENTRIES)
            .map(|(value, name)| LookupEntry {
                value: Some(*value),
                max: None,
                text: None,
                name: name.chars().take(crate::custom::MAX_NAME).collect(),
                state: state_of(name),
            })
            .collect(),
        otherwise: State::Warning,
    }
}

/// An object of the library.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LibraryObject {
    /// The vendor or MIB it comes from, as people know it.
    pub vendor: &'static str,
    pub module: &'static str,
    pub name: &'static str,
    /// The object's OID: a scalar ends in `.0`; a column has none, and
    /// `table` names its table.
    pub oid: &'static str,
    pub table: Option<&'static str>,
    pub kind: ValueKind,
    pub unit: &'static str,
    pub divide: f64,
    pub multiply: f64,
    pub lookup: Option<&'static str>,
    /// For a column: the column naming its rows.
    pub name_column: Option<&'static str>,
    /// In English, what it reads.
    pub about: &'static str,
}

const fn scalar(
    vendor: &'static str,
    module: &'static str,
    name: &'static str,
    oid: &'static str,
    kind: ValueKind,
    unit: &'static str,
    about: &'static str,
) -> LibraryObject {
    LibraryObject {
        vendor,
        module,
        name,
        oid,
        table: None,
        kind,
        unit,
        divide: 1.0,
        multiply: 1.0,
        lookup: None,
        name_column: None,
        about,
    }
}

/// A column of `table`, its rows named by `name_column`.
const fn column(
    object: LibraryObject,
    table: &'static str,
    name_column: &'static str,
) -> LibraryObject {
    LibraryObject {
        table: Some(table),
        name_column: Some(name_column),
        ..object
    }
}

/// The library: common objects of net-snmp, Synology, APC, Fortinet and
/// Cisco devices.
pub fn library() -> Vec<LibraryObject> {
    use ValueKind::{Counter, Gauge, Text};
    let ucd = |name, oid, kind, unit, about| {
        scalar("net-snmp", "UCD-SNMP-MIB", name, oid, kind, unit, about)
    };
    let syno = |name, oid, kind, unit, about| {
        scalar(
            "Synology",
            "SYNOLOGY-SYSTEM-MIB",
            name,
            oid,
            kind,
            unit,
            about,
        )
    };
    let apc =
        |name, oid, kind, unit, about| scalar("APC", "PowerNet-MIB", name, oid, kind, unit, about);
    let forti = |name, oid, kind, unit, about| {
        scalar(
            "Fortinet",
            "FORTINET-FORTIGATE-MIB",
            name,
            oid,
            kind,
            unit,
            about,
        )
    };
    let raid = "1.3.6.1.4.1.6574.3.1.1";
    let disk = "1.3.6.1.4.1.6574.2.1.1";
    let cpm = "1.3.6.1.4.1.9.9.109.1.1.1.1";
    let env = "1.3.6.1.4.1.9.9.13.1.3.1";
    let mut out = vec![
        ucd(
            "laLoad.1",
            "1.3.6.1.4.1.2021.10.1.3.1",
            Gauge,
            "",
            "Load average over 1 minute",
        ),
        ucd(
            "laLoad.2",
            "1.3.6.1.4.1.2021.10.1.3.2",
            Gauge,
            "",
            "Load average over 5 minutes",
        ),
        ucd(
            "laLoad.3",
            "1.3.6.1.4.1.2021.10.1.3.3",
            Gauge,
            "",
            "Load average over 15 minutes",
        ),
        LibraryObject {
            multiply: 1024.0,
            ..ucd(
                "memTotalReal.0",
                "1.3.6.1.4.1.2021.4.5.0",
                Gauge,
                "B",
                "Memory, in all",
            )
        },
        LibraryObject {
            multiply: 1024.0,
            ..ucd(
                "memAvailReal.0",
                "1.3.6.1.4.1.2021.4.6.0",
                Gauge,
                "B",
                "Memory available",
            )
        },
        ucd(
            "ssCpuIdle.0",
            "1.3.6.1.4.1.2021.11.11.0",
            Gauge,
            "%",
            "Processor idle time",
        ),
        LibraryObject {
            lookup: Some("builtin:ok_warning_failed"),
            ..syno(
                "systemStatus.0",
                "1.3.6.1.4.1.6574.1.1.0",
                Gauge,
                "",
                "Whether the system is well: 1 normal, 2 failed",
            )
        },
        syno(
            "temperature.0",
            "1.3.6.1.4.1.6574.1.2.0",
            Gauge,
            "°C",
            "System temperature",
        ),
        column(
            scalar(
                "Synology",
                "SYNOLOGY-RAID-MIB",
                "raidStatus",
                "1.3.6.1.4.1.6574.3.1.1.3",
                Gauge,
                "",
                "State of each volume: 1 normal, 11 degraded, 12 crashed",
            ),
            raid,
            "1.3.6.1.4.1.6574.3.1.1.2",
        ),
        column(
            scalar(
                "Synology",
                "SYNOLOGY-DISK-MIB",
                "diskStatus",
                "1.3.6.1.4.1.6574.2.1.1.5",
                Gauge,
                "",
                "State of each disk: 1 normal, 5 crashed",
            ),
            disk,
            "1.3.6.1.4.1.6574.2.1.1.2",
        ),
        column(
            scalar(
                "Synology",
                "SYNOLOGY-DISK-MIB",
                "diskTemperature",
                "1.3.6.1.4.1.6574.2.1.1.6",
                Gauge,
                "°C",
                "Temperature of each disk",
            ),
            disk,
            "1.3.6.1.4.1.6574.2.1.1.2",
        ),
        apc(
            "upsAdvBatteryCapacity.0",
            "1.3.6.1.4.1.318.1.1.1.2.2.1.0",
            Gauge,
            "%",
            "Battery charge",
        ),
        apc(
            "upsAdvBatteryTemperature.0",
            "1.3.6.1.4.1.318.1.1.1.2.2.2.0",
            Gauge,
            "°C",
            "Battery temperature",
        ),
        LibraryObject {
            divide: 100.0,
            ..apc(
                "upsAdvBatteryRunTimeRemaining.0",
                "1.3.6.1.4.1.318.1.1.1.2.2.3.0",
                Gauge,
                "s",
                "Runtime left on battery",
            )
        },
        apc(
            "upsBasicOutputStatus.0",
            "1.3.6.1.4.1.318.1.1.1.4.1.1.0",
            Gauge,
            "",
            "Output status: 2 on line, 3 on battery",
        ),
        forti(
            "fgSysCpuUsage.0",
            "1.3.6.1.4.1.12356.101.4.1.3.0",
            Gauge,
            "%",
            "Processor use",
        ),
        forti(
            "fgSysMemUsage.0",
            "1.3.6.1.4.1.12356.101.4.1.4.0",
            Gauge,
            "%",
            "Memory use",
        ),
        forti(
            "fgSysSesCount.0",
            "1.3.6.1.4.1.12356.101.4.1.8.0",
            Gauge,
            "sessions",
            "Open sessions",
        ),
        forti(
            "fgSysVersion.0",
            "1.3.6.1.4.1.12356.101.4.1.1.0",
            Text,
            "",
            "Firmware version",
        ),
        column(
            scalar(
                "Cisco",
                "CISCO-PROCESS-MIB",
                "cpmCPUTotal5minRev",
                "1.3.6.1.4.1.9.9.109.1.1.1.1.8",
                Gauge,
                "%",
                "Processor use over 5 minutes, per processor",
            ),
            cpm,
            "1.3.6.1.4.1.9.9.109.1.1.1.1.2",
        ),
        column(
            scalar(
                "Cisco",
                "CISCO-ENVMON-MIB",
                "ciscoEnvMonTemperatureStatusValue",
                "1.3.6.1.4.1.9.9.13.1.3.1.3",
                Gauge,
                "°C",
                "Each temperature sensor",
            ),
            env,
            "1.3.6.1.4.1.9.9.13.1.3.1.2",
        ),
        column(
            scalar(
                "Cisco",
                "CISCO-ENVMON-MIB",
                "ciscoEnvMonTemperatureState",
                "1.3.6.1.4.1.9.9.13.1.3.1.6",
                Gauge,
                "",
                "State of each temperature sensor: 1 normal, 2 warning, 3 critical",
            ),
            env,
            "1.3.6.1.4.1.9.9.13.1.3.1.2",
        ),
        column(
            scalar(
                "IF-MIB",
                "IF-MIB",
                "ifHCInOctets",
                "1.3.6.1.2.1.31.1.1.1.6",
                Counter,
                "B/s",
                "Bytes in of each interface, 64-bit",
            ),
            "1.3.6.1.2.1.31.1.1.1",
            "1.3.6.1.2.1.31.1.1.1.1",
        ),
    ];
    out.sort_by_key(|o| (o.vendor, o.name));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::custom::parse_oid;

    #[test]
    fn built_in_lookups_are_valid() {
        let all = builtin_lookups();
        assert_eq!(all.len(), 6);
        for l in &all {
            l.lookup.validate().unwrap();
            assert!(l.id.starts_with("builtin:"));
        }
        assert_eq!(
            builtin_lookup("builtin:truth_value")
                .unwrap()
                .lookup
                .entries[1]
                .state,
            State::Critical
        );
        assert!(builtin_lookup("builtin:nope").is_none());
    }

    #[test]
    fn states_are_guessed_from_names() {
        assert_eq!(state_of("normal"), State::Ok);
        assert_eq!(state_of("running"), State::Ok);
        assert_eq!(state_of("failed"), State::Critical);
        assert_eq!(state_of("lowerLayerDown"), State::Critical);
        assert_eq!(state_of("degraded"), State::Warning);
        assert_eq!(state_of("repairing"), State::Warning);
        let lookup = lookup_from(&[(1, "normal".into()), (12, "crashed".into())]);
        lookup.validate().unwrap();
        assert_eq!(lookup.entries[1].state, State::Critical);
    }

    #[test]
    fn library_objects_read() {
        for o in library() {
            assert!(parse_oid(o.oid).is_some(), "{}", o.name);
            if let Some(table) = o.table {
                assert!(o.oid.starts_with(table), "{}", o.name);
                assert!(o.name_column.unwrap().starts_with(table), "{}", o.name);
            }
            if let Some(lookup) = o.lookup {
                assert!(builtin_lookup(lookup).is_some(), "{}", o.name);
            }
            assert!(o.unit.chars().count() <= crate::custom::MAX_UNIT);
        }
    }
}
