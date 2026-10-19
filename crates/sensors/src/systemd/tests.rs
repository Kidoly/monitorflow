use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use super::*;
use crate::registry;

/// Captured on Arch Linux, systemd 262: journald, `dbus.service` (an
/// alias of dbus-broker), a failed unit, a missing one and a timer.
const ARCH: &str = include_str!("../../tests/fixtures/systemd/show-arch-systemd262");
const ARCH_UNITS: [&str; 5] = [
    "systemd-journald.service",
    "dbus.service",
    "vmware.service",
    "nonexistent.service",
    "systemd-tmpfiles-clean.timer",
];

fn units(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| s.to_string()).collect()
}

/// Answers with a fixed result and records the units it was asked.
struct Fake {
    answer: Result<String, SensorError>,
    asked: Mutex<Vec<Vec<String>>>,
}

impl Fake {
    fn new(answer: Result<&str, SensorError>) -> Self {
        Self {
            answer: answer.map(str::to_owned),
            asked: Mutex::default(),
        }
    }
}

#[async_trait::async_trait]
impl Systemctl for std::sync::Arc<Fake> {
    async fn show(&self, units: &[String], _deadline: Instant) -> Result<String, SensorError> {
        self.asked.lock().unwrap().push(units.to_vec());
        self.answer.clone()
    }
}

fn soon() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

#[test]
fn unit_names_are_checked_and_get_a_suffix() {
    for (given, expected) in [
        ("ssh", "ssh.service"),
        ("ssh.service", "ssh.service"),
        ("logrotate.timer", "logrotate.timer"),
        ("getty@tty1", "getty@tty1.service"),
        ("dev-sda1.device", "dev-sda1.device"),
        (
            "systemd-fsck@dev-disk-by\\x2duuid-1.service",
            "systemd-fsck@dev-disk-by\\x2duuid-1.service",
        ),
        ("my.app", "my.app.service"),
    ] {
        assert_eq!(unit_name(given).as_deref(), Ok(expected), "{given}");
    }
    let long = "x".repeat(UNIT_NAME_MAX);
    for bad in [
        "",
        ".service",
        "-ssh",
        "--all",
        "ssh service",
        "ssh;reboot",
        "ssh*",
        "a/b",
        "getty@.service",
        "@x.service",
        "é",
        "ssh\n",
        long.as_str(),
    ] {
        assert!(unit_name(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn parses_the_captured_output() {
    let states = parse_show(ARCH, ARCH_UNITS.len()).unwrap();
    assert_eq!(
        states[0],
        UnitState {
            load_state: "loaded".into(),
            active_state: "active".into(),
            sub_state: "running".into(),
            restarts: Some(0),
            state_change_us: Some(6_610_123_047),
        }
    );
    assert_eq!(states[2].active_state, "failed");
    assert_eq!(states[3].load_state, "not-found");
    assert_eq!(states[3].state_change_us, None);
    // Timers have no NRestarts.
    assert_eq!(states[4].restarts, None);
    assert_eq!(states[4].sub_state, "waiting");
}

#[test]
fn odd_output_is_a_transient_error() {
    for (text, count) in [
        (ARCH, 4),
        ("", 1),
        ("LoadState=loaded\n", 1),
        ("Id=a.service\nnonsense\n", 1),
        ("Id=a.service\nNRestarts=many\n", 1),
    ] {
        let error = parse_show(text, count).unwrap_err();
        assert!(!error.is_permanent(), "{text:?}");
    }
    // Unknown properties and empty values are fine.
    let states = parse_show("Id=a.service\nNew=x\nNRestarts=\n\n", 1).unwrap();
    assert_eq!(states[0].restarts, None);
}

fn values(samples: &[Sample]) -> BTreeMap<(String, String), f64> {
    samples
        .iter()
        .map(|s| ((s.instance.clone(), s.sensor.clone()), s.value))
        .collect()
}

#[tokio::test]
async fn reports_each_unit_by_instance() {
    let fake = std::sync::Arc::new(Fake::new(Ok(ARCH)));
    let sensor = SystemdSensor::new(units(&ARCH_UNITS), fake.clone(), || 6_620_123_047);
    let samples = sensor.collect(soon()).await.unwrap();
    assert_eq!(fake.asked.lock().unwrap()[0], units(&ARCH_UNITS));
    assert!(samples.iter().all(|s| s.labels.is_empty()));
    let v = values(&samples);
    let get = |unit: &str, metric: &str| v.get(&(unit.to_owned(), metric.to_owned())).copied();
    assert_eq!(get("systemd-journald.service", "active"), Some(1.0));
    assert_eq!(get("systemd-journald.service", "failed"), Some(0.0));
    assert_eq!(get("systemd-journald.service", "restarts"), Some(0.0));
    assert_eq!(
        get("systemd-journald.service", "state_age_seconds"),
        Some(10.0)
    );
    // Reported under the name asked, not the alias's Id.
    assert_eq!(get("dbus.service", "active"), Some(1.0));
    assert_eq!(get("vmware.service", "active"), Some(0.0));
    assert_eq!(get("vmware.service", "failed"), Some(1.0));
    // A missing unit is data, not an error, so it can alert.
    assert_eq!(get("nonexistent.service", "loaded"), Some(0.0));
    assert_eq!(get("nonexistent.service", "active"), Some(0.0));
    assert_eq!(get("nonexistent.service", "state_age_seconds"), None);
    assert_eq!(get("systemd-tmpfiles-clean.timer", "restarts"), None);
    // 3 flags each, restarts but for the timer, ages but for the missing
    // unit and dbus, which changed after this fake clock's now.
    assert_eq!(samples.len(), 3 * 5 + 4 + 3);
    for sample in &samples {
        crate::validate::sample(sample).unwrap();
        crate::validate::sensor_name(&crate::metric_name("systemd", &sample.sensor)).unwrap();
    }
}

#[test]
fn reloading_counts_as_active_and_a_clock_behind_hides_the_age() {
    let state = UnitState {
        load_state: "loaded".into(),
        active_state: "reloading".into(),
        state_change_us: Some(2_000_000),
        ..UnitState::default()
    };
    let v = values(&state.samples("nginx.service", 1_000_000));
    assert_eq!(v[&("nginx.service".into(), "active".into())], 1.0);
    assert!(!v.contains_key(&("nginx.service".into(), "state_age_seconds".into())));
}

#[tokio::test]
async fn runner_errors_are_passed_on() {
    for error in [
        SensorError::Permanent("not booted".into()),
        SensorError::Transient("slow".into()),
    ] {
        let fake = std::sync::Arc::new(Fake::new(Err(error.clone())));
        let sensor = SystemdSensor::new(units(&["a.service"]), fake, || 0);
        assert_eq!(sensor.collect(soon()).await, Err(error));
    }
}

#[test]
fn params_are_checked() {
    let build = |units: &str| {
        let params = BTreeMap::from([("units".to_owned(), units.to_owned())]);
        registry::build("systemd", &params).map(|_| ())
    };
    assert_eq!(build("ssh, cron.service ,logrotate.timer"), Ok(()));
    for (bad, reason) in [
        ("ssh,ssh.service", "twice"),
        ("ssh,", "empty"),
        ("-a", "not a unit name"),
        ("a b", "not a unit name"),
    ] {
        let error = build(bad).unwrap_err().to_string();
        assert!(error.contains(reason), "{bad}: {error}");
    }
    let many: Vec<String> = (0..=MAX_UNITS).map(|i| format!("u{i}")).collect();
    assert!(build(&many.join(","))
        .unwrap_err()
        .to_string()
        .contains("at most"));
    let error = registry::build("systemd", &BTreeMap::new()).err().unwrap();
    assert!(error.to_string().contains("`units` is required"), "{error}");
}

/// A temporary directory with an executable script, standing in for
/// `systemctl` in tests of [`Command`]. Production never runs a shell.
struct FakeBinary {
    dir: tempfile::TempDir,
}

impl FakeBinary {
    fn new(script: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("systemctl");
        crate::testing::write_executable(&path, &format!("#!/bin/sh\n{script}\n"), 0o755);
        std::fs::create_dir(dir.path().join("booted")).unwrap();
        Self { dir }
    }

    fn command(&self) -> Command {
        Command::new(
            self.dir.path().join("systemctl"),
            self.dir.path().join("booted"),
        )
    }
}

#[tokio::test]
async fn command_passes_arguments_without_a_shell() {
    // Prints its arguments one per line: a shell would have split or
    // expanded them.
    let fake = FakeBinary::new(r#"for a in "$@"; do echo "[$a]"; done; echo "[$LC_ALL]""#);
    let out = fake
        .command()
        .show(&units(&["a b.service", "$(x)"]), soon())
        .await
        .unwrap();
    let expected = format!(
        "[show]\n[--no-pager]\n[--property]\n[{PROPERTIES}]\n[--]\n[a b.service]\n[$(x)]\n[C]\n"
    );
    assert_eq!(out, expected);
}

#[tokio::test]
async fn command_failures() {
    let fake = FakeBinary::new("echo 'Failed to connect to bus' >&2; exit 1");
    let error = fake
        .command()
        .show(&units(&["a"]), soon())
        .await
        .unwrap_err();
    assert!(!error.is_permanent());
    assert!(
        error.to_string().contains("Failed to connect to bus"),
        "{error}"
    );

    let missing = Command::new(fake.dir.path().join("nope"), fake.dir.path().join("booted"));
    let error = missing.show(&units(&["a"]), soon()).await.unwrap_err();
    assert!(error.is_permanent(), "{error}");

    let not_booted = Command::new(
        fake.dir.path().join("systemctl"),
        fake.dir.path().join("nope"),
    );
    let error = not_booted.show(&units(&["a"]), soon()).await.unwrap_err();
    assert_eq!(
        error,
        SensorError::Permanent("this host was not booted with systemd".into())
    );
}

#[tokio::test]
async fn command_is_bounded_by_the_deadline() {
    let fake = FakeBinary::new("exec sleep 30");
    let started = std::time::Instant::now();
    let deadline = Instant::now() + Duration::from_millis(200);
    let error = fake
        .command()
        .show(&units(&["a"]), deadline)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("in time"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// Runs against the host's systemd when `MONITORFLOW_TEST_SYSTEMD=1`.
#[tokio::test]
async fn real_systemd_when_asked() {
    if std::env::var("MONITORFLOW_TEST_SYSTEMD").as_deref() != Ok("1") {
        return;
    }
    let names = [
        "systemd-journald.service",
        "monitorflow-does-not-exist.service",
    ];
    let sensor = SystemdSensor::new(units(&names), Command::default(), monotonic_us);
    let v = values(&sensor.collect(soon()).await.unwrap());
    assert_eq!(v[&(names[0].into(), "active".into())], 1.0);
    assert!(v[&(names[0].into(), "state_age_seconds".into())] > 0.0);
    assert_eq!(v[&(names[1].into(), "loaded".into())], 0.0);
}
