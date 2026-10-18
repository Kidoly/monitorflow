//! Builds the built-in sensors by kind. The configuration pushed by the
//! server names a kind (today the [`SensorConfig::name`](crate::SensorConfig::name),
//! like `cpu`) and its `params`; [`build`] checks both up front so a bad
//! configuration is refused when it arrives, not on the first tick.
//!
//! Parameters are strict: an unknown name is an error, so a typo does not
//! silently fall back to a default. See ADR-0015.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::cpu::CpuSensor;
use crate::Sensor;

/// Every kind [`build`] knows, in a stable order.
pub const KINDS: &[&str] = &["cpu"];

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
}

/// Builds the sensor of kind `kind` with `params`.
pub fn build(kind: &str, params: &BTreeMap<String, String>) -> Result<Arc<dyn Sensor>, BuildError> {
    let mut params = Params::new(kind, params)?;
    let sensor: Arc<dyn Sensor> = match params.kind {
        "cpu" => Arc::new(CpuSensor::new(params.bool("per_core", true)?)),
        _ => unreachable!("Params::new only accepts KINDS"),
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

    fn get(&mut self, name: &'static str) -> Option<&'a str> {
        self.used.insert(name);
        self.map.get(name).map(|v| v.trim())
    }

    fn bad(&self, name: &'static str, reason: impl Into<String>) -> BuildError {
        BuildError::BadParam {
            kind: self.kind,
            name,
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

    #[test]
    fn every_kind_builds_with_defaults() {
        for kind in KINDS {
            assert!(build(kind, &BTreeMap::new()).is_ok(), "{kind}");
        }
    }

    #[test]
    fn unknown_kinds_and_params_are_refused() {
        let error = build("gpu", &BTreeMap::new()).err().unwrap();
        assert_eq!(error, BuildError::UnknownKind("gpu".into()));
        assert!(error.to_string().contains("expected one of: cpu"));
        assert_eq!(
            build("cpu", &params(&[("per_cores", "true")])).err(),
            Some(BuildError::UnknownParam {
                kind: "cpu",
                name: "per_cores".into()
            })
        );
    }

    #[test]
    fn typed_params() {
        let map = params(&[("a", " true "), ("b", "yes")]);
        let mut p = Params::new("cpu", &map).unwrap();
        assert_eq!(p.bool("a", false), Ok(true));
        assert_eq!(p.bool("missing", true), Ok(true));
        assert!(matches!(
            p.bool("b", true),
            Err(BuildError::BadParam { name: "b", .. })
        ));
        assert!(p.finish().is_ok());
    }
}
