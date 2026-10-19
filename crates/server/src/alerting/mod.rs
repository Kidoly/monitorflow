//! The threshold engine (ADR-0017): thresholds per site and server, the
//! state machine of each series, and incidents in PostgreSQL.

pub mod rules;
pub mod state;

use std::fmt;
use std::str::FromStr;

pub use rules::{builtin_defaults, resolve, Comparison, Row, Rule, Rules};
pub use state::{Evaluation, SeriesState, Transition};

/// Severity of an incident; critical is the higher one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Warning,
    Critical,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown severity `{0}`")]
pub struct UnknownSeverity(String);

impl FromStr for Severity {
    type Err = UnknownSeverity;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "warning" => Ok(Self::Warning),
            "critical" => Ok(Self::Critical),
            other => Err(UnknownSeverity(other.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn critical_is_above_warning() {
        assert!(Severity::Critical > Severity::Warning);
        assert!(Some(Severity::Warning) > None);
        for s in [Severity::Warning, Severity::Critical] {
            assert_eq!(s.as_str().parse(), Ok(s));
        }
        assert!("info".parse::<Severity>().is_err());
    }
}
