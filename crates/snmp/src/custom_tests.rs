use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::mibs::Val;

fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn one(pairs: &[(&str, &str)]) -> Channel {
    let mut all = vec![("host", "192.0.2.1"), ("community", "c")];
    all.extend_from_slice(pairs);
    parse(&params(&all)).unwrap().channels.remove(0)
}

fn lookup() -> Lookup {
    serde_json::from_str(
        r#"{"entries": [
            {"value": 1, "name": "OK", "state": "ok"},
            {"value": 2, "name": "Warning", "state": "warning"},
            {"value": 3, "name": "Failed", "state": "critical"},
            {"value": 10, "max": 19, "name": "Rebuilding", "state": "warning"},
            {"text": "degraded", "name": "Degraded", "state": "critical"}
        ]}"#,
    )
    .unwrap()
}

fn label<'a>(sample: &'a Sample, name: &str) -> Option<&'a str> {
    sample.labels.get(name).map(String::as_str)
}

#[test]
fn channels_are_read_with_their_defaults() {
    let custom = parse(&params(&[
        ("host", "192.0.2.1"),
        ("oid1", ".1.3.6.1.4.1.12356.101.4.1.8.0"),
        ("name1", "Sessions"),
        ("oid2", "1.3.6.1.4.1.9.9.13.1.3.1.3.1"),
        ("name2", "Inlet temperature (°C)"),
        ("divide2", "10"),
        ("unit2", "°C"),
        ("oid3", "1.3.6.1.2.1.2.2.1.10.1"),
        ("type3", "counter"),
        ("multiply3", "8"),
        ("key3", "in_bits"),
    ]))
    .unwrap();
    assert!(custom.table.is_none());
    let [sessions, temp, bits] = &custom.channels[..] else {
        panic!("{custom:?}")
    };
    assert_eq!(sessions.oid, vec![1, 3, 6, 1, 4, 1, 12356, 101, 4, 1, 8, 0]);
    assert_eq!(sessions.key, "sessions");
    assert_eq!(sessions.kind, ValueKind::Gauge);
    assert_eq!((sessions.multiply, sessions.divide), (1.0, 1.0));
    assert_eq!(temp.key, "inlet_temperature_c");
    assert_eq!(temp.unit, "°C");
    assert_eq!(temp.divide, 10.0);
    assert_eq!(bits.key, "in_bits");
    assert_eq!(bits.kind, ValueKind::Counter);
    // A name without letters still gives a key.
    assert_eq!(
        one(&[("oid1", "1.3.6.1.2.1.1.3.0"), ("name1", "%%")]).key,
        "channel1"
    );
    assert_eq!(one(&[("oid1", "1.3.6.1.2.1.1.3.0")]).name, "Channel 1");
}

#[test]
fn keys_come_from_names() {
    assert_eq!(key_of("Sessions"), "sessions");
    assert_eq!(key_of("  RAID state / volume 1 "), "raid_state_volume_1");
    assert_eq!(key_of("5 min load"), "v5_min_load");
    assert_eq!(key_of("Températures"), "temp_ratures");
    assert!(key_of(&"a".repeat(50)).len() <= 32);
    assert!(valid_key("sessions"));
    assert!(!valid_key("raid_state"), "the state series would collide");
    assert!(!valid_key("Sessions"));
    assert!(!valid_key("1a"));
    assert!(!valid_key(""));
}

#[test]
fn oids_are_numeric() {
    assert_eq!(parse_oid("1.3.6"), Some(vec![1, 3, 6]));
    assert_eq!(parse_oid(" .1.3.6 "), Some(vec![1, 3, 6]));
    for bad in [
        "",
        "1",
        "1..3",
        "1.3.x",
        "sysUpTime.0",
        "3.1",
        "1.3.99999999999",
        "-1.3",
    ] {
        assert_eq!(parse_oid(bad), None, "{bad}");
    }
    assert_eq!(parse_oid(&vec!["1"; 129].join(".")), None);
    assert_eq!(oid_text(&[1, 3, 6, 1]), "1.3.6.1");
}

#[test]
fn bad_parameters_are_refused() {
    let base = [("host", "192.0.2.1"), ("community", "secret-community")];
    let cases: &[(&[(&str, &str)], &str)] = &[
        (&[], "at least one channel"),
        (&[("name1", "x")], "needs `oid1`"),
        (&[("oid1", "sysUpTime")], "numeric OID"),
        (&[("oid11", "1.3.6.1")], "at most 10 channels"),
        (&[("oid01", "1.3.6.1")], "unknown parameter"),
        (
            &[("oid1", "1.3.6.1"), ("what", "x")],
            "unknown parameter `what`",
        ),
        (&[("oid1", "1.3.6.1"), ("type1", "float")], "`type1`"),
        (&[("oid1", "1.3.6.1"), ("key1", "a_state")], "`key1`"),
        (
            &[
                ("oid1", "1.3.6.1"),
                ("oid2", "1.3.6.2"),
                ("key2", "channel_1"),
            ],
            "two channels",
        ),
        (&[("oid1", "1.3.6.1"), ("divide1", "0")], "`divide1`"),
        (&[("oid1", "1.3.6.1"), ("multiply1", "nan")], "`multiply1`"),
        (
            &[("oid1", "1.3.6.1"), ("type1", "text"), ("divide1", "2")],
            "no scaling",
        ),
        (
            &[("oid1", "1.3.6.1"), ("unit1", "a very long unit name")],
            "`unit1`",
        ),
        (
            &[
                ("oid1", "1.3.6.1"),
                ("match1", "contains"),
                ("expect1", "x"),
            ],
            "set `type1` to `text`",
        ),
        (
            &[("oid1", "1.3.6.1"), ("type1", "text"), ("expect1", "x")],
            "set `match1`",
        ),
        (
            &[("oid1", "1.3.6.1"), ("type1", "text"), ("match1", "like")],
            "`match1`",
        ),
        (
            &[
                ("oid1", "1.3.6.1"),
                ("type1", "text"),
                ("match1", "matches"),
                ("expect1", "("),
            ],
            "pattern",
        ),
        (
            &[
                ("oid1", "1.3.6.1"),
                ("type1", "text"),
                ("match1", "equals"),
                ("severity1", "ok"),
            ],
            "`severity1`",
        ),
        (
            &[("oid1", "1.3.6.1"), ("lookup1", "builtin:truth_value")],
            "was not sent",
        ),
        (
            &[("oid1", "1.3.6.1"), ("lookup1_map", "{")],
            "does not read",
        ),
        (
            &[("oid1", "1.3.6.1"), ("lookup1_map", r#"{"entries": []}"#)],
            "at least one entry",
        ),
        (
            &[
                ("oid1", "1.3.6.1"),
                ("type1", "counter"),
                (
                    "lookup1_map",
                    r#"{"entries": [{"value": 1, "name": "a", "state": "ok"}]}"#,
                ),
            ],
            "counter",
        ),
        (
            &[("name_column", "1.3.6.1.2"), ("oid1", "1.3.6.1.2")],
            "`name_column` is for a table",
        ),
        (
            &[("table", "1.3.6.1.4"), ("oid1", "1.3.6.1.5.1")],
            "column under `table`",
        ),
        (
            &[
                ("table", "1.3.6.1.4"),
                ("name_column", "1.3.6.2"),
                ("oid1", "1.3.6.1.4.1.2"),
            ],
            "`name_column`",
        ),
    ];
    for (pairs, error) in cases {
        let mut all = base.to_vec();
        all.extend_from_slice(pairs);
        let got = parse(&params(&all)).unwrap_err();
        assert!(got.contains(error), "{pairs:?}: {got}");
        assert!(!got.contains("secret-community"), "{got}");
    }
}

#[test]
fn lookups_are_checked_and_matched() {
    let lookup = lookup();
    lookup.validate().unwrap();
    assert_eq!(lookup.otherwise, State::Warning, "the default");
    let name = |v: Val| lookup.find(&v).map(|e| e.name.as_str());
    assert_eq!(name(Val::Int(3)), Some("Failed"));
    assert_eq!(name(Val::Uint(1)), Some("OK"));
    assert_eq!(name(Val::Int(15)), Some("Rebuilding"));
    assert_eq!(name(Val::Text("2".into())), Some("Warning"));
    assert_eq!(name(Val::Text("degraded".into())), Some("Degraded"));
    assert_eq!(name(Val::Int(7)), None);
    let bad = |json: &str| {
        serde_json::from_str::<Lookup>(json)
            .map_err(|e| e.to_string())
            .and_then(|l| l.validate())
            .unwrap_err()
    };
    assert!(
        bad(r#"{"entries": [{"value": 5, "max": 1, "name": "a", "state": "ok"}]}"#)
            .contains("ends before")
    );
    assert!(
        bad(r#"{"entries": [{"value": 5, "text": "a", "name": "a", "state": "ok"}]}"#)
            .contains("must have")
    );
    assert!(bad(r#"{"entries": [{"name": "a", "state": "ok"}]}"#).contains("must have"));
    assert!(bad(r#"{"entries": [{"value": 1, "name": "", "state": "ok"}]}"#).contains("empty"));
    assert!(
        bad(r#"{"entries": [{"value": 1, "name": "a", "state": "fine"}]}"#)
            .contains("unknown variant")
    );
    let many = (0..=MAX_LOOKUP_ENTRIES)
        .map(|i| format!(r#"{{"value": {i}, "name": "n", "state": "ok"}}"#))
        .collect::<Vec<_>>()
        .join(",");
    assert!(bad(&format!(r#"{{"entries": [{many}]}}"#)).contains("at most"));
}

#[test]
fn gauges_are_scaled() {
    let temp = one(&[
        ("oid1", "1.3.6.1.4.1.9.1"),
        ("name1", "Temp"),
        ("divide1", "10"),
    ]);
    let (samples, kept) = evaluate(&temp, &Val::Int(235), None, 0);
    assert_eq!(kept, None);
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].sensor, "temp");
    assert!((samples[0].value - 23.5).abs() < 1e-9);
    // Vendors write floats as text.
    let (samples, _) = evaluate(&temp, &Val::Text(" 41.0 ".into()), None, 0);
    assert!((samples[0].value - 4.1).abs() < 1e-9);
    let (samples, _) = evaluate(&temp, &Val::Text("n/a".into()), None, 0);
    assert!(samples.is_empty());
    let kib = one(&[("oid1", "1.3.6.1.4.1.2021.4.6.0"), ("multiply1", "1024")]);
    assert_eq!(evaluate(&kib, &Val::Uint(2), None, 0).0[0].value, 2048.0);
}

#[test]
fn counters_give_rates_and_skip_going_back() {
    let bits = one(&[
        ("oid1", "1.3.6.1.2.1.2.2.1.10.1"),
        ("name1", "In"),
        ("type1", "counter"),
        ("multiply1", "8"),
    ]);
    let (first, kept) = evaluate(&bits, &Val::Uint(1_000), None, 10_000);
    assert!(first.is_empty(), "no rate from one reading");
    assert_eq!(kept, Some((1_000, 10_000)));
    let (second, kept) = evaluate(&bits, &Val::Uint(3_000), kept, 20_000);
    assert_eq!(second[0].sensor, "in");
    assert_eq!(second[0].value, 1_600.0, "200 bytes a second, in bits");
    // A wrap or a reboot gives nothing this time, and restarts from it.
    let (third, kept) = evaluate(&bits, &Val::Uint(10), kept, 30_000);
    assert!(third.is_empty());
    assert_eq!(kept, Some((10, 30_000)));
    let (fourth, _) = evaluate(&bits, &Val::Uint(110), kept, 40_000);
    assert_eq!(fourth[0].value, 80.0);
    // Counter64 differences stay exact.
    let big = u64::MAX - 10;
    let (rate, _) = evaluate(&bits, &Val::Uint(big + 5), Some((big, 0)), 1_000);
    assert_eq!(rate[0].value, 40.0);
    // The same time twice gives no rate.
    let (none, _) = evaluate(&bits, &Val::Uint(5), Some((1, 1_000)), 1_000);
    assert!(none.is_empty());
}

#[test]
fn lookups_write_a_state_with_the_name() {
    let mut raid = one(&[("oid1", "1.3.6.1.4.1.6574.3.1.1.3.0"), ("name1", "RAID")]);
    raid.lookup = Some(lookup());
    let (samples, _) = evaluate(&raid, &Val::Int(3), None, 0);
    assert_eq!(samples.len(), 2);
    assert_eq!(
        (samples[0].sensor.as_str(), samples[0].value),
        ("raid", 3.0)
    );
    assert_eq!(samples[1].sensor, "raid_state");
    assert_eq!(samples[1].value, 2.0);
    assert_eq!(label(&samples[1], "text"), Some("Failed"));
    let (samples, _) = evaluate(&raid, &Val::Int(1), None, 0);
    assert_eq!(
        (samples[1].value, label(&samples[1], "text")),
        (0.0, Some("OK"))
    );
    // A value no entry names takes `otherwise`, shown as it is.
    let (samples, _) = evaluate(&raid, &Val::Int(42), None, 0);
    assert_eq!(
        (samples[1].value, label(&samples[1], "text")),
        (1.0, Some("42"))
    );
}

#[test]
fn texts_are_shown_and_checked() {
    let plain = one(&[
        ("oid1", "1.3.6.1.2.1.1.1.0"),
        ("name1", "Firmware"),
        ("type1", "text"),
    ]);
    let long = "x".repeat(100) + "\u{7}";
    let (samples, _) = evaluate(&plain, &Val::Text(long), None, 0);
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].sensor, "firmware_state");
    assert_eq!(samples[0].value, 0.0);
    assert_eq!(label(&samples[0], "text").unwrap().len(), MAX_TEXT_LABEL);
    let check = |mode: &str, expect: &str, text: &str| {
        let channel = one(&[
            ("oid1", "1.3.6.1.2.1.1.1.0"),
            ("type1", "text"),
            ("match1", mode),
            ("expect1", expect),
            ("severity1", "warning"),
        ]);
        evaluate(&channel, &Val::Text(text.into()), None, 0).0[0].value
    };
    assert_eq!(check("contains", "OK", "Status OK"), 0.0);
    assert_eq!(check("contains", "OK", "Status bad"), 1.0);
    assert_eq!(check("not_contains", "fail", "all fine"), 0.0);
    assert_eq!(check("not_contains", "fail", "disk failed"), 1.0);
    assert_eq!(check("equals", "", ""), 0.0);
    assert_eq!(check("equals", "on", "On"), 1.0);
    assert_eq!(check("not_equals", "off", "on"), 0.0);
    assert_eq!(check("matches", r"^v\d+\.\d+$", "v7.2"), 0.0);
    assert_eq!(check("matches", r"^v\d+\.\d+$", "beta"), 1.0);
    assert_eq!(check("not_matches", "(?i)error", "ERROR 5"), 1.0);
    // Critical by default.
    let channel = one(&[
        ("oid1", "1.3.6.1.2"),
        ("type1", "text"),
        ("match1", "equals"),
        ("expect1", "ok"),
    ]);
    assert_eq!(
        evaluate(&channel, &Val::Text("no".into()), None, 0).0[0].value,
        2.0
    );
    // A number read as text is compared in decimal.
    assert_eq!(
        evaluate(&channel, &Val::Int(1), None, 0).0[0].labels["text"],
        "1"
    );
}

#[test]
fn rows_are_named_by_their_column() {
    let indexes: BTreeSet<Vec<u32>> = [vec![1], vec![2], vec![3], vec![4, 1]].into();
    let names = BTreeMap::from([
        (vec![1], "Volume 1".to_string()),
        (vec![2], "Volume 1".to_string()),
        (vec![3], " \u{1}".to_string()),
    ]);
    let named = row_names(&indexes, &names);
    assert_eq!(named[&vec![1]], "Volume 1 (1)");
    assert_eq!(named[&vec![2]], "Volume 1 (2)");
    assert_eq!(named[&vec![3]], "3", "an empty name is the index");
    assert_eq!(named[&vec![4, 1]], "4.1");
    let many: BTreeSet<Vec<u32>> = (0..600).map(|i| vec![i]).collect();
    assert_eq!(row_names(&many, &BTreeMap::new()).len(), MAX_TABLE_ROWS);
}

#[test]
fn tables_take_columns_under_them() {
    let custom = parse(&params(&[
        ("host", "nas.lan"),
        ("community", "c"),
        ("table", "1.3.6.1.4.1.6574.3.1.1"),
        ("name_column", "1.3.6.1.4.1.6574.3.1.1.2"),
        ("oid1", "1.3.6.1.4.1.6574.3.1.1.3"),
        ("name1", "Status"),
    ]))
    .unwrap();
    let table = custom.table.unwrap();
    assert_eq!(table.oid, vec![1, 3, 6, 1, 4, 1, 6574, 3, 1, 1]);
    assert_eq!(
        table.name_column,
        Some(vec![1, 3, 6, 1, 4, 1, 6574, 3, 1, 1, 2])
    );
}

#[test]
fn octet_strings_are_text_or_hex() {
    assert_eq!(octets(b"Gi0/1"), "Gi0/1");
    assert_eq!(octets(b"name\0"), "name");
    assert_eq!(octets(&[0x00, 0x1a, 0xff]), "00:1a:ff");
    assert_eq!(octets(&[0xc3, 0x28]), "c3:28", "not UTF-8");
    assert_eq!(type_name(&Value::Counter64(1)), "Counter64");
    assert_eq!(reading(&Value::NoSuchObject), None);
    assert_eq!(
        reading(&Value::IpAddress([192, 0, 2, 1])),
        Some(Val::Text("192.0.2.1".into()))
    );
}

#[test]
fn builds_with_a_device() {
    assert!(build(&params(&[
        ("host", "192.0.2.1"),
        ("community", "c"),
        ("oid1", "1.3.6.1.2.1.1.3.0")
    ]))
    .is_ok());
    assert!(build(&params(&[
        ("community", "c"),
        ("oid1", "1.3.6.1.2.1.1.3.0")
    ]))
    .is_err());
}

#[test]
fn channel_parameters_are_numbered() {
    assert_eq!(channel_number("oid3"), Some((3, "oid")));
    assert_eq!(channel_number("lookup10_map"), Some((10, "lookup_map")));
    assert_eq!(channel_number("lookup2"), Some((2, "lookup")));
    assert_eq!(channel_number("oid_map1"), None);
    assert_eq!(channel_number("name1_map"), None);
    assert_eq!(channel_number("oid"), None);
    assert_eq!(channel_number("oid1x"), None);
    assert_eq!(channel_number("what1"), None);
}
