use super::*;
use crate::model::SnmpState;
use crate::prtg::tree::{Kind, Limits};

fn sensor(kind: &str, settings: &[(&str, &str)]) -> Object {
    let mut s = Object::new(10, Kind::Sensor, "Sensor", Some(1));
    s.sensor_type = Some(kind.into());
    s.settings = settings
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    s
}

fn channel(id: i64, name: &str) -> Channel {
    Channel {
        id,
        name: name.into(),
        ..Default::default()
    }
}

fn custom(mapped: Mapped) -> (SnmpCustom, Fidelity) {
    match mapped {
        Mapped::Watch {
            watch: Watch::SnmpCustom(c),
            fidelity,
            ..
        } => (*c, fidelity),
        other => panic!("{other:?}"),
    }
}

#[test]
fn snmp_custom_moves_its_oid_factors_and_limits() {
    let mut s = sensor(
        "snmpcustom",
        &[
            ("oid", ".1.3.6.1.4.1.6574.1.2.0"),
            ("channel", "Temperature"),
            ("unit", "°C"),
            ("factord", "10"),
            ("snmptype", "abs"),
        ],
    );
    let mut temp = channel(2001, "Temperature");
    temp.limits = Some(Limits {
        max_warning: Some(50.0),
        max_error: Some(60.0),
        ..Default::default()
    });
    s.channels = vec![channel(-4, "Downtime"), temp];
    let (c, fidelity) = custom(map(&s, "snmpcustom"));
    assert_eq!(fidelity, Fidelity::Exact);
    let ch = &c.channels[0];
    assert_eq!(ch.oid, "1.3.6.1.4.1.6574.1.2.0");
    assert_eq!(
        (ch.name.as_str(), ch.unit.as_deref()),
        ("Temperature", Some("°C"))
    );
    assert_eq!(ch.divide, Some(Num(10.0)));
    assert_eq!(ch.value, SnmpValue::Gauge);
    assert_eq!(
        ch.limits,
        Some(SnmpLimits {
            above: true,
            warning: Some(Num(50.0)),
            critical: Some(Num(60.0))
        })
    );
    let delta = sensor(
        "snmpcustom",
        &[("oid", "1.3.6.1.2.1.2.2.1.10.1"), ("snmptype", "diff")],
    );
    assert_eq!(
        custom(map(&delta, "snmpcustom")).0.channels[0].value,
        SnmpValue::Counter
    );
    assert!(matches!(
        map(&sensor("snmpcustom", &[("oid", "sysUpTime")]), "snmpcustom"),
        Mapped::Not(_)
    ));
}

#[test]
fn advanced_sensors_move_up_to_ten_channels() {
    let mut settings: Vec<(String, String)> = Vec::new();
    for n in 1..=12 {
        settings.push((
            format!("channel{n}oid"),
            format!("1.3.6.1.4.1.12356.101.4.1.{n}.0"),
        ));
        settings.push((format!("channel{n}name"), format!("Value {n}")));
    }
    settings.push(("usechannel2".into(), "0".into()));
    settings.push(("channel3unit".into(), "Custom".into()));
    settings.push(("channel3customunit".into(), "sessions".into()));
    settings.push(("channel4unit".into(), "Percent".into()));
    settings.push(("channel5snmptype".into(), "diff".into()));
    settings.push(("channel6valuelookup".into(), "oid.vendor.ha".into()));
    let refs: Vec<(&str, &str)> = settings
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let mut s = sensor("snmpcustomadvanced", &refs);
    s.lookups.insert(
        "oid.vendor.ha".into(),
        SnmpLookup {
            name: "oid.vendor.ha".into(),
            builtin: None,
            entries: vec![SnmpLookupEntry {
                value: Some(3),
                max: None,
                text: None,
                name: "Failed".into(),
                state: SnmpState::Critical,
            }],
            otherwise: SnmpState::Warning,
        },
    );
    let mut v1 = channel(1, "Value 1");
    v1.factor_m = Some(8.0);
    s.channels = vec![v1];
    let (c, fidelity) = custom(map(&s, "snmpcustomadvanced"));
    assert_eq!(c.channels.len(), 10);
    assert!(
        c.channels.iter().all(|ch| ch.name != "Value 2"),
        "a channel turned off"
    );
    assert_eq!(c.channels[0].multiply, Some(Num(8.0)));
    assert_eq!(c.channels[1].unit.as_deref(), Some("sessions"));
    assert_eq!(c.channels[2].unit.as_deref(), Some("%"));
    assert_eq!(c.channels[3].value, SnmpValue::Counter);
    assert_eq!(
        c.channels[4].lookup.as_ref().unwrap().entries[0].name,
        "Failed"
    );
    match fidelity {
        Fidelity::Approximated(note) => assert_eq!(note.code, "snmp_custom_more"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn tables_move_with_their_name_column() {
    let s = sensor(
        "snmpcustomtable",
        &[
            ("tableoid", "1.3.6.1.4.1.6574.3.1.1"),
            ("identcolumn", "1.3.6.1.4.1.6574.3.1.1.2"),
            ("channel1column", "1.3.6.1.4.1.6574.3.1.1.3"),
            ("channel1name", "Status"),
            ("channel1valuelookup", "oid.synology.raid"),
        ],
    );
    let (c, fidelity) = custom(map(&s, "snmpcustomtable"));
    assert_eq!(c.table.as_deref(), Some("1.3.6.1.4.1.6574.3.1.1"));
    assert_eq!(c.name_column.as_deref(), Some("1.3.6.1.4.1.6574.3.1.1.2"));
    assert_eq!(c.channels[0].oid, "1.3.6.1.4.1.6574.3.1.1.3");
    // The lookup file was not read: the channel says so.
    assert!(c.channels[0].lookup.as_ref().unwrap().entries.is_empty());
    match fidelity {
        Fidelity::Approximated(note) => assert_eq!(note.code, "snmp_lookup_missing"),
        other => panic!("{other:?}"),
    }
    let by_index = sensor(
        "snmpcustomtable",
        &[
            ("tableoid", "1.3.6.1.4.1.1"),
            ("identcolumn", "table_index"),
            ("channel1column", "1.3.6.1.4.1.1.1.2"),
        ],
    );
    assert_eq!(
        custom(map(&by_index, "snmpcustomtable")).0.name_column,
        None
    );
    assert!(matches!(
        map(
            &sensor(
                "snmpcustomtable",
                &[("channel1column", "1.3.6.1.4.1.1.1.2")]
            ),
            "snmpcustomtable"
        ),
        Mapped::Not(_)
    ));
}

#[test]
fn strings_move_with_what_they_expect() {
    let s = sensor(
        "snmpcustomstring",
        &[
            ("oid", "1.3.6.1.4.1.12356.101.4.1.1.0"),
            ("mustinclude", "v7."),
        ],
    );
    let ch = custom(map(&s, "snmpcustomstring")).0.channels.remove(0);
    assert_eq!(ch.value, SnmpValue::Text);
    assert_eq!(
        ch.expect,
        Some(("contains".into(), "v7.".into(), Severity::Critical))
    );
    let regex = sensor(
        "snmpcustomstring",
        &[
            ("oid", "1.3.6.1.2.1.1.1.0"),
            ("searchmethod", "1"),
            ("excludewarning", "error"),
            ("mustinclude", "ok"),
            ("maxlength", "20"),
        ],
    );
    let (c, fidelity) = custom(map(&regex, "snmpcustomstring"));
    assert_eq!(c.channels[0].expect.as_ref().unwrap().0, "matches");
    assert!(matches!(fidelity, Fidelity::Approximated(n) if n.code == "snmp_string_one"));
    // A string lookup matches the texts its entries name.
    let mut lookup = sensor(
        "snmpcustomlookup",
        &[("oid", "1.3.6.1.4.1.9.1.0"), ("lookups", "oid.vendor.mode")],
    );
    lookup.lookups.insert(
        "oid.vendor.mode".into(),
        SnmpLookup {
            name: "oid.vendor.mode".into(),
            builtin: None,
            entries: vec![SnmpLookupEntry {
                value: Some(1),
                max: None,
                text: None,
                name: "active".into(),
                state: SnmpState::Ok,
            }],
            otherwise: SnmpState::Critical,
        },
    );
    let ch = custom(map(&lookup, "snmpcustomlookup"))
        .0
        .channels
        .remove(0);
    let entry = &ch.lookup.unwrap().entries[0];
    assert_eq!((entry.text.as_deref(), entry.value), (Some("active"), None));
}

#[test]
fn custom_sensor_types_are_known() {
    use crate::prtg::sensors::{family, Family};
    for t in [
        "snmpcustom",
        "snmpcustomadvanced",
        "snmpcustomstring",
        "snmpcustomlookup",
        "snmpcustomtable",
        "snmplibrary",
        "paessler.snmp.custom_string_sensor",
    ] {
        assert_eq!(family(t), Family::SnmpCustom, "{t}");
    }
    assert_ne!(family("snmpuptime"), Family::SnmpCustom);
    assert!(settings().contains(&"channel10oid"));
    assert!(settings().contains(&"tableoid"));
}
