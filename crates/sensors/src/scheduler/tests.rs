//! Scheduler tests on tokio's paused clock: sleeping advances time at once
//! when every task is idle, so intervals of seconds run instantly.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::sleep;

use super::*;
use crate::testing::{FakeSensor, Step};

const SEC: Duration = Duration::from_secs(1);

fn config(name: &str, interval_secs: u32) -> SensorConfig {
    SensorConfig::new(name, interval_secs * SEC)
}

fn no_jitter(capacity: usize) -> SchedulerOptions {
    SchedulerOptions {
        capacity,
        jitter: false,
    }
}

/// Starts one scheduler with `sensors`.
fn start(
    capacity: usize,
    sensors: Vec<(SensorConfig, Arc<FakeSensor>)>,
) -> (SchedulerHandle, mpsc::Receiver<Sample>) {
    let mut scheduler = Scheduler::new(no_jitter(capacity));
    for (config, sensor) in sensors {
        scheduler.add(config, sensor).unwrap();
    }
    scheduler.start()
}

fn drain(rx: &mut mpsc::Receiver<Sample>) -> Vec<Sample> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

#[tokio::test(start_paused = true)]
async fn interval_is_respected() {
    let sensor = Arc::new(FakeSensor::new());
    let (handle, mut rx) = start(16, vec![(config("cpu", 10), sensor.clone())]);
    // Ticks at 0, 10, 20 and 30 s.
    sleep(35 * SEC).await;
    assert_eq!(sensor.calls(), 4);
    let samples = drain(&mut rx);
    let values: Vec<f64> = samples.iter().map(|s| s.value).collect();
    assert_eq!(values, [0.0, 1.0, 2.0, 3.0]);
    assert!(samples.iter().all(|s| s.sensor == "cpu"));
    assert_eq!(handle.stats().collected, 4);
    handle.shutdown().await;
}

#[test]
fn jitter_stays_below_the_interval_and_varies() {
    let interval = 10 * SEC;
    let delays: Vec<Duration> = (0..100).map(|_| jitter(interval)).collect();
    assert!(delays.iter().all(|d| *d < interval));
    assert!(
        delays.iter().any(|d| *d != delays[0]),
        "jitter is not random"
    );
    assert_eq!(jitter(Duration::ZERO), Duration::ZERO);
}

#[tokio::test(start_paused = true)]
async fn jittered_first_tick_comes_within_one_interval() {
    let sensor = Arc::new(FakeSensor::new());
    let mut scheduler = Scheduler::new(SchedulerOptions::default());
    scheduler.add(config("cpu", 10), sensor.clone()).unwrap();
    let (handle, _rx) = scheduler.start();
    sleep(10 * SEC - Duration::from_millis(1)).await;
    // Wrong only if the random delay lands in the last millisecond.
    assert_eq!(sensor.calls(), 1);
    handle.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn timeout_is_enforced_and_runs_never_overlap() {
    let sensor = Arc::new(FakeSensor::new().with_delay(60 * SEC));
    let mut slow = config("slow", 10);
    slow.timeout = Some(10 * SEC);
    let (handle, mut rx) = start(16, vec![(slow, sensor.clone())]);
    // Each run times out exactly when the next tick is due.
    sleep(95 * SEC).await;
    assert_eq!(sensor.calls(), 10);
    assert_eq!(sensor.max_running(), 1);
    let stats = handle.stats();
    assert_eq!((stats.timed_out, stats.collected), (9, 0));
    assert!(drain(&mut rx).is_empty());
    handle.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn default_timeout_cuts_a_short_interval() {
    let sensor = Arc::new(FakeSensor::new().with_delay(3 * SEC));
    let (handle, _rx) = start(16, vec![(config("quick", 2), sensor.clone())]);
    sleep(SEC).await;
    assert_eq!(handle.stats().timed_out, 0);
    sleep(2 * SEC).await;
    assert_eq!(
        handle.stats().timed_out,
        1,
        "timeout is capped by the 2 s interval"
    );
    handle.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn missed_ticks_are_skipped_not_bursted() {
    let sensor = Arc::new(FakeSensor::new());
    let (handle, _rx) = start(16, vec![(config("cpu", 10), sensor.clone())]);
    tokio::task::yield_now().await;
    sleep(Duration::from_millis(1)).await;
    assert_eq!(sensor.calls(), 1);
    // Jump the clock without letting the sensor task run in between, as if
    // the runtime had stalled for 35 s: ticks at 10, 20 and 30 s are missed.
    tokio::time::advance(35 * SEC).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert_eq!(sensor.calls(), 2, "one late tick, not three");
    sleep(4 * SEC).await;
    assert_eq!(sensor.calls(), 2);
    sleep(2 * SEC).await;
    assert_eq!(sensor.calls(), 3, "back on the 10 s grid at 40 s");
    handle.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn a_failing_sensor_does_not_affect_the_others() {
    let failing =
        Arc::new(FakeSensor::new().always(Step::Fail(SensorError::Transient("busy".into()))));
    let panicking = Arc::new(FakeSensor::new().always(Step::Panic));
    let permanent =
        Arc::new(FakeSensor::new().always(Step::Fail(SensorError::Permanent("no /proc".into()))));
    let healthy = Arc::new(FakeSensor::new());
    let (handle, mut rx) = start(
        16,
        vec![
            (config("failing", 10), failing.clone()),
            (config("panicking", 10), panicking.clone()),
            (config("permanent", 10), permanent.clone()),
            (config("healthy", 10), healthy.clone()),
        ],
    );
    sleep(25 * SEC).await;
    assert_eq!(
        (failing.calls(), panicking.calls(), healthy.calls()),
        (3, 3, 3)
    );
    assert_eq!(permanent.calls(), 1, "a permanent failure stops the sensor");
    let samples = drain(&mut rx);
    assert_eq!(samples.len(), 3);
    assert!(samples.iter().all(|s| s.sensor == "healthy"));
    let stats = handle.stats();
    assert_eq!((stats.failed, stats.panicked, stats.collected), (4, 3, 3));
    handle.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn a_sensor_recovers_after_transient_failures() {
    let error = Step::Fail(SensorError::Transient("busy".into()));
    let sensor = Arc::new(
        FakeSensor::new()
            .on_call(0, error.clone())
            .on_call(1, Step::Panic),
    );
    let (handle, mut rx) = start(16, vec![(config("flaky", 10), sensor.clone())]);
    sleep(25 * SEC).await;
    assert_eq!(drain(&mut rx).len(), 1);
    assert_eq!(handle.stats().collected, 1);
    handle.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn full_channel_drops_new_samples_and_counts_them() {
    let five = (0..5).map(|i| Sample::now(f64::from(i))).collect();
    let sensor = Arc::new(FakeSensor::new().always(Step::Samples(five)));
    let (handle, mut rx) = start(2, vec![(config("burst", 10), sensor.clone())]);
    sleep(SEC).await;
    assert_eq!(handle.stats().dropped, 3);
    let kept: Vec<f64> = drain(&mut rx).iter().map(|s| s.value).collect();
    assert_eq!(kept, [0.0, 1.0], "the oldest samples are kept");
    sleep(10 * SEC).await;
    assert_eq!(handle.stats().dropped, 6);
    handle.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn samples_are_named_defaulted_and_validated() {
    let samples = vec![
        Sample::now(1.0).with_label("ok", "yes"),
        Sample::now(2.0).with_instance("/var"),
        Sample::now(f64::NAN),
        Sample::now(3.0).with_label("__name__", "evil"),
    ];
    let mut lying = samples[0].clone();
    lying.sensor = "not_me".into();
    let sensor = Arc::new(FakeSensor::new().always(Step::Samples([samples, vec![lying]].concat())));
    let mut port = config("port", 10);
    port.instance = "22".into();
    let (handle, mut rx) = start(16, vec![(port, sensor)]);
    sleep(SEC).await;
    let got = drain(&mut rx);
    let summary: Vec<(&str, &str, f64)> = got
        .iter()
        .map(|s| (s.sensor.as_str(), s.instance.as_str(), s.value))
        .collect();
    assert_eq!(
        summary,
        [
            ("port", "22", 1.0),
            ("port", "/var", 2.0),
            ("port", "22", 1.0)
        ]
    );
    assert_eq!(handle.stats().invalid, 2);
    handle.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn shutdown_cancels_running_collections_and_closes_the_channel() {
    let slow = Arc::new(
        FakeSensor::new()
            .with_delay(30 * SEC)
            .on_call(1, Step::Samples(vec![])),
    );
    let mut slow_config = config("slow", 60);
    slow_config.timeout = Some(60 * SEC);
    let quick = Arc::new(FakeSensor::new());
    let (handle, mut rx) = start(
        16,
        vec![(slow_config, slow.clone()), (config("quick", 10), quick)],
    );
    sleep(5 * SEC).await;
    assert_eq!(slow.running(), 1);
    // On the paused clock, a shutdown that waited for the 30 s collection
    // would trip this 1 s timeout first.
    tokio::time::timeout(SEC, handle.shutdown())
        .await
        .expect("shutdown is prompt");
    assert_eq!(slow.running(), 0, "the collection was cancelled");
    assert_eq!(
        drain(&mut rx).len(),
        1,
        "samples sent before the shutdown stay readable"
    );
    assert!(
        rx.recv().await.is_none(),
        "the channel closes once every task ended"
    );
}

#[tokio::test(start_paused = true)]
async fn closed_receiver_stops_the_sensors() {
    let sensor = Arc::new(FakeSensor::new());
    let (handle, rx) = start(16, vec![(config("cpu", 10), sensor.clone())]);
    drop(rx);
    sleep(25 * SEC).await;
    assert_eq!(
        sensor.calls(),
        1,
        "the first send failed and the task ended"
    );
    tokio::time::timeout(SEC, handle.shutdown()).await.unwrap();
}

#[test]
fn bad_and_duplicate_configurations_are_refused() {
    let mut scheduler = Scheduler::new(SchedulerOptions::default());
    let sensor = Arc::new(FakeSensor::new());
    assert!(scheduler.is_empty());
    scheduler.add(config("port", 10), sensor.clone()).unwrap();
    let mut other = config("port", 10);
    other.instance = "443".into();
    scheduler.add(other.clone(), sensor.clone()).unwrap();
    assert_eq!(
        scheduler.add(other, sensor.clone()),
        Err(ValidationError::Duplicate {
            name: "port".into(),
            instance: "443".into()
        })
    );
    assert!(matches!(
        scheduler.add(config("Port", 10), sensor.clone()),
        Err(ValidationError::SensorName(_))
    ));
    assert!(matches!(
        scheduler.add(config("port", 0), sensor),
        Err(ValidationError::Interval(_))
    ));
    assert_eq!(scheduler.len(), 2);
}

#[tokio::test(start_paused = true)]
async fn limiter_allows_one_line_per_period() {
    let mut limiter = Limiter::default();
    assert_eq!(limiter.allow(), Some(0));
    assert_eq!(limiter.allow(), None);
    assert_eq!(limiter.allow(), None);
    tokio::time::advance(LOG_EVERY).await;
    assert_eq!(limiter.allow(), Some(2));
    assert_eq!(limiter.allow(), None);
}
