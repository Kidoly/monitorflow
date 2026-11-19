use std::collections::BTreeMap;

use super::mibs::{self, Counters, Group, Val};
use super::*;

fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn v2c_and_v3_parameters() {
    let v2c = parse(&params(&[
        ("host", "192.0.2.10"),
        ("community", "public"),
        ("what", "interfaces"),
    ]))
    .unwrap();
    assert_eq!(v2c.port, 161);
    assert_eq!(v2c.group, Group::Interfaces);
    assert_eq!(
        v2c.credentials,
        Credentials::V2c {
            community: "public".into()
        }
    );
    let v3 = parse(&params(&[
        ("host", "switch.lan"),
        ("port", "1161"),
        ("version", "v3"),
        ("user", "monitor"),
        ("auth_protocol", "sha256"),
        ("auth_password", "authpass123"),
        ("priv_protocol", "aes128"),
        ("priv_password", "privpass123"),
    ]))
    .unwrap();
    assert_eq!(v3.port, 1161);
    assert_eq!(v3.group, Group::System);
    let Credentials::V3 { auth, privacy, .. } = &v3.credentials else {
        panic!()
    };
    assert_eq!(auth.as_ref().unwrap().0, AuthProtocol::Sha256);
    assert_eq!(privacy.as_ref().unwrap().0, PrivProtocol::Aes128);
    // Secrets never show in debug output.
    let shown = format!("{v2c:?} {v3:?}");
    assert!(
        !shown.contains("public") && !shown.contains("pass123"),
        "{shown}"
    );
}

#[test]
fn bad_parameters_are_refused_without_quoting_secrets() {
    let cases: [(&[(&str, &str)], &str); 9] = [
        (&[("community", "c")], "`host` is required"),
        (&[("host", "a b"), ("community", "c")], "`host`"),
        (&[("host", "x")], "needs a `community`"),
        (
            &[("host", "x"), ("community", "c"), ("what", "gpu")],
            "`what` must be one of",
        ),
        (
            &[("host", "x"), ("community", "c"), ("port", "0")],
            "`port`",
        ),
        (&[("host", "x"), ("version", "v3")], "needs a `user`"),
        (
            &[
                ("host", "x"),
                ("version", "v3"),
                ("user", "u"),
                ("auth_protocol", "sha"),
                ("auth_password", "short"),
            ],
            "at least 8",
        ),
        (
            &[
                ("host", "x"),
                ("version", "v3"),
                ("user", "u"),
                ("priv_protocol", "aes128"),
                ("priv_password", "secret-priv"),
            ],
            "needs authentication",
        ),
        (
            &[("host", "x"), ("community", "c"), ("timeout", "1")],
            "unknown parameter",
        ),
    ];
    for (pairs, want) in cases {
        let error = parse(&params(pairs)).unwrap_err();
        assert!(error.contains(want), "{pairs:?}: {error}");
        assert!(
            !error.contains("secret-priv") && !error.contains("short\""),
            "{error}"
        );
    }
    // SNMPv1 takes a community, like v2c (ADR-0157).
    assert_eq!(
        parse(&params(&[
            ("host", "x"),
            ("version", "v1"),
            ("community", "c")
        ]))
        .unwrap()
        .credentials,
        Credentials::V1 {
            community: "c".into()
        }
    );
    assert!(parse(&params(&[("host", "x"), ("version", "v1")]))
        .unwrap_err()
        .contains("needs a `community`"));
    assert!(parse(&params(&[
        ("host", "x"),
        ("version", "v4"),
        ("community", "c")
    ]))
    .unwrap_err()
    .contains("`v1`, `v2c` or `v3`"));
}

fn ifs(rows: &[(u32, u32, Val)]) -> mibs::Table {
    mibs::table(
        mibs::IF_TABLE,
        rows.iter()
            .map(|(col, idx, v)| ([mibs::IF_TABLE, &[*col, *idx]].concat(), v.clone())),
    )
}

fn ifx(rows: &[(u32, u32, Val)]) -> mibs::Table {
    mibs::table(
        mibs::IFX_TABLE,
        rows.iter()
            .map(|(col, idx, v)| ([mibs::IFX_TABLE, &[*col, *idx]].concat(), v.clone())),
    )
}

fn values(samples: &[monitorflow_sensors::Sample]) -> BTreeMap<(String, String), f64> {
    samples
        .iter()
        .map(|s| ((s.sensor.clone(), s.instance.clone()), s.value))
        .collect()
}

#[test]
fn interfaces_report_state_and_rates_from_64_bit_counters() {
    let t = |s: &str| Val::Text(s.into());
    let n = Val::Uint;
    let if_table = ifs(&[
        (2, 1, t("lo")),
        (3, 1, Val::Int(24)),
        (2, 2, t("GigabitEthernet0/1")),
        (3, 2, Val::Int(6)),
        (7, 2, Val::Int(1)),
        (8, 2, Val::Int(1)),
        (14, 2, n(10)),
        (20, 2, n(0)),
        (13, 2, n(5)),
        (19, 2, n(1)),
        (2, 3, t("Gi0/2")),
        (7, 3, Val::Int(1)),
        (8, 3, Val::Int(2)),
    ]);
    let first_x = ifx(&[
        (1, 2, t("Gi0/1")),
        (6, 2, n(1_000_000)),
        (10, 2, n(2_000_000)),
        (15, 2, n(1000)),
    ]);
    let (samples, kept) = mibs::interfaces(&if_table, &first_x, &BTreeMap::new(), 1_000);
    let v = values(&samples);
    assert_eq!(v[&("oper_up".into(), "Gi0/1".into())], 1.0);
    assert_eq!(
        v[&("oper_up".into(), "Gi0/2".into())],
        0.0,
        "down, named by ifDescr without ifName"
    );
    assert_eq!(v[&("speed_bits_per_second".into(), "Gi0/1".into())], 1e9);
    assert!(!v.keys().any(|(_, i)| i == "lo"), "loopbacks are left out");
    assert!(
        !v.contains_key(&("in_bytes_per_second".into(), "Gi0/1".into())),
        "no rate at first"
    );
    // Ten seconds later, 1.25 MB in: 125 kB/s, 1 Mbit/s of 1 Gbit/s.
    let if_table2 = ifs(&[
        (2, 2, t("GigabitEthernet0/1")),
        (3, 2, Val::Int(6)),
        (7, 2, Val::Int(1)),
        (8, 2, Val::Int(1)),
        (14, 2, n(30)),
        (20, 2, n(0)),
        (13, 2, n(5)),
        (19, 2, n(1)),
    ]);
    let second_x = ifx(&[
        (1, 2, t("Gi0/1")),
        (6, 2, n(2_250_000)),
        (10, 2, n(2_000_000)),
        (15, 2, n(1000)),
    ]);
    let (samples, _) = mibs::interfaces(&if_table2, &second_x, &kept, 11_000);
    let v = values(&samples);
    let gi = |m: &str| v[&(m.to_string(), "Gi0/1".to_string())];
    assert_eq!(gi("in_bytes_per_second"), 125_000.0);
    assert_eq!(gi("out_bytes_per_second"), 0.0);
    assert_eq!(gi("in_errors_per_second"), 2.0);
    assert_eq!(gi("in_discards_per_second"), 0.0);
    assert!((gi("in_utilization_ratio") - 0.001).abs() < 1e-12);
    // A counter that goes back gives no rate.
    let back = Counters {
        at_ms: 1_000,
        values: [Some(9e9), None, None, None, None, None],
        wide: true,
    };
    let (samples, _) = mibs::interfaces(
        &if_table2,
        &second_x,
        &BTreeMap::from([("Gi0/1".to_string(), back)]),
        11_000,
    );
    assert!(!values(&samples).contains_key(&("in_bytes_per_second".into(), "Gi0/1".into())));
}

#[test]
fn host_resources_give_processor_memory_and_disks() {
    let storage = mibs::table(
        mibs::HR_STORAGE,
        [
            (vec![2, 1], Val::Oid(vec![1, 3, 6, 1, 2, 1, 25, 2, 1, 2])),
            (vec![3, 1], Val::Text("Physical memory".into())),
            (vec![4, 1], Val::Int(1024)),
            (vec![5, 1], Val::Int(1000)),
            (vec![6, 1], Val::Int(250)),
            (vec![2, 31], Val::Oid(vec![1, 3, 6, 1, 2, 1, 25, 2, 1, 4])),
            (vec![3, 31], Val::Text("/".into())),
            (vec![4, 31], Val::Int(4096)),
            (vec![5, 31], Val::Int(100)),
            (vec![6, 31], Val::Int(90)),
            (vec![2, 32], Val::Oid(vec![1, 3, 6, 1, 2, 1, 25, 2, 1, 4])),
            (vec![3, 32], Val::Text("/etc/hosts".into())),
            (vec![4, 32], Val::Int(4096)),
            (vec![5, 32], Val::Int(100)),
            (vec![6, 32], Val::Int(90)),
            (vec![2, 33], Val::Oid(vec![1, 3, 6, 1, 2, 1, 25, 2, 1, 4])),
            (vec![3, 33], Val::Text("/proc/kcore".into())),
            (vec![4, 33], Val::Int(4096)),
            (vec![5, 33], Val::Int(7)),
            (vec![6, 33], Val::Int(0)),
        ]
        .into_iter()
        .map(|(rest, v)| ([mibs::HR_STORAGE, &rest[..]].concat(), v)),
    );
    let v = values(&mibs::host(&[10.0, 30.0], &storage));
    assert_eq!(v[&("cpu_usage_ratio".into(), String::new())], 0.2);
    assert_eq!(v[&("memory_used_ratio".into(), String::new())], 0.25);
    assert_eq!(v[&("storage_used_ratio".into(), "/".into())], 0.9);
    assert_eq!(v[&("storage_size_bytes".into(), "/".into())], 409_600.0);
    assert!(
        !v.keys()
            .any(|(_, i)| i == "/etc/hosts" || i == "/proc/kcore"),
        "{v:?}"
    );
}

#[test]
fn ups_temperature_and_printer_supplies() {
    let ups = BTreeMap::from([
        ("2.1.0".to_string(), Val::Int(3)),
        ("2.3.0".to_string(), Val::Int(12)),
        ("2.4.0".to_string(), Val::Int(64)),
        ("4.1.0".to_string(), Val::Int(5)),
    ]);
    let v = values(&mibs::ups(&ups));
    assert_eq!(v[&("battery_charge_ratio".into(), String::new())], 0.64);
    assert_eq!(v[&("runtime_seconds".into(), String::new())], 720.0);
    assert_eq!(v[&("on_battery".into(), String::new())], 1.0);
    assert_eq!(v[&("battery_low".into(), String::new())], 1.0);

    let sensors = mibs::table(
        mibs::ENT_SENSOR,
        [(1, 8), (2, 9), (3, 1), (4, 415), (5, 1), (1, 10), (4, 3)]
            .iter()
            .enumerate()
            .map(|(i, (col, v))| {
                let idx = if i < 5 { 7 } else { 8 };
                ([mibs::ENT_SENSOR, &[*col, idx]].concat(), Val::Int(*v))
            }),
    );
    let names = BTreeMap::from([("7".to_string(), Val::Text("CPU board".into()))]);
    let t = mibs::temperature(&sensors, &names);
    assert_eq!(t.len(), 1, "only celsius sensors");
    assert_eq!((t[0].instance.as_str(), t[0].value), ("CPU board", 41.5));

    let supplies = mibs::table(
        mibs::PRT_SUPPLIES,
        [
            (vec![6, 1, 1], Val::Text("Black Toner".into())),
            (vec![8, 1, 1], Val::Int(2000)),
            (vec![9, 1, 1], Val::Int(500)),
            (vec![6, 1, 2], Val::Text("Waste".into())),
            (vec![8, 1, 2], Val::Int(-2)),
            (vec![9, 1, 2], Val::Int(-3)),
        ]
        .into_iter()
        .map(|(rest, v)| ([mibs::PRT_SUPPLIES, &rest[..]].concat(), v)),
    );
    let p = mibs::printer(&supplies);
    assert_eq!(p.len(), 1);
    assert_eq!((p[0].instance.as_str(), p[0].value), ("Black Toner", 0.25));
}

#[test]
fn detection_names_what_answers() {
    let samples = mibs::detect(
        "Linux nas 6.1",
        "nas",
        "1.3.6.1.4.1.8072.3.2.10",
        &[(Group::Host, true), (Group::Ups, false)],
    );
    assert_eq!(samples[0].labels["descr"], "Linux nas 6.1");
    assert_eq!(samples[1].instance, "host");
    assert_eq!(samples[2].value, 0.0);
    for sample in &samples {
        monitorflow_sensors::validate::sample(sample).unwrap();
    }
    assert_eq!(
        mibs::instance_name(&format!(" a\u{7}b{} ", "x".repeat(300))).len(),
        256
    );
}

/// No answer from a closed port: the system check reports `up 0`, the
/// others fail for the scheduler to log.
#[tokio::test]
async fn an_unreachable_device_is_down() {
    let port = std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let deadline = || tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    let port_text = port.to_string();
    let system = build(&params(&[
        ("host", "127.0.0.1"),
        ("port", &port_text),
        ("community", "public"),
    ]))
    .unwrap();
    let samples = system.collect(deadline()).await.unwrap();
    assert_eq!((samples[0].sensor.as_str(), samples[0].value), ("up", 0.0));
    let ifs = build(&params(&[
        ("host", "127.0.0.1"),
        ("port", &port_text),
        ("community", "public"),
        ("what", "interfaces"),
    ]))
    .unwrap();
    assert!(ifs.collect(deadline()).await.is_err());
}
