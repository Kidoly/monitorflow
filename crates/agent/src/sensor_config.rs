//! The sensor configuration pushed by the server (ADR-0016): turning specs
//! into a [`Scheduler`] through a [`SensorFactory`], the channels between
//! the stream and the sensors task, and the copy kept in `data_dir`.
//!
//! A spec that cannot run (unknown kind, invalid name or interval, bad
//! parameters, duplicate) is reported in the acknowledgement; the others
//! run. Nothing here can stop the agent.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use monitorflow_common::proto::v1::{SensorConfig, SensorConfigAck, SensorSpec, SensorSpecError};
use monitorflow_sensors::{self as sensors, Scheduler, SchedulerOptions, Sensor};
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
    /// The kind exists but refuses this configuration, like a bad param.
    #[error("{0}")]
    Invalid(String),
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

/// The built-in sensors of [`sensors::registry`] (ADR-0015).
#[derive(Debug, Clone, Copy, Default)]
pub struct Builtin;

impl SensorFactory for Builtin {
    fn build(
        &self,
        kind: &str,
        config: &sensors::SensorConfig,
    ) -> Result<Arc<dyn Sensor>, FactoryError> {
        sensors::registry::build(kind, &config.params).map_err(|error| match error {
            sensors::BuildError::UnknownKind(kind) => FactoryError::UnknownKind(kind),
            other => FactoryError::Invalid(other.to_string()),
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

/// A scheduler for the specs that can run, and why the others cannot.
pub fn build(
    config: &SensorConfig,
    factory: &dyn SensorFactory,
    options: SchedulerOptions,
) -> (Scheduler, Vec<SensorSpecError>) {
    let mut scheduler = Scheduler::new(options);
    let mut errors = Vec::new();
    for spec in &config.sensors {
        let sensor_config = sensor_config(spec);
        // Validate before the factory sees it; `add` checks duplicates.
        let added = sensor_config
            .validate()
            .map_err(|e| e.to_string())
            .and_then(|()| {
                factory
                    .build(&spec.kind, &sensor_config)
                    .map_err(|e| e.to_string())
            })
            .and_then(|sensor| {
                scheduler
                    .add(sensor_config, sensor)
                    .map_err(|e| e.to_string())
            });
        if let Err(message) = added {
            errors.push(SensorSpecError {
                name: spec.name.clone(),
                instance: spec.instance.clone(),
                message,
            });
        }
    }
    (scheduler, errors)
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
        let (scheduler, errors) = build(&config, &Fakes, SchedulerOptions::default());
        assert_eq!(scheduler.len(), 1);
        let refused: Vec<_> = errors.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            refused,
            ["cpu", "Bad-Name", "too_fast", "bad_param", "good"]
        );
        assert!(
            errors[0].message.contains("unknown sensor kind `cpu`"),
            "{errors:?}"
        );
        assert!(errors[3].message.contains("told to fail"), "{errors:?}");
        assert!(errors[4].message.contains("twice"), "{errors:?}");
    }

    #[test]
    fn builtin_builds_the_registry_kinds() {
        let mut config = sensors::SensorConfig::new("cpu", Duration::from_secs(60));
        for kind in sensors::registry::KINDS {
            assert!(Builtin.build(kind, &config).is_ok(), "{kind}");
        }
        assert_eq!(
            Builtin.build("gpu", &config).err(),
            Some(FactoryError::UnknownKind("gpu".into()))
        );
        config.params.insert("per_cores".into(), "true".into());
        let error = Builtin.build("cpu", &config).err().unwrap();
        assert!(
            matches!(&error, FactoryError::Invalid(m) if m.contains("per_cores")),
            "{error}"
        );
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
