//! Thresholds and their precedence (ADR-0017).
//!
//! The effective thresholds of a server are three layers keyed by
//! `(metric, instance)`: [`builtin_defaults`], then its site's rows, then
//! its own rows, like sensor configurations (ADR-0016). A disabled row
//! mutes what it replaces. When a sample is evaluated, a threshold for its
//! exact instance wins over one for every instance (`instance: None`),
//! muted or not.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde_json::json;

use super::Severity;
use crate::sensor_config::Layer;

/// How a value is compared with a threshold; the condition holds when
/// `value <comparison> threshold`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Comparison {
    Gt,
    Ge,
    Lt,
    Le,
}

impl Comparison {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::Lt => "<",
            Self::Le => "<=",
        }
    }

    /// Whether the alarm side is above the threshold (`>` and `>=`).
    pub fn above(self) -> bool {
        matches!(self, Self::Gt | Self::Ge)
    }

    /// Whether the alarm condition holds.
    pub fn holds(self, value: f64, threshold: f64) -> bool {
        match self {
            Self::Gt => value > threshold,
            Self::Ge => value >= threshold,
            Self::Lt => value < threshold,
            Self::Le => value <= threshold,
        }
    }

    /// Whether a value that crossed `threshold` is back past it by
    /// `hysteresis`. With no hysteresis this is the opposite of
    /// [`Self::holds`].
    pub fn recovered(self, value: f64, threshold: f64, hysteresis: f64) -> bool {
        if self.above() {
            !self.holds(value, threshold - hysteresis)
        } else {
            !self.holds(value, threshold + hysteresis)
        }
    }

    /// The worse of two values: the higher one for `>` and `>=`.
    pub fn worst(self, a: f64, b: f64) -> f64 {
        if self.above() {
            a.max(b)
        } else {
            a.min(b)
        }
    }
}

impl fmt::Display for Comparison {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown comparison `{0}`")]
pub struct UnknownComparison(String);

impl FromStr for Comparison {
    type Err = UnknownComparison;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            ">" => Ok(Self::Gt),
            ">=" => Ok(Self::Ge),
            "<" => Ok(Self::Lt),
            "<=" => Ok(Self::Le),
            other => Err(UnknownComparison(other.to_owned())),
        }
    }
}

/// One threshold on one metric.
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    /// Full metric name, like `monitorflow_disk_used_ratio`.
    pub metric: String,
    /// `None` matches every instance.
    pub instance: Option<String>,
    pub comparison: Comparison,
    pub warning: Option<f64>,
    pub critical: Option<f64>,
    /// Seconds the condition must hold before an incident opens or
    /// escalates.
    pub duration_s: u32,
    pub hysteresis: f64,
}

impl Rule {
    pub fn threshold(&self, severity: Severity) -> Option<f64> {
        match severity {
            Severity::Warning => self.warning,
            Severity::Critical => self.critical,
        }
    }

    pub fn duration_ms(&self) -> i64 {
        i64::from(self.duration_s) * 1000
    }

    /// The copy stored with an incident, so later edits do not rewrite
    /// why it opened.
    pub fn snapshot(&self) -> serde_json::Value {
        json!({
            "metric": self.metric,
            "instance": self.instance,
            "comparison": self.comparison.as_str(),
            "warning": self.warning,
            "critical": self.critical,
            "duration_s": self.duration_s,
            "hysteresis": self.hysteresis,
        })
    }

    fn key(&self) -> (String, Option<String>) {
        (self.metric.clone(), self.instance.clone())
    }
}

/// What every server is checked against unless its site or itself
/// overrides it.
pub fn builtin_defaults() -> Vec<Rule> {
    let rule =
        |metric: &str, instance: Option<&str>, comparison, warning, critical, minutes: u32| Rule {
            metric: metric.to_owned(),
            instance: instance.map(str::to_owned),
            comparison,
            warning,
            critical,
            duration_s: minutes * 60,
            hysteresis: 0.02,
        };
    vec![
        rule(
            "monitorflow_cpu_usage_ratio",
            Some("total"),
            Comparison::Gt,
            Some(0.9),
            None,
            10,
        ),
        rule(
            "monitorflow_disk_used_ratio",
            None,
            Comparison::Gt,
            Some(0.85),
            Some(0.95),
            5,
        ),
        rule(
            "monitorflow_memory_available_ratio",
            None,
            Comparison::Lt,
            Some(0.10),
            Some(0.05),
            5,
        ),
    ]
}

/// A stored row of `thresholds`.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub layer: Layer,
    pub rule: Rule,
    pub enabled: bool,
}

/// The effective thresholds of one server.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Rules {
    /// `None`: muted by a disabled row.
    entries: BTreeMap<(String, Option<String>), Option<Rule>>,
}

impl Rules {
    /// The threshold for a series, if one applies and is not muted.
    pub fn lookup(&self, metric: &str, instance: &str) -> Option<&Rule> {
        let exact = (metric.to_owned(), Some(instance.to_owned()));
        match self.entries.get(&exact) {
            Some(entry) => entry.as_ref(),
            None => self.entries.get(&(metric.to_owned(), None))?.as_ref(),
        }
    }

    /// The thresholds in force, sorted by `(metric, instance)`.
    pub fn enabled(&self) -> impl Iterator<Item = &Rule> {
        self.entries.values().flatten()
    }
}

/// The built-in defaults overlaid with `rows`, site rows before server
/// rows.
pub fn resolve(mut rows: Vec<Row>) -> Rules {
    let mut entries: BTreeMap<_, _> = builtin_defaults()
        .into_iter()
        .map(|rule| (rule.key(), Some(rule)))
        .collect();
    rows.sort_by_key(|row| row.layer);
    for row in rows {
        entries.insert(row.rule.key(), row.enabled.then_some(row.rule));
    }
    Rules { entries }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(metric: &str, instance: Option<&str>, warning: f64) -> Rule {
        Rule {
            metric: metric.into(),
            instance: instance.map(Into::into),
            comparison: Comparison::Gt,
            warning: Some(warning),
            critical: None,
            duration_s: 0,
            hysteresis: 0.0,
        }
    }

    fn row(layer: Layer, metric: &str, instance: Option<&str>, warning: f64, enabled: bool) -> Row {
        Row {
            layer,
            rule: rule(metric, instance, warning),
            enabled,
        }
    }

    fn warning(rules: &Rules, metric: &str, instance: &str) -> Option<f64> {
        rules.lookup(metric, instance).and_then(|r| r.warning)
    }

    const DISK: &str = "monitorflow_disk_used_ratio";
    const CPU: &str = "monitorflow_cpu_usage_ratio";

    #[test]
    fn comparisons_hold_and_recover_with_hysteresis() {
        use Comparison::*;
        assert!(Gt.holds(0.9, 0.85) && !Gt.holds(0.85, 0.85));
        assert!(Ge.holds(0.85, 0.85) && !Ge.holds(0.84, 0.85));
        assert!(Lt.holds(0.04, 0.05) && !Lt.holds(0.05, 0.05));
        assert!(Le.holds(0.05, 0.05) && !Le.holds(0.06, 0.05));
        // Above-thresholds recover below threshold - hysteresis.
        assert!(!Gt.recovered(0.84, 0.85, 0.02));
        assert!(Gt.recovered(0.83, 0.85, 0.02));
        assert!(!Ge.recovered(0.83, 0.85, 0.02));
        assert!(Ge.recovered(0.829, 0.85, 0.02));
        // Below-thresholds recover above threshold + hysteresis.
        assert!(!Lt.recovered(0.06, 0.05, 0.02));
        assert!(Lt.recovered(0.07, 0.05, 0.02));
        assert!(!Le.recovered(0.07, 0.05, 0.02));
        // Without hysteresis, recovered is the opposite of holds.
        assert!(Gt.recovered(0.85, 0.85, 0.0) && !Gt.recovered(0.851, 0.85, 0.0));
        assert_eq!((Gt.worst(1.0, 2.0), Lt.worst(1.0, 2.0)), (2.0, 1.0));
        for c in [Gt, Ge, Lt, Le] {
            assert_eq!(c.as_str().parse(), Ok(c));
        }
        assert!("=".parse::<Comparison>().is_err());
    }

    #[test]
    fn defaults_apply_without_rows() {
        let rules = resolve(Vec::new());
        assert_eq!(warning(&rules, DISK, "/var"), Some(0.85));
        assert_eq!(warning(&rules, CPU, "total"), Some(0.9));
        // The CPU default only watches the total, not each core.
        assert_eq!(rules.lookup(CPU, "0"), None);
        let memory = rules
            .lookup("monitorflow_memory_available_ratio", "")
            .unwrap();
        assert_eq!(
            (memory.comparison, memory.critical, memory.duration_s),
            (Comparison::Lt, Some(0.05), 300)
        );
        assert_eq!(rules.lookup("monitorflow_cpu_load1", ""), None);
    }

    #[test]
    fn server_rows_win_over_site_rows_which_win_over_defaults() {
        let rules = resolve(vec![
            // Given in the "wrong" order: the layer decides.
            row(Layer::Server, DISK, None, 0.7, true),
            row(Layer::Site, DISK, None, 0.8, true),
            row(Layer::Site, CPU, Some("total"), 0.5, false),
            row(Layer::Server, CPU, Some("total"), 0.6, true),
            row(
                Layer::Site,
                "monitorflow_memory_available_ratio",
                None,
                0.0,
                false,
            ),
        ]);
        assert_eq!(warning(&rules, DISK, "/"), Some(0.7));
        assert_eq!(warning(&rules, CPU, "total"), Some(0.6));
        assert_eq!(rules.lookup("monitorflow_memory_available_ratio", ""), None);
        assert_eq!(rules.enabled().count(), 2);
    }

    #[test]
    fn exact_instances_win_over_every_instance_even_when_muted() {
        let rules = resolve(vec![
            row(Layer::Site, DISK, Some("/backup"), 0.98, true),
            row(Layer::Server, DISK, Some("/boot"), 0.0, false),
        ]);
        assert_eq!(warning(&rules, DISK, "/backup"), Some(0.98));
        assert_eq!(warning(&rules, DISK, "/"), Some(0.85));
        assert_eq!(rules.lookup(DISK, "/boot"), None);

        // Muting every instance leaves the exact ones.
        let rules = resolve(vec![
            row(Layer::Site, DISK, Some("/data"), 0.9, true),
            row(Layer::Server, DISK, None, 0.0, false),
        ]);
        assert_eq!(rules.lookup(DISK, "/"), None);
        assert_eq!(warning(&rules, DISK, "/data"), Some(0.9));
    }

    #[test]
    fn snapshots_keep_every_field() {
        let snapshot = builtin_defaults()[1].snapshot();
        assert_eq!(snapshot["metric"], DISK);
        assert_eq!(snapshot["instance"], serde_json::Value::Null);
        assert_eq!(snapshot["comparison"], ">");
        assert_eq!(snapshot["critical"], 0.95);
        assert_eq!(snapshot["duration_s"], 300);
    }
}
