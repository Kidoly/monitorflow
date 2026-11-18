use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use monitorflow_sensors::testing::FakeSensor;

use super::*;

/// Builds fakes for kind `fake`, counting what it built, and refuses
/// anything else; kind `boom` panics.
#[derive(Default)]
struct Fakes {
    built: AtomicUsize,
    sensors: Mutex<Vec<Arc<FakeSensor>>>,
}

impl ProbeFactory for Fakes {
    fn build(&self, kind: &str, _: &BTreeMap<String, String>) -> Result<Arc<dyn Sensor>, String> {
        match kind {
            "fake" => {
                self.built.fetch_add(1, Ordering::SeqCst);
                let sensor = Arc::new(FakeSensor::new());
                self.sensors.lock().unwrap().push(sensor.clone());
                Ok(sensor)
            }
            "boom" => panic!("boom"),
            other => Err(format!("no `{other}`")),
        }
    }
}

fn check(device: &str, kind: &str, name: &str, instance: &str) -> Check {
    let mut config = SensorConfig::new(name, Duration::from_secs(1));
    config.instance = instance.into();
    Check {
        device: device.into(),
        kind: kind.into(),
        config,
    }
}

fn options() -> SchedulerOptions {
    SchedulerOptions {
        jitter: false,
        ..SchedulerOptions::default()
    }
}

#[tokio::test(start_paused = true)]
async fn samples_come_tagged_with_their_device() {
    let factory = Arc::new(Fakes::default());
    let (mut runner, mut rx) = Runner::with_options(factory.clone(), options());
    let errors = runner
        .apply(vec![
            check("nas", "fake", "ping", "central"),
            check("switch", "fake", "ping", "central"),
            check("switch", "fake", "port_22", "central"),
        ])
        .await;
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(runner.devices(), 2);
    let mut seen = BTreeMap::new();
    for _ in 0..3 {
        let s = rx.recv().await.unwrap();
        assert_eq!(s.sample.instance, "central");
        *seen.entry((s.device, s.sample.sensor)).or_insert(0) += 1;
    }
    assert_eq!(
        seen.keys().cloned().collect::<Vec<_>>(),
        [
            ("nas".to_string(), "ping".to_string()),
            ("switch".into(), "ping".into()),
            ("switch".into(), "port_22".into())
        ]
    );
    runner.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn unchanged_devices_keep_running_and_others_restart() {
    let factory = Arc::new(Fakes::default());
    let (mut runner, _rx) = Runner::with_options(factory.clone(), options());
    let both = vec![
        check("a", "fake", "ping", ""),
        check("b", "fake", "ping", ""),
    ];
    runner.apply(both.clone()).await;
    assert_eq!(factory.built.load(Ordering::SeqCst), 2);
    // Same checks in another order: nothing rebuilt.
    runner.apply(both.into_iter().rev().collect()).await;
    assert_eq!(factory.built.load(Ordering::SeqCst), 2);
    // `b` changes, `a` stays, `c` is new.
    let mut changed = check("b", "fake", "ping", "");
    changed.config.interval = Duration::from_secs(5);
    runner
        .apply(vec![
            check("a", "fake", "ping", ""),
            changed,
            check("c", "fake", "ping", ""),
        ])
        .await;
    assert_eq!(factory.built.load(Ordering::SeqCst), 4);
    runner.apply(vec![]).await;
    assert_eq!(runner.devices(), 0);
}

#[tokio::test(start_paused = true)]
async fn bad_checks_are_reported_and_the_others_run() {
    let (mut runner, mut rx) = Runner::with_options(Arc::new(Fakes::default()), options());
    let mut invalid = check("a", "fake", "Bad Name", "");
    invalid.config.name = "Bad Name".into();
    let errors = runner
        .apply(vec![
            check("a", "nope", "x", ""),
            check("a", "boom", "y", ""),
            invalid,
            check("a", "fake", "ok", ""),
            check("a", "fake", "ok", ""),
        ])
        .await;
    let messages: Vec<_> = errors
        .iter()
        .map(|e| (e.name.as_str(), e.message.as_str()))
        .collect();
    assert_eq!(errors.len(), 4, "{messages:?}");
    assert!(messages.contains(&("x", "no `nope`")));
    assert!(messages
        .iter()
        .any(|(n, m)| *n == "y" && m.contains("unexpectedly")));
    assert!(messages.iter().any(|(n, _)| *n == "Bad Name"));
    assert!(
        messages.iter().any(|(n, m)| *n == "ok" && m.contains("ok")),
        "duplicate"
    );
    assert_eq!(rx.recv().await.unwrap().sample.sensor, "ok");
}

#[test]
fn builtin_runs_network_kinds_only() {
    let ping = BTreeMap::from([("host".to_string(), "192.0.2.1".to_string())]);
    assert!(Builtin.build("ping", &ping).is_ok());
    let udp = BTreeMap::from([
        ("port".to_string(), "53".to_string()),
        ("protocol".to_string(), "udp".to_string()),
    ]);
    assert!(Builtin.build("port", &udp).is_ok());
    assert!(Builtin
        .build("cpu", &BTreeMap::new())
        .err()
        .unwrap()
        .contains("does not run"));
    assert!(Builtin.build("ping", &BTreeMap::new()).is_err());
    let snmp = BTreeMap::from([
        ("host".to_string(), "192.0.2.1".to_string()),
        ("community".to_string(), "public".to_string()),
        ("what".to_string(), "interfaces".to_string()),
    ]);
    assert!(Builtin.build("snmp", &snmp).is_ok());
    assert!(Builtin
        .build("snmp", &BTreeMap::new())
        .err()
        .unwrap()
        .contains("`host`"));
}

#[test]
fn checks_and_results_convert_to_the_protocol() {
    let mut original = check("nas", "ping", "ping", "central");
    original
        .config
        .params
        .insert("host".into(), "192.0.2.1".into());
    original.config.timeout = Some(Duration::from_secs(3));
    assert_eq!(Check::from_proto(original.to_proto()).unwrap(), original);
    original.config.timeout = None;
    assert_eq!(Check::from_proto(original.to_proto()).unwrap(), original);
    assert!(Check::from_proto(proto::ProbeCheck::default()).is_none());

    let sample = |device: &str, value| DeviceSample {
        device: device.into(),
        sample: Sample::metric("ping_up", value).with_label("target", "x"),
    };
    let results = to_results(vec![sample("a", 1.0), sample("b", 0.0), sample("a", 0.5)]);
    assert_eq!(results.devices.len(), 2);
    assert_eq!(results.devices[0].device, "a");
    assert_eq!(results.devices[0].samples.len(), 2);
    assert_eq!(results.devices[0].samples[1].value, 0.5);
    assert_eq!(results.devices[1].samples[0].labels["target"], "x");
}

#[tokio::test]
async fn batches_take_what_is_queued() {
    let (tx, mut rx) = mpsc::channel(10);
    for i in 0..3 {
        tx.send(DeviceSample {
            device: "a".into(),
            sample: Sample::now(f64::from(i)),
        })
        .await
        .unwrap();
    }
    assert_eq!(next_batch(&mut rx).await.unwrap().len(), 3);
    drop(tx);
    assert!(next_batch(&mut rx).await.is_none());
}
