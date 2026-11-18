//! Network checks run from a probe (ADR-0148): the central server, or a
//! relay of the device's site. A probe is given a whole list of checks,
//! each for one server or device of the inventory, and runs them like an
//! agent runs its sensors: one [`Scheduler`] per device, so a device's
//! checks are keyed by `(name, instance)` as on an agent. Samples come out
//! of one channel tagged with their device.
//!
//! Sensors are built by kind through a [`ProbeFactory`], so a probe can run
//! kinds that live outside `monitorflow-sensors` (website checks, ADR-0150)
//! by wrapping [`Builtin`].

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use monitorflow_common::proto::v1 as proto;
use monitorflow_sensors::{
    registry, Sample, Scheduler, SchedulerHandle, SchedulerOptions, Sensor, SensorConfig,
};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// The probe's name of the central server, in the `probe` label of the
/// series it writes and in the API.
pub const CENTRAL: &str = "central";
/// Samples buffered between the probe's checks and whoever sends them on.
pub const CAPACITY: usize = 4096;
/// Most samples in one `ProbeResults`.
pub const MAX_BATCH: usize = 2000;

/// Builds the sensor of one probe check from its kind and parameters.
pub trait ProbeFactory: Send + Sync + 'static {
    fn build(
        &self,
        kind: &str,
        params: &BTreeMap<String, String>,
    ) -> Result<Arc<dyn Sensor>, String>;
}

/// The kinds of `monitorflow-sensors`, without an agent's local limits:
/// `ping`, `port` over TCP or UDP, and the others a probe may run.
#[derive(Debug, Default, Clone, Copy)]
pub struct Builtin;

impl ProbeFactory for Builtin {
    fn build(
        &self,
        kind: &str,
        params: &BTreeMap<String, String>,
    ) -> Result<Arc<dyn Sensor>, String> {
        if !PROBE_RUNS.contains(&kind) {
            return Err(format!("a probe does not run `{kind}` checks"));
        }
        registry::build(kind, params).map_err(|e| e.to_string())
    }
}

/// Built-in kinds a probe runs: what it measures over the network. Machine
/// kinds (processor, disks, services) only make sense on an agent.
pub const PROBE_RUNS: &[&str] = &["ping", "port"];

/// One check of a probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// The server or device it is about: its ID in the inventory.
    pub device: String,
    pub kind: String,
    pub config: SensorConfig,
}

impl Check {
    pub fn from_proto(check: proto::ProbeCheck) -> Option<Self> {
        let spec = check.spec?;
        let mut config = SensorConfig::new(spec.name, Duration::from_millis(spec.interval_ms));
        config.instance = spec.instance;
        config.timeout = (spec.timeout_ms > 0).then(|| Duration::from_millis(spec.timeout_ms));
        config.params = spec.params.into_iter().collect();
        Some(Self {
            device: check.device,
            kind: spec.kind,
            config,
        })
    }

    pub fn to_proto(&self) -> proto::ProbeCheck {
        let millis = |d: Duration| u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
        proto::ProbeCheck {
            device: self.device.clone(),
            spec: Some(proto::SensorSpec {
                kind: self.kind.clone(),
                name: self.config.name.clone(),
                instance: self.config.instance.clone(),
                interval_ms: millis(self.config.interval),
                timeout_ms: self.config.timeout.map_or(0, millis),
                params: self.config.params.clone().into_iter().collect(),
            }),
        }
    }
}

/// A check the probe could not start, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckError {
    pub device: String,
    pub name: String,
    pub instance: String,
    pub message: String,
}

impl CheckError {
    fn new(check: &Check, message: impl Into<String>) -> Self {
        Self {
            device: check.device.clone(),
            name: check.config.name.clone(),
            instance: check.config.instance.clone(),
            message: message.into(),
        }
    }

    pub fn to_proto(&self) -> proto::ProbeCheckError {
        proto::ProbeCheckError {
            device: self.device.clone(),
            name: self.name.clone(),
            instance: self.instance.clone(),
            message: self.message.clone(),
        }
    }
}

/// A sample of one device.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceSample {
    pub device: String,
    pub sample: Sample,
}

/// The checks of one device while they run.
struct Running {
    checks: Vec<Check>,
    scheduler: SchedulerHandle,
    forward: JoinHandle<()>,
}

/// Runs a probe's checks; see the crate documentation.
pub struct Runner {
    factory: Arc<dyn ProbeFactory>,
    options: SchedulerOptions,
    devices: HashMap<String, Running>,
    tx: mpsc::Sender<DeviceSample>,
}

impl Runner {
    /// A runner without checks, and the receiver of its samples. A full
    /// channel drops new samples, as on an agent (ADR-0012).
    pub fn new(factory: Arc<dyn ProbeFactory>) -> (Self, mpsc::Receiver<DeviceSample>) {
        Self::with_options(factory, SchedulerOptions::default())
    }

    pub fn with_options(
        factory: Arc<dyn ProbeFactory>,
        options: SchedulerOptions,
    ) -> (Self, mpsc::Receiver<DeviceSample>) {
        let (tx, rx) = mpsc::channel(CAPACITY);
        let runner = Self {
            factory,
            options,
            devices: HashMap::new(),
            tx,
        };
        (runner, rx)
    }

    /// Devices with checks running.
    pub fn devices(&self) -> usize {
        self.devices.len()
    }

    /// Replaces every check with `checks`. Devices whose checks did not
    /// change keep running undisturbed; the others restart. Returns the
    /// checks that could not start.
    pub async fn apply(&mut self, checks: Vec<Check>) -> Vec<CheckError> {
        let mut by_device: BTreeMap<String, Vec<Check>> = BTreeMap::new();
        for check in checks {
            by_device
                .entry(check.device.clone())
                .or_default()
                .push(check);
        }
        let gone: Vec<String> = self
            .devices
            .keys()
            .filter(|d| !by_device.contains_key(*d))
            .cloned()
            .collect();
        for device in gone {
            if let Some(running) = self.devices.remove(&device) {
                stop(running).await;
            }
        }
        let mut errors = Vec::new();
        for (device, mut checks) in by_device {
            checks.sort_by(|a, b| {
                (&a.config.name, &a.config.instance).cmp(&(&b.config.name, &b.config.instance))
            });
            if self
                .devices
                .get(&device)
                .is_some_and(|r| r.checks == checks)
            {
                continue;
            }
            if let Some(running) = self.devices.remove(&device) {
                stop(running).await;
            }
            let mut scheduler = Scheduler::new(self.options.clone());
            for check in &checks {
                let built = check
                    .config
                    .validate()
                    .map_err(|e| e.to_string())
                    .and_then(|()| guarded(&*self.factory, check))
                    .and_then(|sensor| {
                        scheduler
                            .add(check.config.clone(), sensor)
                            .map_err(|e| e.to_string())
                    });
                if let Err(message) = built {
                    errors.push(CheckError::new(check, message));
                }
            }
            let (handle, mut samples) = scheduler.start();
            let (tx, name) = (self.tx.clone(), device.clone());
            let forward = tokio::spawn(async move {
                while let Some(sample) = samples.recv().await {
                    let sample = DeviceSample {
                        device: name.clone(),
                        sample,
                    };
                    if tx.try_send(sample).is_err() {
                        tracing::debug!(device = %name, "probe sample queue full; dropped");
                    }
                }
            });
            self.devices.insert(
                device,
                Running {
                    checks,
                    scheduler: handle,
                    forward,
                },
            );
        }
        errors
    }

    /// Stops every check and waits for them.
    pub async fn shutdown(mut self) {
        for (_, running) in self.devices.drain() {
            stop(running).await;
        }
    }
}

/// Builds a check's sensor; a factory that panics fails that check only.
fn guarded(factory: &dyn ProbeFactory, check: &Check) -> Result<Arc<dyn Sensor>, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        factory.build(&check.kind, &check.config.params)
    }))
    .unwrap_or_else(|_| {
        Err(format!(
            "building a `{}` check failed unexpectedly",
            check.kind
        ))
    })
}

async fn stop(running: Running) {
    running.scheduler.shutdown().await;
    // The scheduler's channel closed; the forwarder drains it and ends.
    let _ = running.forward.await;
}

/// Waits for the next sample, then takes what else is queued, up to
/// [`MAX_BATCH`]; `None` once the runner is gone.
pub async fn next_batch(rx: &mut mpsc::Receiver<DeviceSample>) -> Option<Vec<DeviceSample>> {
    let first = rx.recv().await?;
    let mut batch = vec![first];
    while batch.len() < MAX_BATCH {
        match rx.try_recv() {
            Ok(sample) => batch.push(sample),
            Err(_) => break,
        }
    }
    Some(batch)
}

/// Samples grouped by device, in the order they came, for `ProbeResults`.
pub fn to_results(batch: Vec<DeviceSample>) -> proto::ProbeResults {
    let mut devices: Vec<proto::DeviceSamples> = Vec::new();
    for DeviceSample { device, sample } in batch {
        let sample = proto::Sample {
            sensor: sample.sensor,
            instance: sample.instance,
            timestamp_unix_ms: sample.timestamp_unix_ms,
            value: sample.value,
            labels: sample.labels.into_iter().collect(),
        };
        match devices.iter_mut().find(|d| d.device == device) {
            Some(entry) => entry.samples.push(sample),
            None => devices.push(proto::DeviceSamples {
                device,
                samples: vec![sample],
            }),
        }
    }
    proto::ProbeResults { devices }
}

#[cfg(test)]
mod tests;
