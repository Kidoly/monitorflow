//! Reading a check's parameters: each one read is marked, and any other
//! is refused, so a typo does not pass silently.

use std::collections::{BTreeMap, BTreeSet};

use monitorflow_sensors::BuildError;

/// The parameters of one check of `kind`.
#[derive(Debug)]
pub struct Params<'a> {
    kind: &'static str,
    map: &'a BTreeMap<String, String>,
    used: BTreeSet<&'static str>,
}

impl<'a> Params<'a> {
    pub fn new(kind: &'static str, map: &'a BTreeMap<String, String>) -> Self {
        Self {
            kind,
            map,
            used: BTreeSet::new(),
        }
    }

    /// The trimmed value of `name`, if given.
    pub fn get(&mut self, name: &'static str) -> Option<&'a str> {
        self.get_raw(name).map(str::trim)
    }

    /// The value of `name` as given, for a password.
    pub fn get_raw(&mut self, name: &'static str) -> Option<&'a str> {
        self.used.insert(name);
        self.map.get(name).map(String::as_str)
    }

    /// A number of `name` in `range`, or `default`.
    pub fn number(
        &mut self,
        name: &'static str,
        default: u32,
        range: std::ops::RangeInclusive<u32>,
    ) -> Result<u32, BuildError> {
        match self.get(name) {
            None => Ok(default),
            Some(value) => value
                .parse::<u32>()
                .ok()
                .filter(|v| range.contains(v))
                .ok_or_else(|| {
                    self.bad(
                        name,
                        format!(
                            "must be a whole number from {} to {}",
                            range.start(),
                            range.end()
                        ),
                    )
                }),
        }
    }

    pub fn bad(&self, name: &'static str, reason: impl Into<String>) -> BuildError {
        BuildError::BadParam {
            kind: self.kind,
            name,
            reason: reason.into(),
        }
    }

    pub fn refused(&self, reason: impl Into<String>) -> BuildError {
        BuildError::Refused {
            kind: self.kind,
            reason: reason.into(),
        }
    }

    /// Refuses parameters nobody read.
    pub fn finish(self) -> Result<(), BuildError> {
        match self.map.keys().find(|k| !self.used.contains(k.as_str())) {
            Some(name) => Err(BuildError::UnknownParam {
                kind: self.kind,
                name: name.clone(),
            }),
            None => Ok(()),
        }
    }
}
