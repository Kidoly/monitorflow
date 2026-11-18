//! The SNMP checks against snmpsim playing the recordings of
//! `deploy/probe-test/snmp/data` (ADR-0149): a switch, a UPS and a
//! printer, each its own community. Skipped unless `MF_SNMPSIM` names
//! the simulator as `host:port`; `deploy/probe-test/run-e2e.sh` starts one.

use std::collections::BTreeMap;
use std::time::Duration;

use monitorflow_sensors::{Sample, Sensor};
use tokio::time::Instant;

fn sensor(device: &str, what: &str) -> Option<std::sync::Arc<dyn Sensor>> {
    let addr = std::env::var("MF_SNMPSIM").ok()?;
    let (host, port) = addr.rsplit_once(':')?;
    let params: BTreeMap<String, String> = [
        ("host", host),
        ("port", port),
        ("community", device),
        ("what", what),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    Some(monitorflow_snmp::build(&params).unwrap())
}

async fn collect(sensor: &dyn Sensor) -> Vec<Sample> {
    sensor
        .collect(Instant::now() + Duration::from_secs(10))
        .await
        .unwrap()
}

fn value(samples: &[Sample], metric: &str, instance: &str) -> f64 {
    samples
        .iter()
        .find(|s| s.sensor == metric && s.instance == instance)
        .unwrap_or_else(|| panic!("no {metric} {instance} in {samples:?}"))
        .value
}

fn has(samples: &[Sample]) -> BTreeMap<String, f64> {
    samples
        .iter()
        .filter(|s| s.sensor == "has")
        .map(|s| (s.instance.clone(), s.value))
        .collect()
}

#[tokio::test]
async fn a_switch_reports_its_ports_traffic_and_temperature() {
    let Some(ifs) = sensor("switch", "interfaces") else {
        eprintln!("MF_SNMPSIM is not set; skipped");
        return;
    };
    let first = collect(&*ifs).await;
    assert_eq!(first.iter().filter(|s| s.sensor == "oper_up").count(), 24);
    assert_eq!(value(&first, "oper_up", "Gi0/1"), 1.0);
    assert_eq!(value(&first, "oper_up", "Gi0/12"), 0.0);
    assert_eq!(value(&first, "speed_bits_per_second", "Gi0/1"), 1e9);
    tokio::time::sleep(Duration::from_secs(3)).await;
    let second = collect(&*ifs).await;
    let rate = value(&second, "in_bytes_per_second", "Gi0/1");
    assert!((100_000.0..150_000.0).contains(&rate), "{rate}");
    assert!((0.0008..0.0012).contains(&value(&second, "in_utilization_ratio", "Gi0/1")));
    let errors = value(&second, "in_errors_per_second", "Gi0/1");
    assert!((1.0..3.0).contains(&errors), "{errors}");
    assert_eq!(value(&second, "in_bytes_per_second", "Gi0/2"), 0.0);

    let temperature = collect(&*sensor("switch", "temperature").unwrap()).await;
    assert_eq!(value(&temperature, "celsius", "Chassis"), 42.0);
    let detected = collect(&*sensor("switch", "detect").unwrap()).await;
    let found = has(&detected);
    assert_eq!(
        (
            found["interfaces"],
            found["temperature"],
            found["ups"],
            found["printer"]
        ),
        (1.0, 1.0, 0.0, 0.0)
    );
    assert_eq!(detected[0].labels["name"], "floor-switch");
}

#[tokio::test]
async fn a_ups_and_a_printer() {
    let Some(ups) = sensor("ups", "ups") else {
        return;
    };
    let samples = collect(&*ups).await;
    assert_eq!(value(&samples, "battery_charge_ratio", ""), 1.0);
    assert_eq!(value(&samples, "runtime_seconds", ""), 2700.0);
    assert_eq!(value(&samples, "on_battery", ""), 0.0);
    assert_eq!(value(&samples, "battery_low", ""), 0.0);
    let found = has(&collect(&*sensor("ups", "detect").unwrap()).await);
    assert_eq!(
        (found["ups"], found["interfaces"], found["printer"]),
        (1.0, 0.0, 0.0)
    );

    let supplies = collect(&*sensor("printer", "printer").unwrap()).await;
    assert_eq!(value(&supplies, "supply_level_ratio", "Black Toner"), 0.06);
    assert_eq!(value(&supplies, "supply_level_ratio", "Cyan Toner"), 0.7);
    assert!(
        !supplies.iter().any(|s| s.instance == "Waste Toner Box"),
        "no level to report"
    );
    let found = has(&collect(&*sensor("printer", "detect").unwrap()).await);
    assert_eq!((found["printer"], found["ups"]), (1.0, 0.0));
}
