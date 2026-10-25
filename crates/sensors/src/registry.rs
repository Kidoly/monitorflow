//! Builds the built-in sensors by kind. The configuration pushed by the
//! server names a kind (today the [`SensorConfig::name`](crate::SensorConfig::name),
//! like `cpu`) and its `params`; [`build`] checks both up front so a bad
//! configuration is refused when it arrives, not on the first tick.
//!
//! Parameters are strict: an unknown name is an error, so a typo does not
//! silently fall back to a default. See ADR-0015.
//!
//! [`build`] only checks syntax, so the server's API can use it. The agent
//! calls [`build_local`] with its [`Local`] limits, which the server's
//! configuration cannot change, and also refuses what this host does not
//! allow, like a script nobody installed (ADR-0024).
//!
//! Some kinds only run on one platform ([`supported`]): `systemd` and
//! `log` on Unix, `eventlog` and `windows_service` on Windows. The agent
//! refuses the others with [`BuildError::Unsupported`]. [`build`] still
//! checks the parameters of Windows kinds on Unix, so the server's API
//! validates them; on Windows, Unix kinds are refused without looking at
//! their parameters, as their code is not compiled there (ADR-0048).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use crate::cpu::CpuSensor;
use crate::disk::DiskSensor;
use crate::eventlog::{self, EventLogSensor};
#[cfg(unix)]
use crate::log::{self, LogSensor};
use crate::memory::MemorySensor;
use crate::port::PortSensor;
use crate::script::{self, ScriptSensor};
#[cfg(unix)]
use crate::systemd::SystemdSensor;
use crate::windows_service::ServiceSensor;
use crate::Sensor;

/// Every kind [`build`] knows, in a stable order, whatever the platform.
pub const KINDS: &[&str] = &[
    "cpu",
    "disk",
    "eventlog",
    "log",
    "memory",
    "port",
    "script",
    "systemd",
    "windows_service",
];

/// Whether this platform runs `kind`: `log` and `systemd` need Unix,
/// `eventlog` and `windows_service` Windows, the others run on both.
pub fn supported(kind: &str) -> bool {
    match kind {
        "log" | "systemd" => cfg!(unix),
        "eventlog" | "windows_service" => cfg!(windows),
        _ => KINDS.contains(&kind),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BuildError {
    #[error("unknown sensor kind `{0}`, expected one of: {kinds}", kinds = KINDS.join(", "))]
    UnknownKind(String),
    #[error("sensor `{kind}`: unknown parameter `{name}`")]
    UnknownParam { kind: &'static str, name: String },
    #[error("sensor `{kind}`: parameter `{name}` {reason}")]
    BadParam {
        kind: &'static str,
        name: &'static str,
        reason: String,
    },
    /// Valid, but not allowed or not possible on this host.
    #[error("sensor `{kind}` refused by this agent: {reason}")]
    Refused { kind: &'static str, reason: String },
    /// A kind this platform does not run, see [`supported`].
    #[error("sensor `{kind}` is not supported on this platform ({os})", os = std::env::consts::OS)]
    Unsupported { kind: &'static str },
}

/// What the agent's own configuration allows sensors to touch. The
/// server's configuration names things inside these limits only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Local {
    /// Where the `script` sensor finds the scripts it may run.
    pub script_dir: PathBuf,
    /// Directories the `log` sensor may read files under.
    pub log_dirs: Vec<PathBuf>,
    /// Event Log channels the `eventlog` sensor may read, compared without
    /// case.
    pub event_channels: Vec<String>,
}

impl Default for Local {
    fn default() -> Self {
        #[cfg(unix)]
        let log_dirs = log::DEFAULT_DIRS.iter().map(PathBuf::from).collect();
        #[cfg(not(unix))]
        let log_dirs = Vec::new();
        Self {
            script_dir: script::DEFAULT_DIR.into(),
            log_dirs,
            event_channels: eventlog::DEFAULT_CHANNELS
                .iter()
                .map(|c| c.to_string())
                .collect(),
        }
    }
}

/// Builds the sensor of kind `kind` with `params`, checking their syntax
/// only. The sensor uses the default [`Local`] limits.
pub fn build(kind: &str, params: &BTreeMap<String, String>) -> Result<Arc<dyn Sensor>, BuildError> {
    build_with(kind, params, None)
}

/// Like [`build`], within the `local` limits, and refuses with
/// [`BuildError::Refused`] what they do not allow and with
/// [`BuildError::Unsupported`] kinds this platform does not run.
pub fn build_local(
    kind: &str,
    params: &BTreeMap<String, String>,
    local: &Local,
) -> Result<Arc<dyn Sensor>, BuildError> {
    build_with(kind, params, Some(local))
}

fn build_with(
    kind: &str,
    params: &BTreeMap<String, String>,
    local: Option<&Local>,
) -> Result<Arc<dyn Sensor>, BuildError> {
    let mut params = Params::new(kind, params)?;
    if local.is_some() && !supported(params.kind) {
        return Err(BuildError::Unsupported { kind: params.kind });
    }
    let sensor: Arc<dyn Sensor> = match params.kind {
        "cpu" => Arc::new(CpuSensor::new(params.bool("per_core", true)?)),
        "disk" => Arc::new(DiskSensor::from_params(&mut params)?),
        "eventlog" => Arc::new(EventLogSensor::from_params(&mut params, local)?),
        #[cfg(unix)]
        "log" => Arc::new(LogSensor::from_params(&mut params, local)?),
        "memory" => Arc::new(MemorySensor),
        "port" => Arc::new(PortSensor::from_params(&mut params)?),
        "script" => Arc::new(ScriptSensor::from_params(&mut params, local)?),
        #[cfg(unix)]
        "systemd" => Arc::new(SystemdSensor::from_params(&mut params)?),
        "windows_service" => Arc::new(ServiceSensor::from_params(&mut params)?),
        // Unix kinds on Windows, not compiled there.
        kind => return Err(BuildError::Unsupported { kind }),
    };
    params.finish()?;
    Ok(sensor)
}

/// Typed access to a sensor's parameters that remembers which were read,
/// so [`Params::finish`] can refuse the others.
pub(crate) struct Params<'a> {
    kind: &'static str,
    map: &'a BTreeMap<String, String>,
    used: BTreeSet<&'static str>,
}

impl<'a> Params<'a> {
    pub(crate) fn new(kind: &str, map: &'a BTreeMap<String, String>) -> Result<Self, BuildError> {
        let kind = KINDS
            .iter()
            .find(|k| **k == kind)
            .ok_or_else(|| BuildError::UnknownKind(kind.chars().take(64).collect()))?;
        Ok(Self {
            kind,
            map,
            used: BTreeSet::new(),
        })
    }

    pub(crate) fn get(&mut self, name: &'static str) -> Option<&'a str> {
        self.used.insert(name);
        self.map.get(name).map(|v| v.trim())
    }

    pub(crate) fn bad(&self, name: &'static str, reason: impl Into<String>) -> BuildError {
        BuildError::BadParam {
            kind: self.kind,
            name,
            reason: reason.into(),
        }
    }

    pub(crate) fn refused(&self, reason: impl Into<String>) -> BuildError {
        BuildError::Refused {
            kind: self.kind,
            reason: reason.into(),
        }
    }

    /// `true` or `false`.
    pub(crate) fn bool(&mut self, name: &'static str, default: bool) -> Result<bool, BuildError> {
        match self.get(name) {
            None => Ok(default),
            Some("true") => Ok(true),
            Some("false") => Ok(false),
            Some(_) => Err(self.bad(name, "must be `true` or `false`")),
        }
    }

    /// A comma-separated list without empty items; `None` when absent.
    pub(crate) fn list(&mut self, name: &'static str) -> Result<Option<Vec<String>>, BuildError> {
        let Some(value) = self.get(name) else {
            return Ok(None);
        };
        let items: Vec<String> = value.split(',').map(|s| s.trim().to_owned()).collect();
        if items.iter().any(String::is_empty) {
            return Err(self.bad(name, "must be a comma-separated list without empty items"));
        }
        Ok(Some(items))
    }

    pub(crate) fn finish(self) -> Result<(), BuildError> {
        match self.map.keys().find(|k| !self.used.contains(k.as_str())) {
            Some(name) => Err(BuildError::UnknownParam {
                kind: self.kind,
                name: name.chars().take(64).collect(),
            }),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn every_kind_builds_with_defaults_or_asks_for_params() {
        let required: [(&str, &[(&str, &str)]); 6] = [
            ("eventlog", &[("channel", "System")]),
            ("log", &[("path", "/var/log/syslog"), ("pattern", "error")]),
            ("port", &[("port", "22")]),
            ("script", &[("script", "check")]),
            ("systemd", &[("units", "ssh")]),
            ("windows_service", &[("services", "W32Time")]),
        ];
        // Unix kinds are not compiled on Windows.
        for kind in KINDS.iter().filter(|k| cfg!(unix) || supported(k)) {
            match required.iter().find(|(k, _)| k == kind) {
                None => assert!(build(kind, &BTreeMap::new()).is_ok(), "{kind}"),
                Some((_, pairs)) => {
                    let error = build(kind, &BTreeMap::new()).err().unwrap();
                    assert!(error.to_string().contains("is required"), "{error}");
                    assert!(build(kind, &params(pairs)).is_ok(), "{kind}");
                }
            }
        }
    }

    #[test]
    fn unknown_kinds_and_params_are_refused() {
        let error = build("gpu", &BTreeMap::new()).err().unwrap();
        assert_eq!(error, BuildError::UnknownKind("gpu".into()));
        assert!(error.to_string().contains(
            "expected one of: cpu, disk, eventlog, log, memory, port, script, systemd, windows_service"
        ));
        assert_eq!(
            build("cpu", &params(&[("per_cores", "true")])).err(),
            Some(BuildError::UnknownParam {
                kind: "cpu",
                name: "per_cores".into()
            })
        );
    }

    #[test]
    fn local_limits_refuse_what_the_host_does_not_allow() {
        let dir = tempfile::tempdir().unwrap();
        let local = Local {
            script_dir: dir.path().into(),
            log_dirs: vec![dir.path().into()],
            ..Local::default()
        };
        let params = params(&[("script", "missing")]);
        assert!(build("script", &params).is_ok());
        let error = build_local("script", &params, &local).err().unwrap();
        assert!(matches!(error, BuildError::Refused { kind: "script", .. }));
        assert!(error.to_string().contains("not installed"), "{error}");
        assert!(build_local("cpu", &BTreeMap::new(), &local).is_ok());
    }

    #[test]
    fn kinds_of_another_platform_are_refused_by_the_agent() {
        let (here, elsewhere) = if cfg!(windows) {
            (["eventlog", "windows_service"], ["log", "systemd"])
        } else {
            (["log", "systemd"], ["eventlog", "windows_service"])
        };
        for kind in KINDS {
            assert_eq!(supported(kind), !elsewhere.contains(kind), "{kind}");
        }
        assert!(here.iter().all(|k| supported(k)) && !supported("gpu"));
        let local = Local::default();
        for kind in elsewhere {
            let error = build_local(kind, &BTreeMap::new(), &local).err().unwrap();
            assert!(matches!(error, BuildError::Unsupported { .. }), "{error}");
            assert!(
                error.to_string().ends_with(&format!(
                    "is not supported on this platform ({})",
                    std::env::consts::OS
                )),
                "{error}"
            );
        }
    }

    /// The server validates rows for Windows agents with [`build`].
    #[cfg(unix)]
    #[tokio::test]
    async fn windows_kinds_are_validated_on_unix() {
        let eventlog = params(&[("channel", "System"), ("levels", "error")]);
        let sensor = build("eventlog", &eventlog).unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
        assert!(sensor.collect(deadline).await.unwrap_err().is_permanent());
        assert!(build("eventlog", &params(&[("channel", "Sys'tem")])).is_err());
        let services = params(&[("services", "W32Time,MSSQL$SQLEXPRESS")]);
        let sensor = build("windows_service", &services).unwrap();
        assert!(sensor.collect(deadline).await.unwrap_err().is_permanent());
        assert!(build("windows_service", &params(&[("services", "a\\b")])).is_err());
    }

    #[test]
    fn typed_params() {
        let map = params(&[("a", " true "), ("b", "yes"), ("l", "x, y"), ("e", "x,,y")]);
        let mut p = Params::new("cpu", &map).unwrap();
        assert_eq!(p.bool("a", false), Ok(true));
        assert_eq!(p.bool("missing", true), Ok(true));
        assert!(matches!(
            p.bool("b", true),
            Err(BuildError::BadParam { name: "b", .. })
        ));
        assert_eq!(p.list("l"), Ok(Some(vec!["x".into(), "y".into()])));
        assert_eq!(p.list("none"), Ok(None));
        assert!(p.list("e").is_err());
        assert!(p.finish().is_ok());
    }
}
