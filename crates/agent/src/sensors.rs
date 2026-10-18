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
//! Samples of every scheduler go to the same [`Uplink`], which the stream
//! empties in batches.

use std::sync::Arc;

use monitorflow_common::proto::v1::{SensorConfig, SensorConfigAck, SensorSpecError};
use monitorflow_sensors::{SchedulerHandle, SchedulerOptions};
use tokio::task::JoinHandle;

use crate::sensor_config::{self, ConfigFile, SensorFactory, SensorsEnd};
use crate::shutdown::Shutdown;
use crate::upload::{self, Uplink};

/// The sensors task.
pub struct Sensors {
    factory: Arc<dyn SensorFactory>,
    file: ConfigFile,
    uplink: Uplink,
    options: SchedulerOptions,
}

/// The configuration in force and its scheduler.
struct Running {
    config: SensorConfig,
    errors: Vec<SensorSpecError>,
    handle: SchedulerHandle,
    consumer: JoinHandle<u64>,
}

impl Running {
    /// Stops the sensors and lets the consumer finish the queued samples.
    async fn stop(self) {
        let stats = self.handle.stats();
        self.handle.shutdown().await;
        // The channel closed with the last sensor task, so this ends.
        let _ = self.consumer.await;
        tracing::info!(revision = self.config.revision, ?stats, "sensors stopped");
    }
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
        loop {
            tokio::select! {
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
                    let applied = self.apply(running.take(), config).await;
                    let ack = SensorConfigAck {
                        revision: applied.config.revision,
                        errors: applied.errors.clone(),
                    };
                    running = Some(applied);
                    if end.acks.try_send(ack).is_err() {
                        tracing::debug!("acknowledgement queue full; dropped");
                    }
                }
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
        let (scheduler, errors) =
            sensor_config::build(&config, self.factory.as_ref(), self.options.clone());
        for error in &errors {
            tracing::warn!(
                sensor = %error.name,
                instance = %error.instance,
                error = %error.message,
                "sensor not started"
            );
        }
        let count = scheduler.len();
        let (handle, samples) = scheduler.start();
        tracing::info!(
            revision = config.revision,
            sensors = count,
            refused = errors.len(),
            "sensors started"
        );
        Running {
            config,
            errors,
            handle,
            consumer: tokio::spawn(upload::collect(samples, self.uplink.clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    use monitorflow_common::proto::v1::SensorSpec;
    use monitorflow_sensors::testing::FakeSensor;
    use monitorflow_sensors::{Sensor, SensorConfig as Config};

    use super::*;
    use crate::sensor_config::{link, FactoryError, StreamEnd};
    use crate::shutdown::{self, Trigger};
    use crate::upload::UploadSettings;

    /// Builds `fake` sensors and keeps them, to count their calls.
    #[derive(Default)]
    struct Fakes {
        built: AtomicUsize,
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
    }

    impl SensorFactory for Fakes {
        fn build(&self, kind: &str, config: &Config) -> Result<Arc<dyn Sensor>, FactoryError> {
            if kind != "fake" {
                return Err(FactoryError::UnknownKind(kind.into()));
            }
            self.built.fetch_add(1, Ordering::SeqCst);
            let sensor = Arc::new(FakeSensor::new());
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
}
