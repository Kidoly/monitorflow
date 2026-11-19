//! Custom SNMP checks (ADR-0157) against real agents: snmpsim playing the
//! recordings of `deploy/snmpx-test/snmp/data` (a firewall and a NAS),
//! and net-snmp's snmpd with the table of `deploy/snmpx-test/snmp`.
//! Skipped unless `MF_SNMPX_SIM` and `MF_SNMPX_SNMPD` name them as
//! `host:port`; `deploy/snmpx-test/run-e2e.sh` starts both.

use std::collections::BTreeMap;
use std::time::Duration;

use monitorflow_sensors::{Sample, Sensor};
use tokio::time::Instant;

fn agent(var: &str) -> Option<(String, String)> {
    let addr = std::env::var(var).ok()?;
    let (host, port) = addr.rsplit_once(':')?;
    Some((host.to_owned(), port.to_owned()))
}

fn build(var: &str, kind: &str, pairs: &[(&str, &str)]) -> Option<std::sync::Arc<dyn Sensor>> {
    let (host, port) = agent(var)?;
    let mut params: BTreeMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    params.insert("host".into(), host);
    params.insert("port".into(), port);
    Some(match kind {
        "read" => monitorflow_snmp::build_read(&params).unwrap(),
        _ => monitorflow_snmp::custom::build(&params).unwrap(),
    })
}

async fn collect(sensor: &dyn Sensor) -> Vec<Sample> {
    sensor
        .collect(Instant::now() + Duration::from_secs(10))
        .await
        .unwrap()
}

fn find<'a>(samples: &'a [Sample], metric: &str, instance: &str) -> &'a Sample {
    samples
        .iter()
        .find(|s| s.sensor == metric && s.instance == instance)
        .unwrap_or_else(|| panic!("no {metric} {instance:?} in {samples:?}"))
}

#[tokio::test]
async fn a_firewall_reports_gauges_rates_texts_and_states() {
    let lookup = r#"{"entries": [{"value": 1, "name": "Master", "state": "ok"},
        {"value": 2, "name": "Backup", "state": "ok"},
        {"value": 3, "name": "Failed", "state": "critical"}]}"#;
    let Some(sensor) = build(
        "MF_SNMPX_SIM",
        "custom",
        &[
            ("community", "firewall"),
            ("oid1", "1.3.6.1.4.1.12356.101.4.1.8.0"),
            ("name1", "Sessions"),
            ("oid2", "1.3.6.1.2.1.2.2.1.10.1"),
            ("name2", "In"),
            ("type2", "counter"),
            ("multiply2", "8"),
            ("oid3", "1.3.6.1.4.1.12356.101.4.1.1.0"),
            ("name3", "Firmware"),
            ("type3", "text"),
            ("match3", "matches"),
            ("expect3", r"^v7\."),
            ("oid4", "1.3.6.1.4.1.12356.101.13.2.1.1.15.1"),
            ("name4", "HA"),
            ("lookup4_map", lookup),
            ("oid5", "1.3.6.1.4.1.12356.101.99.0"),
            ("name5", "Missing"),
        ],
    ) else {
        eprintln!("MF_SNMPX_SIM is not set; skipped");
        return;
    };
    let first = collect(&*sensor).await;
    let sessions = find(&first, "sessions", "").value;
    assert!((900.0..=2100.0).contains(&sessions), "{sessions}");
    assert!(
        first.iter().all(|s| s.sensor != "in"),
        "no rate from one reading"
    );
    let firmware = find(&first, "firmware_state", "");
    assert_eq!(
        (firmware.value, firmware.labels["text"].as_str()),
        (0.0, "v7.2.5")
    );
    let ha = find(&first, "ha_state", "");
    assert_eq!((ha.value, ha.labels["text"].as_str()), (0.0, "Master"));
    assert_eq!(find(&first, "ha", "").value, 1.0);
    assert!(first.iter().all(|s| !s.sensor.starts_with("missing")));
    tokio::time::sleep(Duration::from_secs(2)).await;
    let second = collect(&*sensor).await;
    // 20 MB a second in bits, unless the counter wrapped in between.
    if let Some(rate) = second.iter().find(|s| s.sensor == "in") {
        assert!((1.0e8..=2.5e8).contains(&rate.value), "{}", rate.value);
    }
}

#[tokio::test]
async fn a_nas_table_has_a_row_per_volume_and_scaled_values() {
    let Some(table) = build(
        "MF_SNMPX_SIM",
        "custom",
        &[
            ("community", "nas"),
            ("table", "1.3.6.1.4.1.6574.3.1.1"),
            ("name_column", "1.3.6.1.4.1.6574.3.1.1.2"),
            ("oid1", "1.3.6.1.4.1.6574.3.1.1.3"),
            ("name1", "RAID"),
            (
                "lookup1_map",
                r#"{"entries": [{"value": 1, "name": "Normal", "state": "ok"}, {"value": 12, "name": "Crashed", "state": "critical"}]}"#,
            ),
        ],
    ) else {
        eprintln!("MF_SNMPX_SIM is not set; skipped");
        return;
    };
    let rows = collect(&*table).await;
    assert_eq!(
        find(&rows, "raid_state", "Volume 1").labels["text"],
        "Normal"
    );
    assert_eq!(find(&rows, "raid_state", "Volume 2").value, 0.0);
    let scalars = build(
        "MF_SNMPX_SIM",
        "custom",
        &[
            ("community", "nas"),
            ("oid1", "1.3.6.1.4.1.6574.1.2.0"),
            ("name1", "Temperature"),
            ("divide1", "10"),
            ("unit1", "°C"),
        ],
    )
    .unwrap();
    assert_eq!(
        find(&collect(&*scalars).await, "temperature", "").value,
        41.5
    );
}

#[tokio::test]
async fn reads_and_walks_say_types_and_values() {
    let Some(read) = build(
        "MF_SNMPX_SIM",
        "read",
        &[
            ("community", "firewall"),
            (
                "oids",
                "1.3.6.1.2.1.1.5.0,1.3.6.1.2.1.2.2.1.10.1,1.3.6.1.4.1.12356.1.0",
            ),
        ],
    ) else {
        eprintln!("MF_SNMPX_SIM is not set; skipped");
        return;
    };
    let got = collect(&*read).await;
    let name = find(&got, "object", "1.3.6.1.2.1.1.5.0");
    assert_eq!(
        (name.labels["type"].as_str(), name.labels["value"].as_str()),
        ("OctetString", "fw-edge")
    );
    assert_eq!(
        find(&got, "object", "1.3.6.1.2.1.2.2.1.10.1").labels["type"],
        "Counter32"
    );
    assert!(find(&got, "object", "1.3.6.1.4.1.12356.1.0").labels["type"].starts_with("noSuch"));
    let walk = build(
        "MF_SNMPX_SIM",
        "read",
        &[
            ("community", "firewall"),
            ("walk", "1.3.6.1.4.1"),
            ("max", "3"),
        ],
    )
    .unwrap();
    let got = collect(&*walk).await;
    assert_eq!(got.iter().filter(|s| s.sensor == "object").count(), 3);
    assert_eq!(find(&got, "cut", "").value, 1.0);
}

#[tokio::test]
async fn net_snmp_rows_appear_and_go_over_every_version() {
    let Some((host, _)) = agent("MF_SNMPX_SNMPD") else {
        eprintln!("MF_SNMPX_SNMPD is not set; skipped");
        return;
    };
    let table = [
        ("table", "1.3.6.1.4.1.8072.9999.9999.1.1"),
        ("name_column", "1.3.6.1.4.1.8072.9999.9999.1.1.2"),
        ("oid1", "1.3.6.1.4.1.8072.9999.9999.1.1.3"),
        ("name1", "State"),
        ("oid2", "1.3.6.1.4.1.8072.9999.9999.1.1.4"),
        ("name2", "Count"),
    ];
    let versions: [&[(&str, &str)]; 3] = [
        &[("version", "v1"), ("community", "e2e-community")],
        &[("community", "e2e-community")],
        &[
            ("version", "v3"),
            ("user", "e2e-sha256"),
            ("auth_protocol", "sha256"),
            ("auth_password", "e2e-auth-password"),
            ("priv_protocol", "aes128"),
            ("priv_password", "e2e-priv-password"),
        ],
    ];
    for credentials in versions {
        let mut pairs = table.to_vec();
        pairs.extend_from_slice(credentials);
        let sensor = build("MF_SNMPX_SNMPD", "custom", &pairs).unwrap();
        let rows = collect(&*sensor).await;
        assert!(
            rows.iter()
                .any(|s| s.sensor == "state" && s.instance == "backup-nightly"),
            "{host} {credentials:?}: {rows:?}"
        );
    }
    // Rows the file adds or drops are followed: `MF_SNMPX_ROWS` names the
    // file the agent reads, which the test may rewrite.
    let Ok(file) = std::env::var("MF_SNMPX_ROWS") else {
        return;
    };
    let mut pairs = table.to_vec();
    pairs.extend_from_slice(versions[1]);
    let sensor = build("MF_SNMPX_SNMPD", "custom", &pairs).unwrap();
    let before = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, "2|backup-weekly|1|200\n3|archive|2|7\n").unwrap();
    let rows = collect(&*sensor).await;
    std::fs::write(&file, before).unwrap();
    let names: std::collections::BTreeSet<&str> = rows
        .iter()
        .filter(|s| s.sensor == "state")
        .map(|s| s.instance.as_str())
        .collect();
    assert_eq!(names, ["archive", "backup-weekly"].into());
}
