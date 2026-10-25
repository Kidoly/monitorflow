//! The sensor configuration pushed by the server (ADR-0016): turning specs
//! into a [`Scheduler`] through a [`SensorFactory`], the channels between
//! the stream and the sensors task, and the copy kept in `data_dir`.
//!
//! A spec that cannot run (unknown kind, invalid name or interval, bad
//! parameters, duplicate, refused by this host) is reported in the
//! acknowledgement; the others run. Sensors are wrapped so that a
//! permanent error, which stops them (ADR-0012), is reported too. Nothing
//! here can stop the agent.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use monitorflow_common::config::AgentSensorsConfig;
use monitorflow_common::proto::v1::{SensorConfig, SensorConfigAck, SensorSpec, SensorSpecError};
use monitorflow_sensors::registry::Local;
use monitorflow_sensors::{
    self as sensors, Sample, Scheduler, SchedulerOptions, Sensor, SensorError,
};
use prost::Message;
use tokio::sync::{mpsc, watch};

/// File in `data_dir` holding the last configuration received.
pub const FILE_NAME: &str = "sensor-config.pb";
/// Acknowledgements waiting for a stream. More are dropped: the server
/// sends the configuration after every Hello, and the agent acknowledges
/// it again.
const ACK_QUEUE: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FactoryError {
    #[error("unknown sensor kind `{0}`")]
    UnknownKind(String),
    /// Valid, but not allowed or not possible on this host, which can
    /// change: a script installed later (ADR-0024).
    #[error("{0}")]
    Refused(String),
    /// The kind exists but refuses this configuration, like a bad param.
    #[error("{0}")]
    Invalid(String),
}

impl FactoryError {
    fn down_kind(&self) -> DownKind {
        match self {
            Self::Refused(_) => DownKind::Refused,
            Self::UnknownKind(_) | Self::Invalid(_) => DownKind::Invalid,
        }
    }
}

/// Builds sensor implementations by kind. The agent uses [`Builtin`];
/// tests plug in fakes.
pub trait SensorFactory: Send + Sync + 'static {
    fn build(
        &self,
        kind: &str,
        config: &sensors::SensorConfig,
    ) -> Result<Arc<dyn Sensor>, FactoryError>;
}

/// The built-in sensors of [`sensors::registry`] (ADR-0015), within the
/// limits of the agent's own configuration (ADR-0024).
#[derive(Debug, Clone, Default)]
pub struct Builtin {
    local: Local,
}

impl Builtin {
    pub fn new(local: Local) -> Self {
        Self { local }
    }

    /// The limits set by the `[sensors]` section of the agent's config.
    pub fn from_config(config: &AgentSensorsConfig) -> Self {
        Self::new(local(config))
    }
}

/// The registry's [`Local`] limits from the agent's configuration.
pub fn local(config: &AgentSensorsConfig) -> Local {
    Local {
        script_dir: config.script_dir.clone(),
        log_dirs: config.log_dirs.clone(),
    }
}

impl SensorFactory for Builtin {
    fn build(
        &self,
        kind: &str,
        config: &sensors::SensorConfig,
    ) -> Result<Arc<dyn Sensor>, FactoryError> {
        sensors::registry::build_local(kind, &config.params, &self.local).map_err(|error| {
            match error {
                sensors::BuildError::UnknownKind(kind) => FactoryError::UnknownKind(kind),
                refused @ sensors::BuildError::Refused { .. } => {
                    FactoryError::Refused(refused.to_string())
                }
                other => FactoryError::Invalid(other.to_string()),
            }
        })
    }
}

/// The scheduling part of a spec.
pub fn sensor_config(spec: &SensorSpec) -> sensors::SensorConfig {
    sensors::SensorConfig {
        name: spec.name.clone(),
        instance: spec.instance.clone(),
        interval: Duration::from_millis(spec.interval_ms),
        timeout: (spec.timeout_ms > 0).then(|| Duration::from_millis(spec.timeout_ms)),
        params: spec.params.clone().into_iter().collect(),
    }
}

/// Why a spec of the configuration is not running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownKind {
    /// Invalid on any host (name, interval, kind, parameters, duplicate):
    /// trying again cannot help.
    Invalid,
    /// Refused by this host's limits, which can change.
    Refused,
    /// Started, then stopped by a permanent error.
    Failed,
}

/// A spec that is not running, and why.
#[derive(Debug, Clone, PartialEq)]
pub struct Down {
    pub spec: SensorSpec,
    pub kind: DownKind,
    pub message: String,
}

impl Down {
    fn new(spec: &SensorSpec, kind: DownKind, message: String) -> Self {
        Self {
            spec: spec.clone(),
            kind,
            message,
        }
    }

    /// The factory's refusal of `spec`.
    pub fn refused(spec: &SensorSpec, error: FactoryError) -> Self {
        Self::new(spec, error.down_kind(), error.to_string())
    }

    /// `spec` stopped by the permanent error `message`.
    pub fn failed(spec: &SensorSpec, message: &str) -> Self {
        Self::new(
            spec,
            DownKind::Failed,
            format!("stopped by a permanent error: {message}"),
        )
    }

    /// Whether the host may now accept it.
    pub fn retryable(&self) -> bool {
        self.kind != DownKind::Invalid
    }

    /// As reported in the acknowledgement.
    pub fn error(&self) -> SensorSpecError {
        SensorSpecError {
            name: self.spec.name.clone(),
            instance: self.spec.instance.clone(),
            message: self.message.clone(),
        }
    }
}

/// A permanent error of a running sensor, after which the scheduler stops
/// it: the spec's name and instance, and the error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub name: String,
    pub instance: String,
    pub message: String,
}

/// Where [`watch`]ed sensors report their [`Failure`]. Unbounded, but each
/// sensor reports at most once, since it stops right after.
pub type Failures = mpsc::UnboundedSender<Failure>;

/// `sensor`, reporting its permanent errors to `failures`.
pub fn watch(
    sensor: Arc<dyn Sensor>,
    config: &sensors::SensorConfig,
    failures: &Failures,
) -> Arc<dyn Sensor> {
    Arc::new(Watched {
        sensor,
        name: config.name.clone(),
        instance: config.instance.clone(),
        failures: failures.clone(),
    })
}

struct Watched {
    sensor: Arc<dyn Sensor>,
    name: String,
    instance: String,
    failures: Failures,
}

#[async_trait::async_trait]
impl Sensor for Watched {
    async fn collect(&self, deadline: tokio::time::Instant) -> Result<Vec<Sample>, SensorError> {
        let result = self.sensor.collect(deadline).await;
        if let Err(error @ SensorError::Permanent(_)) = &result {
            // Closed only once the configuration was replaced.
            let _ = self.failures.send(Failure {
                name: self.name.clone(),
                instance: self.instance.clone(),
                message: error.to_string(),
            });
        }
        result
    }
}

/// A scheduler for the specs that can run, reporting permanent errors to
/// `failures`, and why the others cannot.
pub fn build(
    config: &SensorConfig,
    factory: &dyn SensorFactory,
    options: SchedulerOptions,
    failures: &Failures,
) -> (Scheduler, Vec<Down>) {
    let mut scheduler = Scheduler::new(options);
    let mut down = Vec::new();
    for spec in &config.sensors {
        let sensor_config = sensor_config(spec);
        // Validate before the factory sees it; `add` checks duplicates.
        let added = sensor_config
            .validate()
            .map_err(|e| (DownKind::Invalid, e.to_string()))
            .and_then(|()| {
                factory
                    .build(&spec.kind, &sensor_config)
                    .map_err(|e| (e.down_kind(), e.to_string()))
            })
            .and_then(|sensor| {
                let sensor = watch(sensor, &sensor_config, failures);
                scheduler
                    .add(sensor_config, sensor)
                    .map_err(|e| (DownKind::Invalid, e.to_string()))
            });
        if let Err((kind, message)) = added {
            down.push(Down::new(spec, kind, message));
        }
    }
    (scheduler, down)
}

/// The stream's side: configurations go down, acknowledgements come up.
pub struct StreamEnd {
    pub configs: watch::Sender<Option<SensorConfig>>,
    pub acks: mpsc::Receiver<SensorConfigAck>,
}

/// The sensors task's side. Only the latest configuration matters, hence
/// a watch channel.
pub struct SensorsEnd {
    pub configs: watch::Receiver<Option<SensorConfig>>,
    pub acks: mpsc::Sender<SensorConfigAck>,
}

pub fn link() -> (StreamEnd, SensorsEnd) {
    let (configs_tx, configs_rx) = watch::channel(None);
    let (acks_tx, acks_rx) = mpsc::channel(ACK_QUEUE);
    (
        StreamEnd {
            configs: configs_tx,
            acks: acks_rx,
        },
        SensorsEnd {
            configs: configs_rx,
            acks: acks_tx,
        },
    )
}

/// The last configuration received, kept so the agent monitors from
/// start-up even when the upstream is unreachable. Stored as the protobuf
/// message itself.
#[derive(Debug, Clone)]
pub struct ConfigFile {
    path: PathBuf,
}

impl ConfigFile {
    pub fn in_dir(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join(FILE_NAME),
        }
    }

    /// `None` when missing or unreadable; an unreadable file is logged and
    /// replaced by the next configuration.
    pub async fn load(&self) -> Option<SensorConfig> {
        let bytes = match tokio::fs::read(&self.path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
            Err(error) => {
                tracing::warn!(path = %self.path.display(), %error, "cannot read the saved sensor configuration");
                return None;
            }
        };
        match SensorConfig::decode(bytes.as_slice()) {
            Ok(config) => Some(config),
            Err(error) => {
                tracing::warn!(path = %self.path.display(), %error, "ignoring a corrupt saved sensor configuration");
                None
            }
        }
    }

    /// Writes a temporary file and renames it, so a crash leaves the old
    /// or the new configuration, never half of one.
    pub async fn save(&self, config: &SensorConfig) -> io::Result<()> {
        if let Some(dir) = self.path.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        let tmp = self.path.with_extension("pb.tmp");
        tokio::fs::write(&tmp, config.encode_to_vec()).await?;
        tokio::fs::rename(&tmp, &self.path).await
    }
}

#[cfg(test)]
mod tests {
    use monitorflow_sensors::testing::FakeSensor;

    use super::*;

    /// Knows the kind `fake`; refuses a `fail` param.
    struct Fakes;

    impl SensorFactory for Fakes {
        fn build(
            &self,
            kind: &str,
            config: &sensors::SensorConfig,
        ) -> Result<Arc<dyn Sensor>, FactoryError> {
            match kind {
                "fake" if config.params.contains_key("fail") => {
                    Err(FactoryError::Invalid("told to fail".into()))
                }
                "fake" => Ok(Arc::new(FakeSensor::new())),
                other => Err(FactoryError::UnknownKind(other.into())),
            }
        }
    }

    fn spec(kind: &str, name: &str, interval_ms: u64) -> SensorSpec {
        SensorSpec {
            kind: kind.into(),
            name: name.into(),
            interval_ms,
            ..SensorSpec::default()
        }
    }

    #[test]
    fn specs_become_scheduler_configs() {
        let mut spec = spec("port", "port", 10_000);
        spec.params.insert("port".into(), "22".into());
        let config = sensor_config(&spec);
        assert_eq!(config.interval, Duration::from_secs(10));
        assert_eq!(config.timeout, None);
        assert_eq!(config.params["port"], "22");
        spec.timeout_ms = 2_000;
        assert_eq!(sensor_config(&spec).timeout, Some(Duration::from_secs(2)));
    }

    #[test]
    fn invalid_specs_are_reported_and_the_others_run() {
        let mut bad_param = spec("fake", "bad_param", 10_000);
        bad_param.params.insert("fail".into(), "1".into());
        let config = SensorConfig {
            revision: 3,
            sensors: vec![
                spec("fake", "good", 10_000),
                spec("cpu", "cpu", 60_000),
                spec("fake", "Bad-Name", 10_000),
                spec("fake", "too_fast", 10),
                bad_param,
                spec("fake", "good", 20_000),
            ],
        };
        let (failures, _) = mpsc::unbounded_channel();
        let (scheduler, errors) = build(&config, &Fakes, SchedulerOptions::default(), &failures);
        assert_eq!(scheduler.len(), 1);
        let refused: Vec<_> = errors.iter().map(|e| e.spec.name.as_str()).collect();
        assert_eq!(
            refused,
            ["cpu", "Bad-Name", "too_fast", "bad_param", "good"]
        );
        assert!(errors.iter().all(|e| e.kind == DownKind::Invalid));
        assert!(
            errors[0].message.contains("unknown sensor kind `cpu`"),
            "{errors:?}"
        );
        assert!(errors[3].message.contains("told to fail"), "{errors:?}");
        assert!(errors[4].message.contains("twice"), "{errors:?}");
    }

    // The Linux kinds; Windows refuses them until it has its own.
    #[cfg(unix)]
    #[test]
    fn builtin_builds_the_registry_kinds() {
        let mut config = sensors::SensorConfig::new("cpu", Duration::from_secs(60));
        // Every parameter a kind requires; the others ignore none of them.
        let scripts = tempfile::tempdir().unwrap();
        std::fs::write(scripts.path().join("check"), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(
            scripts.path().join("check"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        let builtin = Builtin::from_config(&AgentSensorsConfig {
            script_dir: scripts.path().into(),
            log_dirs: vec![scripts.path().into()],
        });
        let log = format!("{}/app.log", scripts.path().display());
        let required: [(&str, &[(&str, &str)]); 4] = [
            ("log", &[("path", &log), ("pattern", "error")]),
            ("port", &[("port", "22")]),
            ("script", &[("script", "check")]),
            ("systemd", &[("units", "ssh")]),
        ];
        for kind in sensors::registry::KINDS {
            let mut config = config.clone();
            let pairs = required.iter().find(|(k, _)| k == kind);
            for (name, value) in pairs.map_or(&[][..], |(_, p)| p) {
                config.params.insert(name.to_string(), value.to_string());
            }
            assert!(builtin.build(kind, &config).is_ok(), "{kind}");
        }
        // Valid for the server, refused by this host's limits.
        let mut missing = config.clone();
        missing.params.insert("script".into(), "missing".into());
        let error = builtin.build("script", &missing).err().unwrap();
        assert!(
            matches!(&error, FactoryError::Refused(m) if m.contains("refused by this agent") && m.contains("not installed")),
            "{error}"
        );
        let builtin = Builtin::default();
        assert_eq!(
            builtin.build("gpu", &config).err(),
            Some(FactoryError::UnknownKind("gpu".into()))
        );
        config.params.insert("per_cores".into(), "true".into());
        let error = builtin.build("cpu", &config).err().unwrap();
        assert!(
            matches!(&error, FactoryError::Invalid(m) if m.contains("per_cores")),
            "{error}"
        );
    }

    #[test]
    fn only_host_refusals_and_failures_are_retried() {
        let spec = spec("script", "backup", 60_000);
        let refused = Down::refused(&spec, FactoryError::Refused("not installed".into()));
        assert_eq!(refused.kind, DownKind::Refused);
        assert!(refused.retryable());
        let invalid = Down::refused(&spec, FactoryError::Invalid("bad".into()));
        assert!(!invalid.retryable());
        assert!(!Down::refused(&spec, FactoryError::UnknownKind("x".into())).retryable());
        let failed = Down::failed(&spec, "gone");
        assert!(failed.retryable());
        assert_eq!(failed.error().message, "stopped by a permanent error: gone");
        assert_eq!(failed.error().name, "backup");
    }

    #[tokio::test(start_paused = true)]
    async fn watched_sensors_report_permanent_errors_only() {
        use monitorflow_sensors::testing::Step;
        let (failures, mut reported) = mpsc::unbounded_channel();
        let mut config = sensors::SensorConfig::new("flaky", Duration::from_secs(10));
        config.instance = "a".into();
        let fake = FakeSensor::new()
            .on_call(0, Step::Fail(SensorError::Transient("busy".into())))
            .on_call(1, Step::Fail(SensorError::Permanent("gone".into())));
        let sensor = watch(Arc::new(fake), &config, &failures);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        assert!(sensor.collect(deadline).await.is_err());
        assert!(reported.try_recv().is_err());
        assert!(sensor.collect(deadline).await.is_err());
        assert_eq!(
            reported.try_recv().unwrap(),
            Failure {
                name: "flaky".into(),
                instance: "a".into(),
                message: "gone".into()
            }
        );
        assert_eq!(sensor.collect(deadline).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_file_round_trips_and_tolerates_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let file = ConfigFile::in_dir(&dir.path().join("sub"));
        assert_eq!(file.load().await, None);
        let config = SensorConfig {
            revision: 9,
            sensors: vec![spec("fake", "good", 10_000)],
        };
        file.save(&config).await.unwrap();
        assert_eq!(file.load().await, Some(config));
        std::fs::write(dir.path().join("sub").join(FILE_NAME), b"\xff\xff\xff").unwrap();
        assert_eq!(file.load().await, None);
    }
}
