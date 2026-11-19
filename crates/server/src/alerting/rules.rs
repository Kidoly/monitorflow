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

/// What a rule judges at each evaluation (ADR-0153).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Kind {
    /// The sample itself.
    #[default]
    Value,
    /// A [`Function`] of the samples of a window.
    Window,
    /// The window's average against the same window `offset_s` earlier.
    Compare,
    /// Seconds left until the value reaches `target`.
    Forecast,
    /// Seconds since the last sample arrived.
    NoData,
}

impl Kind {
    pub const ALL: [Kind; 5] = [
        Self::Value,
        Self::Window,
        Self::Compare,
        Self::Forecast,
        Self::NoData,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Value => "value",
            Self::Window => "window",
            Self::Compare => "compare",
            Self::Forecast => "forecast",
            Self::NoData => "nodata",
        }
    }

    /// Whether the rule reads a summary of its series' past.
    pub fn needs_summary(self) -> bool {
        matches!(self, Self::Window | Self::Compare | Self::Forecast)
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown rule kind `{0}`")]
pub struct UnknownKind(String);

impl FromStr for Kind {
    type Err = UnknownKind;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|k| k.as_str() == s)
            .ok_or_else(|| UnknownKind(s.to_owned()))
    }
}

/// The function of a `window` rule or condition, or how a `compare` rule
/// compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Function {
    Avg,
    Min,
    Max,
    /// The last sample minus the first one of the window.
    Change,
    /// The change per second.
    Rate,
    /// Now divided by then.
    Ratio,
    /// Now minus then.
    Difference,
}

impl Function {
    pub const ALL: [Function; 7] = [
        Self::Avg,
        Self::Min,
        Self::Max,
        Self::Change,
        Self::Rate,
        Self::Ratio,
        Self::Difference,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Avg => "avg",
            Self::Min => "min",
            Self::Max => "max",
            Self::Change => "change",
            Self::Rate => "rate",
            Self::Ratio => "ratio",
            Self::Difference => "difference",
        }
    }

    /// Whether a `window` rule or a condition may use it.
    pub fn over_window(self) -> bool {
        !matches!(self, Self::Ratio | Self::Difference)
    }
}

impl fmt::Display for Function {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown function `{0}`")]
pub struct UnknownFunction(String);

impl FromStr for Function {
    type Err = UnknownFunction;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|f| f.as_str() == s)
            .ok_or_else(|| UnknownFunction(s.to_owned()))
    }
}

/// A condition joined with "and" to a rule (ADR-0153): a metric of the same
/// server past one limit.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Condition {
    pub metric: String,
    /// `None`: the rule's series' instance, else the series without one.
    #[serde(default)]
    pub instance: Option<String>,
    /// `None`: the latest sample; else a window function.
    #[serde(default, with = "function_name")]
    pub function: Option<Function>,
    /// Seconds, with a function.
    #[serde(default)]
    pub window_s: Option<u32>,
    #[serde(with = "comparison_text")]
    pub comparison: Comparison,
    pub value: f64,
}

mod function_name {
    use super::Function;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(f: &Option<Function>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(f.map_or("value", Function::as_str))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Function>, D::Error> {
        let text = Option::<String>::deserialize(d)?;
        match text.as_deref() {
            None | Some("value") => Ok(None),
            Some(other) => other.parse().map(Some).map_err(serde::de::Error::custom),
        }
    }
}

mod comparison_text {
    use super::Comparison;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(c: &Comparison, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(c.as_str())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Comparison, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// One threshold on one metric.
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    /// Full metric name, like `monitorflow_disk_used_ratio`.
    pub metric: String,
    /// `None` matches every instance.
    pub instance: Option<String>,
    pub kind: Kind,
    /// How a value is compared with the limits; for a forecast, the side
    /// of `target` the value is heading to.
    pub comparison: Comparison,
    pub warning: Option<f64>,
    pub critical: Option<f64>,
    /// Seconds the condition must hold before an incident opens or
    /// escalates.
    pub duration_s: u32,
    pub hysteresis: f64,
    /// `window` and `compare` rules.
    pub function: Option<Function>,
    /// Seconds: the window of `window` and `compare`, the learning window
    /// of `forecast`.
    pub window_s: u32,
    /// Seconds back for `compare`.
    pub offset_s: u32,
    /// The value a forecast is about.
    pub target: Option<f64>,
    pub conditions: Vec<Condition>,
}

impl Default for Rule {
    fn default() -> Self {
        Self {
            metric: String::new(),
            instance: None,
            kind: Kind::Value,
            comparison: Comparison::Gt,
            warning: None,
            critical: None,
            duration_s: 0,
            hysteresis: 0.0,
            function: None,
            window_s: 0,
            offset_s: 0,
            target: None,
            conditions: Vec::new(),
        }
    }
}

/// Longest time left a forecast reports: 10 years, so stored values stay
/// finite. Never an alarm.
pub const FOREVER_S: f64 = 10.0 * 365.0 * 86_400.0;

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

    pub fn window_ms(&self) -> i64 {
        i64::from(self.window_s) * 1000
    }

    /// How the judged number is compared with the limits: time left alarms
    /// when low, silence when long, others as the row says.
    pub fn judged(&self) -> Comparison {
        match self.kind {
            Kind::Forecast => Comparison::Le,
            Kind::NoData => Comparison::Gt,
            _ => self.comparison,
        }
    }

    /// The copy stored with an incident, so later edits do not rewrite
    /// why it opened.
    pub fn snapshot(&self) -> serde_json::Value {
        let mut snapshot = json!({
            "metric": self.metric,
            "instance": self.instance,
            "comparison": self.comparison.as_str(),
            "warning": self.warning,
            "critical": self.critical,
            "duration_s": self.duration_s,
            "hysteresis": self.hysteresis,
        });
        if self.kind != Kind::Value {
            snapshot["kind"] = json!(self.kind.as_str());
            snapshot["function"] = json!(self.function.map(Function::as_str));
            snapshot["window_s"] = json!((self.window_s > 0).then_some(self.window_s));
            snapshot["offset_s"] = json!((self.offset_s > 0).then_some(self.offset_s));
            snapshot["target"] = json!(self.target);
        }
        if !self.conditions.is_empty() {
            snapshot["conditions"] = json!(self.conditions);
        }
        snapshot
    }

    fn key(&self) -> (String, Option<String>, Kind) {
        (self.metric.clone(), self.instance.clone(), self.kind)
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
            ..Rule::default()
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
        // A disk whose I/Os take over 200 ms on average for 10 minutes is
        // slowing what runs on it, whatever its kind; a busy disk that
        // still answers fast is not a problem (ADR-0092).
        rule(
            "monitorflow_diskio_wait_seconds",
            None,
            Comparison::Gt,
            Some(0.2),
            None,
            10,
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
    entries: BTreeMap<(String, Option<String>, Kind), Option<Rule>>,
}

impl Rules {
    /// The rule of `kind` for a series, if one applies and is not muted.
    pub fn lookup_kind(&self, metric: &str, instance: &str, kind: Kind) -> Option<&Rule> {
        let exact = (metric.to_owned(), Some(instance.to_owned()), kind);
        match self.entries.get(&exact) {
            Some(entry) => entry.as_ref(),
            None => self.entries.get(&(metric.to_owned(), None, kind))?.as_ref(),
        }
    }

    /// The `value` threshold for a series, if one applies and is not muted.
    pub fn lookup(&self, metric: &str, instance: &str) -> Option<&Rule> {
        self.lookup_kind(metric, instance, Kind::Value)
    }

    /// Every rule that applies to a series, one per kind.
    pub fn matching<'a>(
        &'a self,
        metric: &'a str,
        instance: &'a str,
    ) -> impl Iterator<Item = &'a Rule> + 'a {
        Kind::ALL
            .into_iter()
            .filter_map(move |kind| self.lookup_kind(metric, instance, kind))
    }

    /// The thresholds in force, sorted by `(metric, instance, kind)`.
    pub fn enabled(&self) -> impl Iterator<Item = &Rule> {
        self.entries.values().flatten()
    }

    /// Every key with its threshold, `None` when muted, sorted by
    /// `(metric, instance, kind)`. For the API's view of the layers.
    pub(crate) fn entries(
        &self,
    ) -> impl Iterator<Item = (&str, Option<&str>, Kind, Option<&Rule>)> {
        self.entries.iter().map(|((metric, instance, kind), rule)| {
            (metric.as_str(), instance.as_deref(), *kind, rule.as_ref())
        })
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
            warning: Some(warning),
            ..Rule::default()
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
        let wait = rules
            .lookup("monitorflow_diskio_wait_seconds", "sda")
            .unwrap();
        assert_eq!(
            (wait.warning, wait.critical, wait.duration_s),
            (Some(0.2), None, 600)
        );
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
        assert_eq!(rules.enabled().count(), 3);
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

    #[test]
    fn kinds_of_one_series_live_side_by_side() {
        let forecast = Rule {
            metric: DISK.into(),
            kind: Kind::Forecast,
            comparison: Comparison::Ge,
            target: Some(1.0),
            window_s: 86_400,
            warning: Some(259_200.0),
            ..Rule::default()
        };
        let rules = resolve(vec![
            Row {
                layer: Layer::Site,
                rule: forecast.clone(),
                enabled: true,
            },
            row(Layer::Server, DISK, Some("/tmp"), 0.0, false),
        ]);
        let kinds: Vec<_> = rules.matching(DISK, "/").map(|r| r.kind).collect();
        assert_eq!(kinds, [Kind::Value, Kind::Forecast]);
        // Muting the value threshold of /tmp leaves its forecast.
        let kinds: Vec<_> = rules.matching(DISK, "/tmp").map(|r| r.kind).collect();
        assert_eq!(kinds, [Kind::Forecast]);
        assert_eq!(forecast.judged(), Comparison::Le);
        let snapshot = forecast.snapshot();
        assert_eq!(snapshot["kind"], "forecast");
        assert_eq!(snapshot["target"], 1.0);
        assert_eq!(snapshot["comparison"], ">=");
        // A plain threshold's snapshot is as before.
        assert!(builtin_defaults()[0].snapshot().get("kind").is_none());
        for kind in Kind::ALL {
            assert_eq!(kind.as_str().parse(), Ok(kind));
        }
        for function in Function::ALL {
            assert_eq!(function.as_str().parse(), Ok(function));
        }
        assert!("median".parse::<Function>().is_err());
    }

    #[test]
    fn conditions_read_and_write_plain_json() {
        let condition: Condition = serde_json::from_value(json!({
            "metric": "monitorflow_cpu_load1", "comparison": ">", "value": 8
        }))
        .unwrap();
        assert_eq!(
            (condition.function, condition.instance.clone()),
            (None, None)
        );
        assert_eq!(
            serde_json::to_value(&condition).unwrap()["function"],
            "value"
        );
        let windowed: Condition = serde_json::from_value(json!({
            "metric": "monitorflow_cpu_load1", "function": "avg", "window_s": 300,
            "comparison": "<=", "value": 1
        }))
        .unwrap();
        assert_eq!(windowed.function, Some(Function::Avg));
        assert_eq!(windowed.comparison, Comparison::Le);
        for bad in [
            json!({"metric": "m", "comparison": "=", "value": 1}),
            json!({"metric": "m", "comparison": ">", "value": 1, "extra": 1}),
            json!({"metric": "m", "function": "median", "comparison": ">", "value": 1}),
        ] {
            assert!(serde_json::from_value::<Condition>(bad).is_err());
        }
    }
}
