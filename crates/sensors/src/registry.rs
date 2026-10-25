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

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

#[cfg(unix)]
use crate::cpu::CpuSensor;
#[cfg(unix)]
use crate::disk::DiskSensor;
#[cfg(unix)]
use crate::log::{self, LogSensor};
#[cfg(unix)]
use crate::memory::MemorySensor;
use crate::port::PortSensor;
#[cfg(unix)]
use crate::script::{self, ScriptSensor};
#[cfg(unix)]
use crate::systemd::SystemdSensor;
use crate::Sensor;

/// Every kind [`build`] knows, in a stable order.
pub const KINDS: &[&str] = &["cpu", "disk", "log", "memory", "port", "script", "systemd"];

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
}

/// What the agent's own configuration allows sensors to touch. The
/// server's configuration names things inside these limits only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Local {
    /// Where the `script` sensor finds the scripts it may run.
    pub script_dir: PathBuf,
    /// Directories the `log` sensor may read files under.
    pub log_dirs: Vec<PathBuf>,
}

#[cfg(unix)]
impl Default for Local {
    fn default() -> Self {
        Self {
            script_dir: script::DEFAULT_DIR.into(),
            log_dirs: log::DEFAULT_DIRS.iter().map(PathBuf::from).collect(),
        }
    }
}

/// The Windows agent's defaults (ADR-0047).
#[cfg(windows)]
impl Default for Local {
    fn default() -> Self {
        Self {
            script_dir: r"C:\ProgramData\MonitorFlow\scripts.d".into(),
            log_dirs: Vec::new(),
        }
    }
}

/// Builds the sensor of kind `kind` with `params`, checking their syntax
/// only. The sensor uses the default [`Local`] limits.
pub fn build(kind: &str, params: &BTreeMap<String, String>) -> Result<Arc<dyn Sensor>, BuildError> {
    build_with(kind, params, None)
}

/// Like [`build`], within the `local` limits, and refuses with
/// [`BuildError::Refused`] what they do not allow.
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
    let sensor: Arc<dyn Sensor> = match params.kind {
        #[cfg(unix)]
        "cpu" => Arc::new(CpuSensor::new(params.bool("per_core", true)?)),
        #[cfg(unix)]
        "disk" => Arc::new(DiskSensor::from_params(&mut params)?),
        #[cfg(unix)]
        "log" => Arc::new(LogSensor::from_params(&mut params, local)?),
        #[cfg(unix)]
        "memory" => Arc::new(MemorySensor),
        "port" => Arc::new(PortSensor::from_params(&mut params)?),
        #[cfg(unix)]
        "script" => Arc::new(ScriptSensor::from_params(&mut params, local)?),
        #[cfg(unix)]
        "systemd" => Arc::new(SystemdSensor::from_params(&mut params)?),
        #[cfg(unix)]
        _ => unreachable!("Params::new only accepts KINDS"),
        // A kind without an implementation on this platform yet.
        #[cfg(not(unix))]
        _ => {
            let _ = local;
            return Err(params.refused("not available on this platform"));
        }
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
    fn new(kind: &str, map: &'a BTreeMap<String, String>) -> Result<Self, BuildError> {
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
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn bool(&mut self, name: &'static str, default: bool) -> Result<bool, BuildError> {
        match self.get(name) {
            None => Ok(default),
            Some("true") => Ok(true),
            Some("false") => Ok(false),
            Some(_) => Err(self.bad(name, "must be `true` or `false`")),
        }
    }

    /// A comma-separated list without empty items; `None` when absent.
    #[cfg_attr(not(unix), allow(dead_code))]
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

    fn finish(self) -> Result<(), BuildError> {
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

    #[cfg(unix)]
    #[test]
    fn every_kind_builds_with_defaults_or_asks_for_params() {
        let required: [(&str, &[(&str, &str)]); 4] = [
            ("log", &[("path", "/var/log/syslog"), ("pattern", "error")]),
            ("port", &[("port", "22")]),
            ("script", &[("script", "check")]),
            ("systemd", &[("units", "ssh")]),
        ];
        for kind in KINDS {
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

    #[cfg(unix)]
    #[test]
    fn unknown_kinds_and_params_are_refused() {
        let error = build("gpu", &BTreeMap::new()).err().unwrap();
        assert_eq!(error, BuildError::UnknownKind("gpu".into()));
        assert!(error
            .to_string()
            .contains("expected one of: cpu, disk, log, memory, port, script, systemd"));
        assert_eq!(
            build("cpu", &params(&[("per_cores", "true")])).err(),
            Some(BuildError::UnknownParam {
                kind: "cpu",
                name: "per_cores".into()
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn local_limits_refuse_what_the_host_does_not_allow() {
        let dir = tempfile::tempdir().unwrap();
        let local = Local {
            script_dir: dir.path().into(),
            log_dirs: vec![dir.path().into()],
        };
        let params = params(&[("script", "missing")]);
        assert!(build("script", &params).is_ok());
        let error = build_local("script", &params, &local).err().unwrap();
        assert!(matches!(error, BuildError::Refused { kind: "script", .. }));
        assert!(error.to_string().contains("not installed"), "{error}");
        assert!(build_local("cpu", &BTreeMap::new(), &local).is_ok());
    }

    #[cfg(not(unix))]
    #[test]
    fn kinds_without_an_implementation_are_refused() {
        let error = build_local("cpu", &BTreeMap::new(), &Local::default()).err();
        assert!(matches!(
            error,
            Some(BuildError::Refused { kind: "cpu", .. })
        ));
        assert!(build("port", &params(&[("port", "22")])).is_ok());
        let error = build("gpu", &BTreeMap::new()).err().unwrap();
        assert_eq!(error, BuildError::UnknownKind("gpu".into()));
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
