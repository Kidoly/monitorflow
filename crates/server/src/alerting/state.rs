//! The state machine of one series (ADR-0017): ok, pending, firing.
//!
//! Each severity has its own pending timer: the sample time since which
//! its condition holds without a break. A severity fires once its timer is
//! `duration` old, and keeps firing until the value recovers past its
//! threshold by the hysteresis. The incident takes the highest severity
//! that fires: opening, escalating, de-escalating and resolving all follow
//! from that. Time is the samples' own timestamps, so the machine is pure
//! and replayed backlogs are judged on when values were measured.

use super::rules::Rule;
use super::Severity;

/// A change of the incident of a series.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    Opened(Severity),
    Escalated { from: Severity, to: Severity },
    Deescalated { from: Severity, to: Severity },
    Resolved { from: Severity },
}

impl Transition {
    /// Severity of the incident after the change; `None` once resolved.
    pub fn severity(self) -> Option<Severity> {
        match self {
            Self::Opened(to) | Self::Escalated { to, .. } | Self::Deescalated { to, .. } => {
                Some(to)
            }
            Self::Resolved { .. } => None,
        }
    }

    /// The `incident_events.kind` of the change.
    pub fn kind(self) -> &'static str {
        match self {
            Self::Opened(_) => "opened",
            Self::Escalated { .. } => "escalated",
            Self::Deescalated { .. } => "deescalated",
            Self::Resolved { .. } => "resolved",
        }
    }
}

/// What one sample changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Evaluation {
    pub transition: Option<Transition>,
    /// Whether a pending timer started or stopped, so the persisted state
    /// is behind.
    pub timers_changed: bool,
}

/// Evaluation state of one series. Times are sample timestamps in Unix
/// milliseconds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeriesState {
    /// Severity of the open incident.
    pub firing: Option<Severity>,
    pub warning_since: Option<i64>,
    pub critical_since: Option<i64>,
    /// Timestamp of the last sample evaluated.
    pub last_at: Option<i64>,
}

impl SeriesState {
    /// Neither firing nor pending: nothing to persist.
    pub fn is_ok(&self) -> bool {
        self.firing.is_none() && self.warning_since.is_none() && self.critical_since.is_none()
    }

    fn since_mut(&mut self, severity: Severity) -> &mut Option<i64> {
        match severity {
            Severity::Warning => &mut self.warning_since,
            Severity::Critical => &mut self.critical_since,
        }
    }

    /// Evaluates the sample `value` taken at `at`. Returns `None` for a
    /// sample not newer than the last one, which is ignored: a resent or
    /// reordered sample must not move timers back.
    ///
    /// A gap of more than `max_gap_ms` since the previous sample restarts
    /// the pending timers at this sample, since nothing says the condition
    /// held meanwhile. An open incident stays open over a gap.
    pub fn evaluate(
        &mut self,
        rule: &Rule,
        at: i64,
        value: f64,
        max_gap_ms: i64,
    ) -> Option<Evaluation> {
        let timers = (self.warning_since, self.critical_since);
        if let Some(last) = self.last_at {
            if at <= last {
                return None;
            }
            if at - last > max_gap_ms {
                self.warning_since = None;
                self.critical_since = None;
            }
        }
        self.last_at = Some(at);
        let mut target = None;
        for severity in [Severity::Warning, Severity::Critical] {
            let firing = self.firing;
            let since = self.since_mut(severity);
            let Some(threshold) = rule.threshold(severity) else {
                *since = None;
                continue;
            };
            if rule.comparison.holds(value, threshold) {
                since.get_or_insert(at);
            } else {
                *since = None;
            }
            let matured = since.is_some_and(|s| at - s >= rule.duration_ms());
            let held = firing >= Some(severity)
                && !rule.comparison.recovered(value, threshold, rule.hysteresis);
            if matured || held {
                target = Some(severity);
            }
        }
        let transition = self.move_to(target);
        Some(Evaluation {
            transition,
            timers_changed: timers != (self.warning_since, self.critical_since),
        })
    }

    /// Resolves an open incident whose threshold was removed or muted, and
    /// drops the timers.
    pub fn clear(&mut self) -> Option<Transition> {
        self.warning_since = None;
        self.critical_since = None;
        self.move_to(None)
    }

    fn move_to(&mut self, target: Option<Severity>) -> Option<Transition> {
        let transition = match (self.firing, target) {
            (None, Some(to)) => Transition::Opened(to),
            (Some(from), None) => Transition::Resolved { from },
            (Some(from), Some(to)) if to > from => Transition::Escalated { from, to },
            (Some(from), Some(to)) if to < from => Transition::Deescalated { from, to },
            _ => return None,
        };
        self.firing = target;
        Some(transition)
    }
}

#[cfg(test)]
mod tests {
    use super::super::rules::Comparison;
    use super::*;
    use Severity::{Critical, Warning};

    const S: i64 = 1000;
    const GAP: i64 = 600 * S;

    /// disk used > 0.85 warning, > 0.95 critical, for 300 s.
    fn disk(hysteresis: f64) -> Rule {
        Rule {
            metric: "monitorflow_disk_used_ratio".into(),
            instance: None,
            comparison: Comparison::Gt,
            warning: Some(0.85),
            critical: Some(0.95),
            duration_s: 300,
            hysteresis,
        }
    }

    /// Feeds `(seconds, value)` samples and returns the transitions.
    fn run(
        state: &mut SeriesState,
        rule: &Rule,
        samples: &[(i64, f64)],
    ) -> Vec<Option<Transition>> {
        samples
            .iter()
            .map(|&(t, v)| {
                state
                    .evaluate(rule, t * S, v, GAP)
                    .and_then(|e| e.transition)
            })
            .collect()
    }

    #[test]
    fn an_incident_opens_once_the_condition_held_for_the_duration() {
        let rule = disk(0.0);
        let mut state = SeriesState::default();
        let first = state.evaluate(&rule, 0, 0.9, GAP).unwrap();
        assert_eq!(
            first,
            Evaluation {
                transition: None,
                timers_changed: true
            }
        );
        assert_eq!(state.warning_since, Some(0));
        let next = state.evaluate(&rule, 60 * S, 0.9, GAP).unwrap();
        assert!(!next.timers_changed);
        assert_eq!(
            run(&mut state, &rule, &[(299, 0.9), (300, 0.9), (360, 0.9)]),
            [None, Some(Transition::Opened(Warning)), None]
        );
        assert_eq!(state.firing, Some(Warning));
        assert!(!state.is_ok());
    }

    #[test]
    fn a_zero_duration_fires_on_the_first_sample() {
        let rule = Rule {
            duration_s: 0,
            ..disk(0.0)
        };
        let mut state = SeriesState::default();
        assert_eq!(
            run(&mut state, &rule, &[(0, 0.97)]),
            [Some(Transition::Opened(Critical))]
        );
    }

    #[test]
    fn a_break_in_the_condition_restarts_the_timer() {
        let rule = disk(0.0);
        let mut state = SeriesState::default();
        let transitions = run(
            &mut state,
            &rule,
            &[(0, 0.9), (200, 0.8), (240, 0.9), (500, 0.9), (540, 0.9)],
        );
        assert_eq!(
            transitions,
            [None, None, None, None, Some(Transition::Opened(Warning))]
        );
    }

    #[test]
    fn escalation_needs_its_own_duration_and_de_escalation_is_immediate() {
        let rule = disk(0.0);
        let mut state = SeriesState::default();
        let transitions = run(
            &mut state,
            &rule,
            &[
                (0, 0.9),
                (300, 0.9),
                (360, 0.97),
                (600, 0.97),
                (660, 0.97),
                (720, 0.9),
                (780, 0.5),
            ],
        );
        assert_eq!(
            transitions,
            [
                None,
                Some(Transition::Opened(Warning)),
                None,
                None,
                Some(Transition::Escalated {
                    from: Warning,
                    to: Critical
                }),
                Some(Transition::Deescalated {
                    from: Critical,
                    to: Warning
                }),
                Some(Transition::Resolved { from: Warning }),
            ]
        );
        assert!(state.is_ok());
    }

    #[test]
    fn a_value_past_both_thresholds_opens_critical_directly() {
        let rule = disk(0.0);
        let mut state = SeriesState::default();
        assert_eq!(
            run(&mut state, &rule, &[(0, 0.99), (300, 0.99)]),
            [None, Some(Transition::Opened(Critical))]
        );
        // From critical straight to resolved.
        assert_eq!(
            run(&mut state, &rule, &[(360, 0.1)]),
            [Some(Transition::Resolved { from: Critical })]
        );
    }

    #[test]
    fn hysteresis_keeps_the_incident_open_until_the_value_is_clearly_back() {
        let rule = disk(0.05);
        let mut state = SeriesState::default();
        let transitions = run(
            &mut state,
            &rule,
            &[
                (0, 0.9),
                (300, 0.9),
                (360, 0.84),
                (420, 0.81),
                (480, 0.79),
                (540, 0.78),
            ],
        );
        // Recovery needs about 0.80 or less: 0.84 and 0.81 keep it open.
        assert_eq!(
            transitions,
            [
                None,
                Some(Transition::Opened(Warning)),
                None,
                None,
                Some(Transition::Resolved { from: Warning }),
                None
            ]
        );
        // Critical de-escalates once below 0.90, not below 0.95.
        let mut state = SeriesState::default();
        let transitions = run(
            &mut state,
            &rule,
            &[(0, 0.97), (300, 0.97), (360, 0.92), (420, 0.89)],
        );
        assert_eq!(
            transitions,
            [
                None,
                Some(Transition::Opened(Critical)),
                None,
                Some(Transition::Deescalated {
                    from: Critical,
                    to: Warning
                })
            ]
        );
    }

    #[test]
    fn flapping_values_never_open_an_incident() {
        let rule = disk(0.0);
        let mut state = SeriesState::default();
        let samples: Vec<_> = (0..100)
            .map(|i| (i * 60, if i % 2 == 0 { 0.9 } else { 0.8 }))
            .collect();
        assert!(run(&mut state, &rule, &samples).iter().all(Option::is_none));
        // Once firing, flapping inside the hysteresis band does not resolve.
        let rule = disk(0.1);
        let mut state = SeriesState::default();
        run(&mut state, &rule, &[(0, 0.9), (300, 0.9)]);
        let samples: Vec<_> = (6..100)
            .map(|i| (i * 60, if i % 2 == 0 { 0.9 } else { 0.8 }))
            .collect();
        assert!(run(&mut state, &rule, &samples).iter().all(Option::is_none));
        assert_eq!(state.firing, Some(Warning));
    }

    #[test]
    fn old_and_repeated_samples_are_ignored() {
        let rule = disk(0.0);
        let mut state = SeriesState::default();
        run(&mut state, &rule, &[(0, 0.9), (300, 0.9)]);
        assert_eq!(state.evaluate(&rule, 300 * S, 0.1, GAP), None);
        assert_eq!(state.evaluate(&rule, 100 * S, 0.1, GAP), None);
        assert_eq!(state.firing, Some(Warning));
        assert_eq!(state.last_at, Some(300 * S));
        // An old sample cannot start a timer before the current one either.
        let mut state = SeriesState::default();
        run(&mut state, &rule, &[(100, 0.9)]);
        assert_eq!(state.evaluate(&rule, 0, 0.9, GAP), None);
        assert_eq!(state.warning_since, Some(100 * S));
    }

    #[test]
    fn a_gap_restarts_pending_timers_but_keeps_incidents() {
        let rule = disk(0.0);
        let mut state = SeriesState::default();
        // 0 -> 700 s is longer than the gap: the timer restarts at 700.
        assert_eq!(
            run(
                &mut state,
                &rule,
                &[(0, 0.9), (700, 0.9), (999, 0.9), (1000, 0.9)]
            ),
            [None, None, None, Some(Transition::Opened(Warning))]
        );
        assert_eq!(run(&mut state, &rule, &[(5000, 0.9)]), [None]);
        assert_eq!(state.firing, Some(Warning));
        assert_eq!(state.warning_since, Some(5000 * S));
    }

    #[test]
    fn a_rule_without_warning_or_critical_drops_that_level() {
        let cpu = Rule {
            critical: None,
            ..disk(0.0)
        };
        let mut state = SeriesState::default();
        assert_eq!(
            run(&mut state, &cpu, &[(0, 0.99), (300, 0.99)]),
            [None, Some(Transition::Opened(Warning))]
        );
        assert_eq!(state.critical_since, None);
        // A critical-only rule.
        let rule = Rule {
            warning: None,
            ..disk(0.0)
        };
        let mut state = SeriesState::default();
        assert_eq!(
            run(&mut state, &rule, &[(0, 0.9), (300, 0.9)]),
            [None, None]
        );
        assert_eq!(state.warning_since, None);
    }

    #[test]
    fn below_thresholds_work_the_same_way() {
        let memory = Rule {
            comparison: Comparison::Lt,
            warning: Some(0.10),
            critical: Some(0.05),
            ..disk(0.02)
        };
        let mut state = SeriesState::default();
        let transitions = run(
            &mut state,
            &memory,
            &[
                (0, 0.08),
                (300, 0.04),
                (600, 0.04),
                (660, 0.11),
                (720, 0.13),
            ],
        );
        assert_eq!(
            transitions,
            [
                None,
                Some(Transition::Opened(Warning)),
                Some(Transition::Escalated {
                    from: Warning,
                    to: Critical
                }),
                Some(Transition::Deescalated {
                    from: Critical,
                    to: Warning
                }),
                Some(Transition::Resolved { from: Warning }),
            ]
        );
    }

    #[test]
    fn clearing_resolves_and_drops_timers() {
        let rule = disk(0.0);
        let mut state = SeriesState::default();
        run(&mut state, &rule, &[(0, 0.99), (300, 0.99)]);
        assert_eq!(state.clear(), Some(Transition::Resolved { from: Critical }));
        assert!(state.is_ok());
        assert_eq!(state.clear(), None);
        assert_eq!(Transition::Resolved { from: Critical }.severity(), None);
        assert_eq!(Transition::Opened(Warning).kind(), "opened");
    }
}
