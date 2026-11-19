//! What a rule would have done (ADR-0153): the history of a series replayed
//! through the same summaries, derivation and state machine as the engine.
//!
//! Pure: the API reads the history from VictoriaMetrics and hands it here.

use std::collections::HashMap;

use super::history::Points;
use super::rules::{Condition, Kind, Rule};
use super::state::{SeriesState, Transition};
use super::summary::{derive, Summary};
use super::Severity;

/// Points of the judged value kept for a chart.
pub const MAX_POINTS: usize = 300;

/// An incident the rule would have opened.
#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
pub struct PreviewIncident {
    /// The highest severity it reached: `warning` or `critical`.
    pub severity: String,
    /// Unix milliseconds.
    pub opened_at: i64,
    /// Unix milliseconds; `null` when it would still be open.
    pub resolved_at: Option<i64>,
    /// The worst judged value while open.
    pub peak_value: f64,
}

/// One series replayed.
#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
pub struct PreviewSeries {
    /// `""` for a series without an instance.
    pub instance: String,
    /// `[Unix ms, judged value]`, at most 300, the worst of each stretch:
    /// the sample, the window's value, the ratio, the seconds left or of
    /// silence.
    #[schema(value_type = Vec<Vec<f64>>)]
    pub points: Vec<(i64, f64)>,
    pub incidents: Vec<PreviewIncident>,
}

/// The history a replay needs.
#[derive(Debug, Default)]
pub struct Inputs {
    /// The rule's series, by instance, oldest first. Points before `from`
    /// only fill windows.
    pub series: Vec<(String, Points)>,
    /// Per condition, its series by instance.
    pub conditions: Vec<HashMap<String, Points>>,
    /// A comparison's past, by instance, already moved forward by the
    /// offset: the value at `t` is the one `offset_s` before `t`.
    pub past: HashMap<String, Points>,
    /// Step of `past`, in milliseconds.
    pub past_step_ms: i64,
    pub from_ms: i64,
    pub to_ms: i64,
    pub max_gap_ms: i64,
}

/// The point at or just before `at`, no older than `within_ms`.
fn at_or_before(points: &[(i64, f64)], at: i64, within_ms: i64) -> Option<f64> {
    let i = points.partition_point(|p| p.0 <= at);
    let (t, v) = *points.get(i.checked_sub(1)?)?;
    (at - t <= within_ms).then_some(v)
}

/// A condition's value at each of its samples: the sample, or its window
/// function.
fn condition_values(condition: &Condition, points: &[(i64, f64)]) -> Points {
    let Some(function) = condition.function else {
        return points.to_vec();
    };
    let window = i64::from(condition.window_s.unwrap_or(0)) * 1000;
    let mut summary = Summary::new(window);
    points
        .iter()
        .filter_map(|&(t, v)| {
            summary.push(t, v);
            Some((t, summary.function(function, t, window)?))
        })
        .collect()
}

/// Follows transitions into incidents.
#[derive(Default)]
struct Incidents {
    done: Vec<PreviewIncident>,
    open: Option<PreviewIncident>,
}

impl Incidents {
    fn step(&mut self, rule: &Rule, at: i64, value: Option<f64>, transition: Option<Transition>) {
        if let (Some(open), Some(v)) = (&mut self.open, value) {
            open.peak_value = rule.judged().worst(open.peak_value, v);
        }
        match transition {
            Some(Transition::Opened(severity)) => {
                self.open = Some(PreviewIncident {
                    severity: severity.as_str().into(),
                    opened_at: at,
                    resolved_at: None,
                    peak_value: value.unwrap_or_default(),
                });
            }
            Some(Transition::Escalated { to, .. }) => {
                if let Some(open) = &mut self.open {
                    open.severity = to.as_str().into();
                }
            }
            Some(Transition::Resolved { .. }) => {
                if let Some(mut open) = self.open.take() {
                    open.resolved_at = Some(at);
                    self.done.push(open);
                }
            }
            _ => {}
        }
    }

    /// Those still open or resolved after `from`.
    fn finish(mut self, from: i64) -> Vec<PreviewIncident> {
        self.done.extend(self.open.take());
        self.done
            .into_iter()
            .filter(|i| i.resolved_at.is_none_or(|r| r >= from))
            .collect()
    }
}

/// Keeps at most [`MAX_POINTS`], the worst of each stretch.
fn thin(rule: &Rule, points: Vec<(i64, f64)>, from: i64, to: i64) -> Vec<(i64, f64)> {
    if points.len() <= MAX_POINTS {
        return points;
    }
    let width = ((to - from) / MAX_POINTS as i64).max(1);
    let mut out: Vec<(i64, f64)> = Vec::with_capacity(MAX_POINTS + 1);
    let mut bucket = None;
    for (t, v) in points {
        let b = (t - from) / width;
        match out.last_mut() {
            Some(last) if bucket == Some(b) => {
                if rule.judged().worst(last.1, v) == v {
                    *last = (t, v);
                }
            }
            _ => {
                out.push((t, v));
                bucket = Some(b);
            }
        }
    }
    out
}

/// Replays `inputs` through `rule`.
pub fn replay(rule: &Rule, inputs: &Inputs) -> Vec<PreviewSeries> {
    inputs
        .series
        .iter()
        .map(|(instance, points)| {
            let (judged, incidents) = if rule.kind == Kind::NoData {
                silence(rule, points, inputs)
            } else {
                series(rule, instance, points, inputs)
            };
            PreviewSeries {
                instance: instance.clone(),
                points: thin(rule, judged, inputs.from_ms, inputs.to_ms),
                incidents,
            }
        })
        .collect()
}

fn series(
    rule: &Rule,
    instance: &str,
    points: &[(i64, f64)],
    inputs: &Inputs,
) -> (Vec<(i64, f64)>, Vec<PreviewIncident>) {
    let mut summary = rule
        .kind
        .needs_summary()
        .then(|| Summary::new(rule.window_ms()));
    let conditions: Vec<Option<Points>> = rule
        .conditions
        .iter()
        .zip(&inputs.conditions)
        .map(|(condition, series)| {
            let points = match &condition.instance {
                Some(i) => series.get(i),
                None => series.get(instance).or_else(|| series.get("")),
            }?;
            Some(condition_values(condition, points))
        })
        .collect();
    let past = inputs.past.get(instance);
    let mut state = SeriesState::default();
    let mut incidents = Incidents::default();
    let mut judged = Vec::new();
    for &(t, v) in points {
        if let Some(summary) = &mut summary {
            summary.push(t, v);
        }
        let then = past.and_then(|p| at_or_before(p, t, inputs.past_step_ms));
        let value = derive(rule, summary.as_ref(), t, v, then);
        let gates = rule.conditions.iter().zip(&conditions).all(|(c, values)| {
            values
                .as_deref()
                .and_then(|values| at_or_before(values, t, inputs.max_gap_ms))
                .is_some_and(|value| c.comparison.holds(value, c.value))
        });
        let Some(evaluation) = state.evaluate_gated(rule, t, value, gates, inputs.max_gap_ms)
        else {
            continue;
        };
        incidents.step(rule, t, value, evaluation.transition);
        if let (Some(v), true) = (value, t >= inputs.from_ms) {
            judged.push((t, v));
        }
    }
    (judged, incidents.finish(inputs.from_ms))
}

/// No data, from the gaps between samples' timestamps.
fn silence(
    rule: &Rule,
    points: &[(i64, f64)],
    inputs: &Inputs,
) -> (Vec<(i64, f64)>, Vec<PreviewIncident>) {
    let mut limits: Vec<f64> = [Severity::Warning, Severity::Critical]
        .into_iter()
        .filter_map(|s| rule.threshold(s))
        .collect();
    limits.sort_by(f64::total_cmp);
    let mut state = SeriesState::default();
    let mut incidents = Incidents::default();
    let mut judged = Vec::new();
    let mut judge = |at: i64, silence_s: f64, judged: &mut Vec<(i64, f64)>| {
        let evaluation = state.evaluate_silence(rule, at, silence_s);
        incidents.step(rule, at, Some(silence_s), evaluation.transition);
        if at >= inputs.from_ms {
            judged.push((at, silence_s));
        }
    };
    let ends = points
        .iter()
        .map(|p| p.0)
        .chain(std::iter::once(inputs.to_ms));
    let mut last: Option<i64> = None;
    for end in ends {
        if let Some(start) = last {
            for &limit in &limits {
                // Just past the limit, as the engine's sweep would see it.
                let at = start + (limit * 1000.0) as i64 + 1_000;
                if at < end {
                    judge(at, (at - start) as f64 / 1000.0, &mut judged);
                }
            }
        }
        if end < inputs.to_ms || last.is_none() {
            judge(end, 0.0, &mut judged);
        } else {
            judge(
                end,
                (end - last.unwrap_or(end)) as f64 / 1000.0,
                &mut judged,
            );
        }
        last = Some(end);
    }
    (judged, incidents.finish(inputs.from_ms))
}

#[cfg(test)]
mod tests {
    use super::super::rules::{Comparison, Function};
    use super::*;

    const MIN: i64 = 60_000;

    fn inputs(series: Vec<(String, Points)>, from: i64, to: i64) -> Inputs {
        Inputs {
            series,
            from_ms: from,
            to_ms: to,
            max_gap_ms: 10 * MIN,
            past_step_ms: 5 * MIN,
            ..Inputs::default()
        }
    }

    #[test]
    fn a_window_rule_replays_with_its_warm_up() {
        // Load 1, a one-minute spike at 60 min, then 6 from 90 min.
        let points: Vec<_> = (0..120)
            .map(|i| {
                let v = match i {
                    60 => 30.0,
                    i if i >= 90 => 6.0,
                    _ => 1.0,
                };
                (i * MIN, v)
            })
            .collect();
        let rule = Rule {
            metric: "monitorflow_cpu_load1".into(),
            kind: Kind::Window,
            function: Some(Function::Avg),
            window_s: 900,
            warning: Some(4.0),
            ..Rule::default()
        };
        let replayed = replay(
            &rule,
            &inputs(vec![(String::new(), points)], 30 * MIN, 90 * MIN),
        );
        let incidents = &replayed[0].incidents;
        // The spike lifts the 15-minute average to under 3: nothing; the
        // sustained 6 opens one, still open at the end.
        assert_eq!(incidents.len(), 1, "{incidents:?}");
        assert!(incidents[0].opened_at > 90 * MIN && incidents[0].resolved_at.is_none());
        assert!(replayed[0].points.iter().all(|p| p.0 >= 30 * MIN));
    }

    #[test]
    fn a_forecast_and_its_conditions_replay() {
        let points: Vec<_> = (0..=48)
            .map(|h| (h * 60 * MIN, 0.5 + 0.01 * h as f64))
            .collect();
        let rule = Rule {
            metric: "monitorflow_disk_used_ratio".into(),
            kind: Kind::Forecast,
            comparison: Comparison::Ge,
            target: Some(1.0),
            window_s: 86_400,
            warning: Some(3.0 * 86_400.0),
            conditions: vec![Condition {
                metric: "monitorflow_disk_used_ratio".into(),
                instance: None,
                function: None,
                window_s: None,
                comparison: Comparison::Gt,
                value: 0.8,
            }],
            ..Rule::default()
        };
        let mut input = inputs(
            vec![("/".into(), points.clone())],
            24 * 60 * MIN,
            48 * 60 * MIN,
        );
        input.max_gap_ms = 2 * 60 * MIN;
        input.conditions = vec![[("/".to_owned(), points)].into()];
        let replayed = replay(&rule, &input);
        let incidents = &replayed[0].incidents;
        // Full in under 3 days from about 26 hours in, but gated until the
        // disk is over 80%, at hour 31.
        assert_eq!(incidents.len(), 1, "{incidents:?}");
        assert_eq!(incidents[0].opened_at, 31 * 60 * MIN);
        assert!(incidents[0].peak_value < 2.0 * 86_400.0);
    }

    #[test]
    fn a_comparison_reads_its_moved_past() {
        let points: Vec<_> = (0..30)
            .map(|i| (i * MIN, if i < 20 { 100.0 } else { 300.0 }))
            .collect();
        let rule = Rule {
            metric: "monitorflow_net_received_bytes_per_second".into(),
            kind: Kind::Compare,
            function: Some(Function::Ratio),
            window_s: 300,
            offset_s: 604_800,
            warning: Some(2.0),
            ..Rule::default()
        };
        let mut input = inputs(vec![("eth0".into(), points)], 0, 30 * MIN);
        input.past = [(
            "eth0".to_owned(),
            (0..7).map(|i| (i * 5 * MIN, 100.0)).collect(),
        )]
        .into();
        let replayed = replay(&rule, &input);
        assert_eq!(replayed[0].incidents.len(), 1, "{:?}", replayed[0]);
        // No past: never judged.
        input.past.clear();
        let replayed = replay(&rule, &input);
        assert!(replayed[0].incidents.is_empty() && replayed[0].points.is_empty());
    }

    #[test]
    fn silences_open_and_close_from_the_gaps() {
        // A sample a minute, then 20 minutes without, then again, then
        // nothing until the end.
        let mut points: Vec<_> = (0..10).map(|i| (i * MIN, 1.0)).collect();
        points.extend((30..40).map(|i| (i * MIN, 1.0)));
        let rule = Rule {
            kind: Kind::NoData,
            warning: Some(300.0),
            critical: Some(900.0),
            ..Rule::default()
        };
        let replayed = replay(&rule, &inputs(vec![(String::new(), points)], 0, 60 * MIN));
        let incidents = &replayed[0].incidents;
        assert_eq!(incidents.len(), 2, "{incidents:?}");
        assert_eq!(
            (incidents[0].severity.as_str(), incidents[0].resolved_at),
            ("critical", Some(30 * MIN))
        );
        assert_eq!(incidents[0].opened_at, 9 * MIN + 301_000);
        // The last silence lasts 21 minutes at the end: still open.
        assert_eq!(incidents[1].resolved_at, None);
        assert_eq!(incidents[1].severity, "critical");
    }

    #[test]
    fn long_series_are_thinned_keeping_the_worst() {
        let rule = Rule::default();
        let points: Vec<_> = (0..3_000)
            .map(|i| (i * 1000, if i == 1234 { 9.0 } else { 1.0 }))
            .collect();
        let thinned = thin(&rule, points, 0, 3_000_000);
        assert!(thinned.len() <= MAX_POINTS + 1);
        assert!(thinned.iter().any(|p| p.1 == 9.0));
    }
}
