//! Runs the sensors the server configured (ADR-0016) until shutdown.
//!
//! At start-up the last configuration received is read from `data_dir`
//! and started, so the agent monitors before, or without, any upstream.
//! Each configuration from the stream is built into a new [`Scheduler`]
//! that replaces the running one (ADR-0012), saved, and acknowledged with
//! the specs that could not run. A configuration with the same specs as
//! the running one only updates the revision: the sensors keep their
//! schedule.
//!
//! Specs refused by this host or stopped by a permanent error are tried
//! again every [`RETRY_EVERY`] (ADR-0024): each is built and collected
//! once, and the ones that now work start in a scheduler of their own,
//! so the running sensors keep their schedule and state. A new
//! acknowledgement with the same revision is sent whenever the set of
//! specs that do not run, or their errors, changes.
//!
//! Samples of every scheduler go to the same [`Uplink`], which the stream
//! empties in batches.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use monitorflow_common::proto::v1::{SensorConfig, SensorConfigAck, SensorSpecError};
use monitorflow_sensors::{self as sensors, Scheduler, SchedulerHandle, SchedulerOptions, Sensor};
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{interval_at, timeout_at, Instant, MissedTickBehavior};

use crate::sensor_config::{
    self, ConfigFile, Down, DownKind, Failure, Failures, SensorFactory, SensorsEnd,
};
use crate::shutdown::Shutdown;
use crate::upload::{self, Uplink};

/// How often specs refused by this host or stopped by a permanent error
/// are tried again (ADR-0024). A script installed by hand starts within
/// five minutes, and one that keeps failing costs a build and one
/// collection per period, without a log line.
pub const RETRY_EVERY: Duration = Duration::from_secs(5 * 60);

/// The sensors task.
pub struct Sensors {
    factory: Arc<dyn SensorFactory>,
    file: ConfigFile,
    uplink: Uplink,
    options: SchedulerOptions,
}

/// The configuration in force and its schedulers.
struct Running {
    config: SensorConfig,
    /// Specs not running, in the order they stopped.
    down: Vec<Down>,
    /// The configuration's scheduler, then one per retry that started some.
    schedulers: Vec<(SchedulerHandle, JoinHandle<u64>)>,
    failures: Failures,
    failed: mpsc::UnboundedReceiver<Failure>,
    /// Errors of the last acknowledgement sent.
    acked: Vec<SensorSpecError>,
}

impl Running {
    fn errors(&self) -> Vec<SensorSpecError> {
        self.down.iter().map(Down::error).collect()
    }

    /// The acknowledgement to send if the errors changed since the last one.
    fn ack_if_changed(&mut self) -> Option<SensorConfigAck> {
        let errors = self.errors();
        if errors == self.acked {
            return None;
        }
        self.acked.clone_from(&errors);
        Some(SensorConfigAck {
            revision: self.config.revision,
            errors,
        })
    }

    /// Records a running sensor stopped by a permanent error. The
    /// scheduler has logged it.
    fn failed(&mut self, failure: &Failure) {
        let spec = self
            .config
            .sensors
            .iter()
            .find(|s| s.name == failure.name && s.instance == failure.instance);
        if let Some(spec) = spec {
            self.down.push(Down::failed(spec, &failure.message));
        }
    }

    /// Stops the sensors and lets the consumers finish the queued samples.
    async fn stop(self) {
        let mut stats = Vec::with_capacity(self.schedulers.len());
        for (handle, consumer) in self.schedulers {
            stats.push(handle.stats());
            handle.shutdown().await;
            // The channel closed with the last sensor task, so this ends.
            let _ = consumer.await;
        }
        tracing::info!(revision = self.config.revision, ?stats, "sensors stopped");
    }
}

/// What a retry found for one spec that was down.
enum Retried {
    /// It built and its collection did not fail permanently.
    Up(sensors::SensorConfig, Arc<dyn Sensor>),
    Down(Down),
}

impl Sensors {
    pub fn new(factory: Arc<dyn SensorFactory>, file: ConfigFile, uplink: Uplink) -> Self {
        Self {
            factory,
            file,
            uplink,
            options: SchedulerOptions::default(),
        }
    }

    pub fn with_options(mut self, options: SchedulerOptions) -> Self {
        self.options = options;
        self
    }

    /// Runs until `stop` fires.
    pub async fn run(self, mut end: SensorsEnd, mut stop: Shutdown) {
        let mut running = None;
        if let Some(saved) = self.file.load().await {
            tracing::info!(
                revision = saved.revision,
                "starting the last sensor configuration received"
            );
            running = Some(self.start(saved));
        }
        let mut retry = interval_at(Instant::now() + RETRY_EVERY, RETRY_EVERY);
        retry.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            let ack = tokio::select! {
                () = stop.wait() => break,
                changed = end.configs.changed() => {
                    if changed.is_err() {
                        // No stream any more: keep running until shutdown.
                        stop.wait().await;
                        break;
                    }
                    let Some(config) = end.configs.borrow_and_update().clone() else {
                        continue;
                    };
                    let mut applied = self.apply(running.take(), config).await;
                    // Always sent: the server records the revision.
                    applied.acked = applied.errors();
                    let ack = SensorConfigAck {
                        revision: applied.config.revision,
                        errors: applied.acked.clone(),
                    };
                    running = Some(applied);
                    ack
                }
                Some(failure) = next_failure(&mut running) => {
                    let Some(current) = running.as_mut() else { continue };
                    current.failed(&failure);
                    let Some(ack) = current.ack_if_changed() else { continue };
                    ack
                }
                _ = retry.tick() => {
                    let Some(current) = running.as_mut() else { continue };
                    if !current.down.iter().any(Down::retryable) {
                        continue;
                    }
                    let retried = tokio::select! {
                        () = stop.wait() => break,
                        retried = self.retry(&current.down) => retried,
                    };
                    self.restart(current, retried);
                    let Some(ack) = current.ack_if_changed() else { continue };
                    ack
                }
            };
            if end.acks.try_send(ack).is_err() {
                tracing::debug!("acknowledgement queue full; dropped");
            }
        }
        if let Some(running) = running {
            running.stop().await;
        }
    }

    /// Builds `config`, then replaces `current` with it and saves it.
    async fn apply(&self, current: Option<Running>, config: SensorConfig) -> Running {
        let running = match current {
            Some(mut current) if current.config.sensors == config.sensors => {
                tracing::debug!(revision = config.revision, "sensor configuration unchanged");
                current.config.revision = config.revision;
                current
            }
            current => {
                if let Some(current) = current {
                    current.stop().await;
                }
                self.start(config)
            }
        };
        if let Err(error) = self.file.save(&running.config).await {
            tracing::warn!(%error, "cannot save the sensor configuration; it will not survive a restart");
        }
        running
    }

    fn start(&self, config: SensorConfig) -> Running {
        let (failures, failed) = mpsc::unbounded_channel();
        let (scheduler, down) = sensor_config::build(
            &config,
            self.factory.as_ref(),
            self.options.clone(),
            &failures,
        );
        for down in &down {
            tracing::warn!(
                sensor = %down.spec.name,
                instance = %down.spec.instance,
                error = %down.message,
                "sensor not started"
            );
        }
        let count = scheduler.len();
        let mut running = Running {
            config,
            down,
            schedulers: Vec::new(),
            failures,
            failed,
            acked: Vec::new(),
        };
        self.launch(&mut running, scheduler);
        tracing::info!(
            revision = running.config.revision,
            sensors = count,
            refused = running.down.len(),
            "sensors started"
        );
        running
    }

    fn launch(&self, running: &mut Running, scheduler: Scheduler) {
        let (handle, samples) = scheduler.start();
        let consumer = tokio::spawn(upload::collect(samples, self.uplink.clone()));
        running.schedulers.push((handle, consumer));
    }

    /// Builds and collects once each retryable spec of `down`, in parallel,
    /// each within its collection timeout. Collected samples are dropped:
    /// the scheduler sends the next ones.
    async fn retry(&self, down: &[Down]) -> Vec<(usize, Retried)> {
        let mut results = Vec::new();
        let mut probes = JoinSet::new();
        let mut started = HashMap::new();
        for (index, current) in down.iter().enumerate() {
            if !current.retryable() {
                continue;
            }
            let config = sensor_config::sensor_config(&current.spec);
            let sensor = match self.factory.build(&current.spec.kind, &config) {
                Ok(sensor) => sensor,
                Err(error) => {
                    results.push((index, Retried::Down(Down::refused(&current.spec, error))));
                    continue;
                }
            };
            let deadline = Instant::now() + config.timeout();
            let probe = sensor.clone();
            let id = probes
                .spawn(async move { timeout_at(deadline, probe.collect(deadline)).await })
                .id();
            started.insert(id, (index, config, sensor));
        }
        while let Some(joined) = probes.join_next_with_id().await {
            let (id, permanent) = match joined {
                Ok((id, Ok(Err(error)))) if error.is_permanent() => (id, Some(error.to_string())),
                Ok((id, _)) => (id, None),
                // A panic does not stop a running sensor either.
                Err(error) => (error.id(), None),
            };
            let Some((index, config, sensor)) = started.remove(&id) else {
                continue;
            };
            let retried = match permanent {
                Some(message) => Retried::Down(Down::failed(&down[index].spec, &message)),
                None => Retried::Up(config, sensor),
            };
            results.push((index, retried));
        }
        results
    }

    /// Starts the specs that came back up and updates why the others are
    /// down, logging only changes of state.
    fn restart(&self, running: &mut Running, retried: Vec<(usize, Retried)>) {
        let mut scheduler = Scheduler::new(self.options.clone());
        let mut up = Vec::new();
        for (index, retried) in retried {
            let before = &running.down[index];
            let (name, instance) = (&before.spec.name, &before.spec.instance);
            match retried {
                Retried::Up(config, sensor) => {
                    let sensor = sensor_config::watch(sensor, &config, &running.failures);
                    match scheduler.add(config, sensor) {
                        Ok(()) => {
                            tracing::info!(sensor = %name, instance = %instance, "sensor started after a retry");
                            up.push(index);
                        }
                        Err(error) => {
                            let spec = &before.spec;
                            running.down[index] = Down {
                                spec: spec.clone(),
                                kind: DownKind::Invalid,
                                message: error.to_string(),
                            };
                        }
                    }
                }
                Retried::Down(after) => {
                    if after.kind != before.kind {
                        tracing::warn!(sensor = %name, instance = %instance, error = %after.message, "sensor still not running");
                    }
                    running.down[index] = after;
                }
            }
        }
        if up.is_empty() {
            return;
        }
        let mut index = 0;
        running.down.retain(|_| {
            index += 1;
            !up.contains(&(index - 1))
        });
        self.launch(running, scheduler);
    }
}

/// The next permanent failure of the running sensors; never without any.
async fn next_failure(running: &mut Option<Running>) -> Option<Failure> {
    match running {
        Some(running) => running.failed.recv().await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use monitorflow_common::proto::v1::SensorSpec;
    use monitorflow_sensors::testing::{FakeSensor, Step};
    use monitorflow_sensors::{SensorConfig as Config, SensorError};

    use super::*;
    use crate::sensor_config::{link, FactoryError, StreamEnd};
    use crate::shutdown::{self, Trigger};
    use crate::upload::UploadSettings;

    /// Builds `fake` sensors and keeps them, to count their calls. While
    /// their name is `broken`, `refusable` specs are refused by the host
    /// and `fragile` sensors fail permanently.
    #[derive(Default)]
    struct Fakes {
        built: AtomicUsize,
        attempts: Mutex<HashMap<String, usize>>,
        broken: Mutex<HashSet<String>>,
        sensors: Mutex<Vec<(String, Arc<FakeSensor>)>>,
    }

    impl Fakes {
        fn calls(&self, name: &str) -> usize {
            let sensors = self.sensors.lock().unwrap();
            sensors
                .iter()
                .filter(|(n, _)| n == name)
                .map(|(_, s)| s.calls())
                .sum()
        }

        fn attempts(&self, name: &str) -> usize {
            self.attempts
                .lock()
                .unwrap()
                .get(name)
                .copied()
                .unwrap_or(0)
        }

        fn set_broken(&self, name: &str, broken: bool) {
            let mut names = self.broken.lock().unwrap();
            if broken {
                names.insert(name.into());
            } else {
                names.remove(name);
            }
        }
    }

    impl SensorFactory for Fakes {
        fn build(&self, kind: &str, config: &Config) -> Result<Arc<dyn Sensor>, FactoryError> {
            *self
                .attempts
                .lock()
                .unwrap()
                .entry(config.name.clone())
                .or_default() += 1;
            let broken = self.broken.lock().unwrap().contains(&config.name);
            let sensor = match kind {
                "fake" => FakeSensor::new(),
                "refusable" if broken => return Err(FactoryError::Refused("not installed".into())),
                "refusable" => FakeSensor::new(),
                "fragile" if broken => {
                    FakeSensor::new().always(Step::Fail(SensorError::Permanent("broken".into())))
                }
                "fragile" => FakeSensor::new(),
                _ => return Err(FactoryError::UnknownKind(kind.into())),
            };
            self.built.fetch_add(1, Ordering::SeqCst);
            let sensor = Arc::new(sensor);
            let mut sensors = self.sensors.lock().unwrap();
            sensors.push((config.name.clone(), sensor.clone()));
            Ok(sensor)
        }
    }

    fn config(revision: u64, specs: &[(&str, &str)]) -> SensorConfig {
        SensorConfig {
            revision,
            sensors: specs
                .iter()
                .map(|(kind, name)| SensorSpec {
                    kind: kind.to_string(),
                    name: name.to_string(),
                    interval_ms: 10_000,
                    ..SensorSpec::default()
                })
                .collect(),
        }
    }

    struct Harness {
        fakes: Arc<Fakes>,
        uplink: Uplink,
        stream: StreamEnd,
        trigger: Trigger,
        task: JoinHandle<()>,
    }

    fn start(dir: &std::path::Path) -> Harness {
        let fakes = Arc::new(Fakes::default());
        let uplink = Uplink::new(UploadSettings::default());
        let sensors = Sensors::new(fakes.clone(), ConfigFile::in_dir(dir), uplink.clone())
            .with_options(SchedulerOptions {
                jitter: false,
                ..SchedulerOptions::default()
            });
        let (stream, end) = link();
        let (trigger, stop) = shutdown::channel();
        let task = tokio::spawn(sensors.run(end, stop));
        Harness {
            fakes,
            uplink,
            stream,
            trigger,
            task,
        }
    }

    impl Harness {
        async fn push(&mut self, config: SensorConfig) -> SensorConfigAck {
            self.stream.configs.send_replace(Some(config));
            self.stream.acks.recv().await.unwrap()
        }

        async fn stop(self) {
            self.trigger.fire();
            self.task.await.unwrap();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn configurations_are_applied_acknowledged_and_saved() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = start(dir.path());
        let ack = h.push(config(1, &[("fake", "one"), ("cpu", "cpu")])).await;
        assert_eq!(ack.revision, 1);
        assert_eq!(ack.errors.len(), 1);
        assert_eq!(ack.errors[0].name, "cpu");
        tokio::time::sleep(Duration::from_secs(25)).await;
        assert_eq!(h.fakes.calls("one"), 3);
        let batch = h.uplink.take();
        assert_eq!(batch.len(), 3);
        assert!(batch.iter().all(|s| s.sensor == "one"));

        // Same specs, new revision: acknowledged, saved, not rebuilt.
        let ack = h.push(config(2, &[("fake", "one"), ("cpu", "cpu")])).await;
        assert_eq!((ack.revision, ack.errors.len()), (2, 1));
        assert_eq!(h.fakes.built.load(Ordering::SeqCst), 1);
        let file = ConfigFile::in_dir(dir.path());
        assert_eq!(file.load().await.unwrap().revision, 2);

        // New specs replace the running ones.
        let ack = h.push(config(3, &[("fake", "two")])).await;
        assert!(ack.errors.is_empty());
        let before = h.fakes.calls("one");
        tokio::time::sleep(Duration::from_secs(25)).await;
        assert_eq!(h.fakes.calls("one"), before);
        assert_eq!(h.fakes.calls("two"), 3);
        assert_eq!(file.load().await, Some(config(3, &[("fake", "two")])));
        h.stop().await;
    }

    #[tokio::test(start_paused = true)]
    async fn the_saved_configuration_runs_after_a_restart_without_upstream() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = start(dir.path());
        h.push(config(5, &[("fake", "kept")])).await;
        h.stop().await;

        // No configuration from the stream this time.
        let h = start(dir.path());
        tokio::time::sleep(Duration::from_secs(25)).await;
        assert_eq!(h.fakes.calls("kept"), 3);
        h.stop().await;
    }

    #[tokio::test(start_paused = true)]
    async fn nothing_runs_without_a_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let h = start(dir.path());
        tokio::time::sleep(Duration::from_secs(25)).await;
        assert_eq!(h.fakes.built.load(Ordering::SeqCst), 0);
        h.stop().await;
    }

    fn refused(ack: &SensorConfigAck) -> Vec<&str> {
        ack.errors.iter().map(|e| e.name.as_str()).collect()
    }

    #[tokio::test(start_paused = true)]
    async fn refused_specs_start_once_the_host_allows_them() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = start(dir.path());
        h.fakes.set_broken("late", true);
        let specs = [("fake", "one"), ("refusable", "late"), ("cpu", "cpu")];
        let ack = h.push(config(1, &specs)).await;
        assert_eq!(refused(&ack), ["late", "cpu"]);
        assert!(ack.errors[0].message.contains("not installed"));

        // Still refused: tried again, nothing to acknowledge.
        tokio::time::sleep(RETRY_EVERY + Duration::from_secs(1)).await;
        assert_eq!(h.fakes.attempts("late"), 2);
        assert!(h.stream.acks.try_recv().is_err());

        // Installed: it starts and a new acknowledgement says so. The
        // unknown kind is not retried.
        h.fakes.set_broken("late", false);
        tokio::time::sleep(RETRY_EVERY).await;
        let ack = h.stream.acks.recv().await.unwrap();
        assert_eq!((ack.revision, refused(&ack)), (1, vec!["cpu"]));
        assert_eq!(h.fakes.attempts("cpu"), 1);
        // The build's collection, then every 10 s from the start.
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(h.fakes.calls("late"), 2);
        tokio::time::sleep(Duration::from_secs(20)).await;
        assert_eq!(h.fakes.calls("late"), 4);
        // The running sensor was left alone.
        assert_eq!(h.fakes.attempts("one"), 1);
        assert!(h.uplink.take().iter().any(|s| s.sensor == "late"));

        // Later retries have nothing to do.
        tokio::time::sleep(RETRY_EVERY).await;
        assert_eq!(h.fakes.attempts("late"), 3);
        assert!(h.stream.acks.try_recv().is_err());
        h.stop().await;
    }

    #[tokio::test(start_paused = true)]
    async fn permanent_failures_are_acknowledged_and_retried() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = start(dir.path());
        h.fakes.set_broken("flaky", true);
        let ack = h
            .push(config(4, &[("fake", "one"), ("fragile", "flaky")]))
            .await;
        assert!(ack.errors.is_empty());
        // The first collection fails permanently and stops the sensor.
        let ack = h.stream.acks.recv().await.unwrap();
        assert_eq!((ack.revision, refused(&ack)), (4, vec!["flaky"]));
        assert_eq!(
            ack.errors[0].message,
            "stopped by a permanent error: broken"
        );

        // Still failing when tried again: no new acknowledgement.
        tokio::time::sleep(RETRY_EVERY + Duration::from_secs(1)).await;
        assert_eq!(h.fakes.attempts("flaky"), 2);
        assert_eq!(h.fakes.calls("flaky"), 2);
        assert!(h.stream.acks.try_recv().is_err());

        h.fakes.set_broken("flaky", false);
        tokio::time::sleep(RETRY_EVERY).await;
        let ack = h.stream.acks.recv().await.unwrap();
        assert_eq!(ack.revision, 4);
        assert!(ack.errors.is_empty());
        h.uplink.take();
        tokio::time::sleep(Duration::from_secs(25)).await;
        assert!(h.uplink.take().iter().any(|s| s.sensor == "flaky"));

        // A new configuration starts from its own state.
        h.fakes.set_broken("flaky", true);
        let ack = h.push(config(5, &[("fragile", "flaky")])).await;
        assert!(ack.errors.is_empty());
        let ack = h.stream.acks.recv().await.unwrap();
        assert_eq!((ack.revision, refused(&ack)), (5, vec!["flaky"]));
        h.stop().await;
    }
}
