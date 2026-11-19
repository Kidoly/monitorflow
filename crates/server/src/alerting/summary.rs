//! What a richer rule judges (ADR-0153): a summary of a series' recent
//! samples in at most [`BUCKETS`] buckets, the window functions and the
//! forecast read from it, and [`derive`], which turns a sample into the
//! number the state machine judges for each kind of rule.
//!
//! Everything here is pure: the engine and the preview feed the same
//! samples through the same code.

use std::collections::{BTreeMap, VecDeque};

use super::rules::{Comparison, Function, Kind, Rule, Rules, FOREVER_S};

/// Buckets of a summary: a window edge is precise to this fraction.
pub const BUCKETS: i64 = 60;
/// The narrowest bucket, in milliseconds.
pub const MIN_BUCKET_MS: i64 = 10_000;

/// The width of a bucket for a summary covering `span_ms`.
pub fn bucket_width(span_ms: i64) -> i64 {
    (span_ms / BUCKETS).max(MIN_BUCKET_MS)
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Bucket {
    /// Start, a multiple of the width.
    start: i64,
    count: u32,
    sum: f64,
    min: f64,
    max: f64,
    first: (i64, f64),
    last: (i64, f64),
}

impl Bucket {
    fn new(start: i64, at: i64, value: f64) -> Self {
        Self {
            start,
            count: 1,
            sum: value,
            min: value,
            max: value,
            first: (at, value),
            last: (at, value),
        }
    }

    fn add(&mut self, at: i64, value: f64) {
        self.count += 1;
        self.sum += value;
        self.min = self.min.min(value);
        self.max = self.max.max(value);
        self.last = (at, value);
    }

    fn mean(&self) -> f64 {
        self.sum / f64::from(self.count)
    }
}

/// The recent samples of one series, in buckets.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    width: i64,
    span: i64,
    buckets: VecDeque<Bucket>,
    /// Since when the summary knows the series: the start of loaded
    /// history, or the first sample.
    known_from: Option<i64>,
    last_at: Option<i64>,
}

impl Summary {
    /// A summary able to answer windows up to `span_ms`.
    pub fn new(span_ms: i64) -> Self {
        Self {
            width: bucket_width(span_ms),
            span: span_ms.max(MIN_BUCKET_MS),
            buckets: VecDeque::new(),
            known_from: None,
            last_at: None,
        }
    }

    pub fn span_ms(&self) -> i64 {
        self.span
    }

    pub fn width_ms(&self) -> i64 {
        self.width
    }

    pub fn last_at(&self) -> Option<i64> {
        self.last_at
    }

    /// The latest sample.
    pub fn last(&self) -> Option<(i64, f64)> {
        self.buckets.back().map(|b| b.last)
    }

    /// Adds a sample. Returns `false` for one not newer than the last,
    /// which is ignored.
    pub fn push(&mut self, at: i64, value: f64) -> bool {
        if self.last_at.is_some_and(|last| at <= last) || !value.is_finite() {
            return false;
        }
        self.last_at = Some(at);
        self.known_from.get_or_insert(at);
        let start = at.div_euclid(self.width) * self.width;
        match self.buckets.back_mut() {
            Some(bucket) if bucket.start == start => bucket.add(at, value),
            _ => self.buckets.push_back(Bucket::new(start, at, value)),
        }
        let oldest = at - self.span - self.width;
        while self.buckets.front().is_some_and(|b| b.start < oldest) {
            self.buckets.pop_front();
        }
        true
    }

    /// Adds history loaded from VictoriaMetrics that starts at `from`:
    /// one value per bucket, each counting as one sample, kept only where
    /// it is older than the first live bucket.
    pub fn load(&mut self, from: i64, points: &[(i64, f64)]) {
        let first_live = self.buckets.front().map(|b| b.start);
        let mut loaded: Vec<Bucket> = Vec::new();
        for &(at, value) in points {
            if !value.is_finite() {
                continue;
            }
            let start = at.div_euclid(self.width) * self.width;
            if first_live.is_some_and(|live| start >= live)
                || loaded.last().is_some_and(|b| b.start >= start)
            {
                continue;
            }
            loaded.push(Bucket::new(start, at, value));
        }
        for bucket in loaded.into_iter().rev() {
            self.buckets.push_front(bucket);
        }
        if self.last_at.is_none() {
            self.last_at = self.buckets.back().map(|b| b.last.0);
        }
        self.known_from = Some(self.known_from.map_or(from, |k| k.min(from)));
        if let Some(last) = self.last_at {
            let oldest = last - self.span - self.width;
            while self.buckets.front().is_some_and(|b| b.start < oldest) {
                self.buckets.pop_front();
            }
        }
    }

    /// Whether the summary knows at least `needed_ms` before `at`.
    fn knows(&self, at: i64, needed_ms: i64) -> bool {
        self.known_from.is_some_and(|k| k <= at - needed_ms)
    }

    /// The buckets with a sample in the window `(at - window_ms, at]`:
    /// their last sample is in it. A bucket across the window's start
    /// counts whole, so an edge is precise to a bucket.
    fn window(&self, at: i64, window_ms: i64) -> impl Iterator<Item = &Bucket> {
        let from = at - window_ms;
        self.buckets
            .iter()
            .filter(move |b| b.last.0 > from && b.start <= at)
    }

    /// A window function at `at`, or `None` when the summary knows less
    /// than half of the window or has too few samples in it.
    pub fn function(&self, function: Function, at: i64, window_ms: i64) -> Option<f64> {
        if !self.knows(at, window_ms / 2) {
            return None;
        }
        let mut buckets = self.window(at, window_ms).peekable();
        let first = **buckets.peek()?;
        let (mut count, mut sum, mut min, mut max, mut last) =
            (0u64, 0.0, f64::MAX, f64::MIN, first);
        let mut samples = 0u64;
        for bucket in buckets {
            count += u64::from(bucket.count);
            samples += u64::from(bucket.count);
            sum += bucket.sum;
            min = min.min(bucket.min);
            max = max.max(bucket.max);
            last = *bucket;
        }
        let change = || (samples >= 2).then_some(last.last.1 - first.first.1);
        match function {
            Function::Avg => Some(sum / count as f64),
            Function::Min => Some(min),
            Function::Max => Some(max),
            Function::Change => change(),
            Function::Rate => {
                let seconds = (last.last.0 - first.first.0) as f64 / 1000.0;
                change().filter(|_| seconds > 0.0).map(|c| c / seconds)
            }
            Function::Ratio | Function::Difference => None,
        }
    }

    /// Seconds left at `at` until the value reaches `target` on the side
    /// `side` names, from a least-squares line through the bucket means of
    /// the last `window_ms`, weighted by their counts. 0 when the latest
    /// sample is there already, [`FOREVER_S`] when the line never gets
    /// there; `None` without samples over a quarter of the window in at
    /// least 3 buckets.
    pub fn forecast(&self, at: i64, window_ms: i64, target: f64, side: Comparison) -> Option<f64> {
        if !self.knows(at, window_ms / 4) {
            return None;
        }
        let points: Vec<(f64, f64, f64)> = self
            .window(at, window_ms)
            .map(|b| {
                let t = (b.first.0 + b.last.0) as f64 / 2.0;
                (t, b.mean(), f64::from(b.count))
            })
            .collect();
        let (first, last) = (points.first()?, points.last()?);
        if points.len() < 3 || last.0 - first.0 < window_ms as f64 / 4.0 {
            return None;
        }
        let (_, latest) = self.last()?;
        if side.holds(latest, target) {
            return Some(0.0);
        }
        let origin = first.0;
        let weight: f64 = points.iter().map(|p| p.2).sum();
        let mean_t = points.iter().map(|p| (p.0 - origin) * p.2).sum::<f64>() / weight;
        let mean_v = points.iter().map(|p| p.1 * p.2).sum::<f64>() / weight;
        let (mut sxy, mut sxx) = (0.0, 0.0);
        for &(t, v, w) in &points {
            let dt = t - origin - mean_t;
            sxy += w * dt * (v - mean_v);
            sxx += w * dt * dt;
        }
        if sxx <= 0.0 {
            return Some(FOREVER_S);
        }
        // Value per millisecond.
        let slope = sxy / sxx;
        let towards = if side.above() {
            slope > 0.0
        } else {
            slope < 0.0
        };
        if !towards || !slope.is_finite() {
            return Some(FOREVER_S);
        }
        let reach = origin + mean_t + (target - mean_v) / slope;
        let left = (reach - at as f64) / 1000.0;
        Some(left.clamp(0.0, FOREVER_S))
    }
}

/// The span of summary `rule` needs: its window, plus what a comparison
/// with the past needs now.
pub fn summary_span_ms(rule: &Rule) -> Option<i64> {
    rule.kind.needs_summary().then(|| rule.window_ms())
}

/// The number the state machine judges for `rule` at a sample `value`
/// taken at `at`: the sample, a window function, a comparison with
/// `past` (the window's average `offset_s` earlier) or the time left.
/// `None` when it cannot be judged yet. `nodata` is judged by the engine's
/// clock, not here.
pub fn derive(
    rule: &Rule,
    summary: Option<&Summary>,
    at: i64,
    value: f64,
    past: Option<f64>,
) -> Option<f64> {
    match rule.kind {
        Kind::Value => Some(value),
        Kind::Window => summary?.function(rule.function?, at, rule.window_ms()),
        Kind::Compare => {
            let now = summary?.function(Function::Avg, at, rule.window_ms())?;
            let past = past?;
            match rule.function? {
                Function::Ratio => (past != 0.0).then(|| now / past),
                Function::Difference => Some(now - past),
                _ => None,
            }
        }
        Kind::Forecast => summary?.forecast(at, rule.window_ms(), rule.target?, rule.comparison),
        Kind::NoData => None,
    }
}

/// What a server's rules need kept of one metric's series (ADR-0153).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Need {
    /// The longest window a rule or condition reads, 0 for none.
    pub span_ms: i64,
    /// Every window read, shortest first: each gets a summary of its own,
    /// so a window of minutes is not read from buckets sized for a day's
    /// forecast of the same metric.
    pub spans: Vec<i64>,
    /// How history fills its buckets: `Max` or `Min` when every reader
    /// wants that, `Avg` otherwise.
    pub seed: Function,
    /// A condition reads the latest value.
    pub gate: bool,
    /// A `nodata` rule watches when samples arrive.
    pub arrival: bool,
}

/// What each metric of a server needs, from its rules in force.
pub fn needs(rules: &Rules) -> BTreeMap<String, Need> {
    let mut needs: BTreeMap<String, (Need, Vec<Function>)> = BTreeMap::new();
    let mut add = |metric: &str, span_ms: i64, function: Option<Function>, gate, arrival| {
        let (need, functions) = needs.entry(metric.to_owned()).or_insert((
            Need {
                span_ms: 0,
                spans: Vec::new(),
                seed: Function::Avg,
                gate: false,
                arrival: false,
            },
            Vec::new(),
        ));
        need.span_ms = need.span_ms.max(span_ms);
        if span_ms > 0 && !need.spans.contains(&span_ms) {
            need.spans.push(span_ms);
            need.spans.sort_unstable();
        }
        need.gate |= gate;
        need.arrival |= arrival;
        functions.extend(function);
    };
    for rule in rules.enabled() {
        match rule.kind {
            Kind::Value => {}
            Kind::NoData => add(&rule.metric, 0, None, false, true),
            Kind::Window => add(&rule.metric, rule.window_ms(), rule.function, false, false),
            Kind::Compare | Kind::Forecast => add(
                &rule.metric,
                rule.window_ms(),
                Some(Function::Avg),
                false,
                false,
            ),
        }
        for condition in &rule.conditions {
            let span = condition
                .function
                .map_or(0, |_| i64::from(condition.window_s.unwrap_or(0)) * 1000);
            add(&condition.metric, span, condition.function, true, false);
        }
    }
    needs
        .into_iter()
        .map(|(metric, (mut need, functions))| {
            let only = |f| !functions.is_empty() && functions.iter().all(|&g| g == f);
            need.seed = if only(Function::Max) {
                Function::Max
            } else if only(Function::Min) {
                Function::Min
            } else {
                Function::Avg
            };
            (metric, need)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: i64 = 1000;
    const MIN: i64 = 60 * S;
    const HOUR: i64 = 60 * MIN;
    const DAY: i64 = 24 * HOUR;

    fn fed(span: i64, samples: impl IntoIterator<Item = (i64, f64)>) -> Summary {
        let mut summary = Summary::new(span);
        for (at, v) in samples {
            assert!(summary.push(at, v));
        }
        summary
    }

    #[test]
    fn buckets_are_a_sixtieth_of_the_span_and_never_below_ten_seconds() {
        assert_eq!(bucket_width(HOUR), MIN);
        assert_eq!(bucket_width(5 * MIN), 10 * S);
        assert_eq!(bucket_width(MIN), 10 * S);
        assert_eq!(bucket_width(7 * DAY), 7 * DAY / 60);
    }

    #[test]
    fn window_functions_read_the_window() {
        // One sample a minute for an hour: 0, 1, 2, ... 59.
        let summary = fed(HOUR, (0..60).map(|i| (i * MIN, i as f64)));
        let at = 59 * MIN;
        let f = |function, window| summary.function(function, at, window);
        // The last 10 minutes: 50 to 59.
        assert_eq!(f(Function::Avg, 10 * MIN), Some(54.5));
        assert_eq!(f(Function::Min, 10 * MIN), Some(50.0));
        assert_eq!(f(Function::Max, 10 * MIN), Some(59.0));
        assert_eq!(f(Function::Change, 10 * MIN), Some(9.0));
        assert_eq!(f(Function::Rate, 10 * MIN), Some(9.0 / 540.0));
        assert_eq!(f(Function::Avg, HOUR), Some(29.5));
        assert_eq!(f(Function::Ratio, HOUR), None);
    }

    #[test]
    fn a_spike_moves_the_average_little_and_the_maximum_fully() {
        let mut samples: Vec<_> = (0..15).map(|i| (i * MIN, 1.0)).collect();
        samples[14].1 = 16.0;
        let summary = fed(15 * MIN, samples);
        let at = 14 * MIN;
        assert_eq!(summary.function(Function::Avg, at, 15 * MIN), Some(2.0));
        assert_eq!(summary.function(Function::Max, at, 15 * MIN), Some(16.0));
    }

    #[test]
    fn a_window_needs_half_of_it_known_and_change_two_samples() {
        let mut summary = Summary::new(HOUR);
        summary.push(0, 1.0);
        assert_eq!(summary.function(Function::Avg, 0, HOUR), None);
        summary.push(29 * MIN, 1.0);
        assert_eq!(summary.function(Function::Avg, 29 * MIN, HOUR), None);
        summary.push(30 * MIN, 3.0);
        assert_eq!(
            summary.function(Function::Avg, 30 * MIN, HOUR),
            Some(5.0 / 3.0)
        );
        // One sample in the window: no change, no rate.
        let mut summary = Summary::new(10 * MIN);
        summary.push(0, 1.0);
        summary.push(30 * MIN, 2.0);
        assert_eq!(summary.function(Function::Change, 30 * MIN, 10 * MIN), None);
        assert_eq!(
            summary.function(Function::Avg, 30 * MIN, 10 * MIN),
            Some(2.0)
        );
        // Nothing in the window: no value at all.
        assert_eq!(summary.function(Function::Avg, 50 * MIN, 10 * MIN), None);
    }

    #[test]
    fn old_samples_are_ignored_and_old_buckets_dropped() {
        let mut summary = fed(10 * MIN, (0..120).map(|i| (i * MIN, i as f64)));
        assert!(!summary.push(119 * MIN, 0.0));
        assert!(!summary.push(MIN, 0.0));
        assert!(!summary.push(200 * MIN, f64::NAN));
        assert!(summary.buckets.len() <= (BUCKETS + 2) as usize);
        assert_eq!(
            summary.function(Function::Min, 119 * MIN, 10 * MIN),
            Some(110.0)
        );
    }

    #[test]
    fn loaded_history_fills_what_is_older_than_live_samples() {
        let mut summary = Summary::new(HOUR);
        summary.push(60 * MIN, 10.0);
        // Before the load, half an hour is not known.
        assert_eq!(summary.function(Function::Avg, 60 * MIN, HOUR), None);
        let history: Vec<_> = (0..=60).map(|i| (i * MIN, 4.0)).collect();
        summary.load(0, &history);
        // The point of the live bucket is skipped, and the one at the
        // window's start is outside it: 59 loaded and 1 live.
        assert_eq!(summary.function(Function::Max, 60 * MIN, HOUR), Some(10.0));
        let avg = summary.function(Function::Avg, 60 * MIN, HOUR).unwrap();
        assert!((avg - (4.0 * 59.0 + 10.0) / 60.0).abs() < 1e-9, "{avg}");
        // A load into an empty summary, then live samples go on from it.
        let mut summary = Summary::new(HOUR);
        summary.load(0, &[(0, 1.0), (30 * MIN, 2.0)]);
        assert_eq!(summary.last_at(), Some(30 * MIN));
        assert!(!summary.push(30 * MIN, 5.0));
        assert!(summary.push(31 * MIN, 5.0));
        assert_eq!(
            summary.function(Function::Change, 31 * MIN, HOUR),
            Some(4.0)
        );
    }

    #[test]
    fn a_steady_disk_forecasts_the_day_it_is_full() {
        // 1% more every hour from 50%: full in 50 hours from the start.
        let summary = fed(DAY, (0..=24).map(|h| (h * HOUR, 0.5 + 0.01 * h as f64)));
        let at = 24 * HOUR;
        let left = summary.forecast(at, DAY, 1.0, Comparison::Ge).unwrap();
        assert!((left - 26.0 * 3600.0).abs() < 60.0, "{left}");
        // Memory falling towards 5% the same way.
        let summary = fed(DAY, (0..=24).map(|h| (h * HOUR, 0.5 - 0.01 * h as f64)));
        let left = summary.forecast(at, DAY, 0.05, Comparison::Le).unwrap();
        assert!((left - 21.0 * 3600.0).abs() < 60.0, "{left}");
    }

    #[test]
    fn a_flat_or_receding_series_never_reaches_its_target() {
        let flat = fed(DAY, (0..=24).map(|h| (h * HOUR, 0.5)));
        assert_eq!(
            flat.forecast(24 * HOUR, DAY, 1.0, Comparison::Ge),
            Some(FOREVER_S)
        );
        let freeing = fed(DAY, (0..=24).map(|h| (h * HOUR, 0.9 - 0.01 * h as f64)));
        assert_eq!(
            freeing.forecast(24 * HOUR, DAY, 1.0, Comparison::Ge),
            Some(FOREVER_S)
        );
        // Already there: nothing left.
        let full = fed(DAY, (0..=24).map(|h| (h * HOUR, 0.9 + 0.01 * h as f64)));
        assert_eq!(
            full.forecast(24 * HOUR, DAY, 1.0, Comparison::Ge),
            Some(0.0)
        );
        // Very slow: capped.
        let slow = fed(DAY, (0..=24).map(|h| (h * HOUR, 0.5 + 1e-12 * h as f64)));
        assert_eq!(
            slow.forecast(24 * HOUR, DAY, 1.0, Comparison::Ge),
            Some(FOREVER_S)
        );
    }

    #[test]
    fn a_forecast_needs_a_quarter_of_its_window() {
        let young = fed(DAY, (0..=5).map(|h| (h * HOUR, 0.5 + 0.1 * h as f64)));
        assert_eq!(young.forecast(5 * HOUR, DAY, 1.1, Comparison::Ge), None);
        let enough = fed(DAY, (0..=6).map(|h| (h * HOUR, 0.4 + 0.01 * h as f64)));
        assert!(enough
            .forecast(6 * HOUR, DAY, 1.0, Comparison::Ge)
            .is_some());
        // A whole day loaded at a restart answers at once.
        let mut loaded = Summary::new(DAY);
        let history: Vec<_> = (0..=24)
            .map(|h| (h * HOUR, 0.5 + 0.01 * h as f64))
            .collect();
        loaded.load(0, &history);
        assert!(loaded.push(24 * HOUR + MIN, 0.74));
        let left = loaded
            .forecast(24 * HOUR + MIN, DAY, 1.0, Comparison::Ge)
            .unwrap();
        assert!((left - 26.0 * 3600.0).abs() < 1800.0, "{left}");
    }

    #[test]
    fn derive_gives_each_kind_its_number() {
        let summary = fed(HOUR, (0..60).map(|i| (i * MIN, 10.0 + i as f64)));
        let at = 59 * MIN;
        let rule = |kind, function, window_s| Rule {
            kind,
            function,
            window_s,
            ..Rule::default()
        };
        let value = rule(Kind::Value, None, 0);
        assert_eq!(derive(&value, None, at, 3.0, None), Some(3.0));
        let max = rule(Kind::Window, Some(Function::Max), 600);
        assert_eq!(derive(&max, Some(&summary), at, 69.0, None), Some(69.0));
        assert_eq!(derive(&max, None, at, 69.0, None), None);
        let ratio = Rule {
            offset_s: 604_800,
            ..rule(Kind::Compare, Some(Function::Ratio), 600)
        };
        // Average of the last 10 min is 64.5.
        assert_eq!(
            derive(&ratio, Some(&summary), at, 0.0, Some(21.5)),
            Some(3.0)
        );
        assert_eq!(derive(&ratio, Some(&summary), at, 0.0, Some(0.0)), None);
        assert_eq!(derive(&ratio, Some(&summary), at, 0.0, None), None);
        let difference = Rule {
            function: Some(Function::Difference),
            ..ratio.clone()
        };
        assert_eq!(
            derive(&difference, Some(&summary), at, 0.0, Some(60.0)),
            Some(4.5)
        );
        let forecast = Rule {
            target: Some(129.0),
            comparison: Comparison::Ge,
            ..rule(Kind::Forecast, None, 3600)
        };
        let left = derive(&forecast, Some(&summary), at, 69.0, None).unwrap();
        assert!((left - 3600.0).abs() < 5.0, "{left}");
        assert_eq!(
            derive(&rule(Kind::NoData, None, 0), None, at, 1.0, None),
            None
        );
        assert_eq!(summary_span_ms(&forecast), Some(HOUR));
        assert_eq!(summary_span_ms(&value), None);
    }

    #[test]
    fn needs_follow_the_rules_in_force() {
        use super::super::rules::{resolve, Condition, Row};
        use crate::sensor_config::Layer;
        let row = |rule: Rule| Row {
            layer: Layer::Server,
            rule,
            enabled: true,
        };
        let load = "monitorflow_cpu_load1";
        let rules = resolve(vec![
            row(Rule {
                metric: load.into(),
                kind: Kind::Window,
                function: Some(Function::Max),
                window_s: 600,
                warning: Some(4.0),
                ..Rule::default()
            }),
            row(Rule {
                metric: load.into(),
                instance: Some(String::new()),
                kind: Kind::Compare,
                function: Some(Function::Ratio),
                window_s: 3600,
                offset_s: 86_400,
                warning: Some(2.0),
                ..Rule::default()
            }),
            row(Rule {
                metric: "monitorflow_port_443_up".into(),
                kind: Kind::NoData,
                warning: Some(300.0),
                ..Rule::default()
            }),
            row(Rule {
                metric: "monitorflow_cpu_usage_ratio".into(),
                instance: Some("total".into()),
                warning: Some(0.9),
                conditions: vec![Condition {
                    metric: "monitorflow_memory_available_ratio".into(),
                    instance: None,
                    function: Some(Function::Min),
                    window_s: Some(300),
                    comparison: Comparison::Lt,
                    value: 0.1,
                }],
                ..Rule::default()
            }),
        ]);
        let needs = needs(&rules);
        // The longest window, and the average since not every reader
        // wants the maximum.
        assert_eq!(
            needs[load],
            Need {
                span_ms: 3_600_000,
                // The 10-minute maximum and the hour's comparison apart.
                spans: vec![600_000, 3_600_000],
                seed: Function::Avg,
                gate: false,
                arrival: false
            }
        );
        assert!(needs["monitorflow_port_443_up"].arrival);
        assert_eq!(needs["monitorflow_port_443_up"].span_ms, 0);
        let memory = &needs["monitorflow_memory_available_ratio"];
        assert!(memory.gate);
        assert_eq!((memory.span_ms, memory.seed), (300_000, Function::Min));
        // Plain thresholds, the built-in ones included, need nothing.
        assert!(!needs.contains_key("monitorflow_cpu_usage_ratio"));
        assert!(!needs.contains_key("monitorflow_disk_used_ratio"));
    }
}
