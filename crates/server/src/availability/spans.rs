//! Sets of time spans: what availability is computed with (ADR-0155).

use chrono::{DateTime, Utc};

/// A stretch of time from `0`, included, to `1`, excluded.
pub type Span = (DateTime<Utc>, DateTime<Utc>);

/// Spans sorted by start, none empty, none touching or overlapping.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Spans(Vec<Span>);

impl Spans {
    /// No time at all.
    pub fn new() -> Self {
        Self::default()
    }

    /// The union of `spans`, in any order; empty ones are dropped.
    pub fn from_spans(spans: impl IntoIterator<Item = Span>) -> Self {
        let mut all: Vec<Span> = spans.into_iter().filter(|(a, b)| a < b).collect();
        all.sort_unstable();
        let mut merged: Vec<Span> = Vec::with_capacity(all.len());
        for (start, end) in all {
            match merged.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        Self(merged)
    }

    /// One span, or nothing when it is empty.
    pub fn one(start: DateTime<Utc>, end: DateTime<Utc>) -> Self {
        Self::from_spans([(start, end)])
    }

    pub fn spans(&self) -> &[Span] {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Seconds in all, rounded down to the millisecond.
    pub fn seconds(&self) -> f64 {
        // A sum of floats starts at -0.0, which a CSV would write "-0".
        self.0
            .iter()
            .map(|(a, b)| (*b - *a).num_milliseconds() as f64 / 1000.0)
            .fold(0.0, |all, s| all + s)
    }

    pub fn union(&self, other: &Spans) -> Spans {
        Self::from_spans(self.0.iter().chain(other.0.iter()).copied())
    }

    /// The time in both.
    pub fn intersect(&self, other: &Spans) -> Spans {
        let (mut i, mut j) = (0, 0);
        let mut out = Vec::new();
        while i < self.0.len() && j < other.0.len() {
            let (a, b) = (self.0[i], other.0[j]);
            let start = a.0.max(b.0);
            let end = a.1.min(b.1);
            if start < end {
                out.push((start, end));
            }
            if a.1 < b.1 {
                i += 1;
            } else {
                j += 1;
            }
        }
        Self(out)
    }

    /// The time in `self` and not in `other`.
    pub fn minus(&self, other: &Spans) -> Spans {
        let mut out = Vec::new();
        let mut j = 0;
        for &(start, end) in &self.0 {
            let mut from = start;
            while j < other.0.len() && other.0[j].1 <= from {
                j += 1;
            }
            let mut k = j;
            while k < other.0.len() && other.0[k].0 < end {
                let (cut_start, cut_end) = other.0[k];
                if cut_start > from {
                    out.push((from, cut_start));
                }
                from = from.max(cut_end);
                if from >= end {
                    break;
                }
                k += 1;
            }
            if from < end {
                out.push((from, end));
            }
        }
        Self(out)
    }

    /// Only the time within `span`.
    pub fn clip(&self, span: Span) -> Spans {
        self.intersect(&Spans::one(span.0, span.1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minute `m` of a fixed day.
    fn t(m: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_790_000_000 + m * 60, 0).unwrap()
    }

    fn s(list: &[(i64, i64)]) -> Spans {
        Spans::from_spans(list.iter().map(|&(a, b)| (t(a), t(b))))
    }

    #[test]
    fn spans_are_merged_and_sorted() {
        let spans = s(&[(10, 20), (0, 5), (5, 7), (15, 30), (40, 40)]);
        assert_eq!(spans, Spans(vec![(t(0), t(7)), (t(10), t(30))]));
        assert_eq!(spans.seconds(), 27.0 * 60.0);
        assert!(Spans::new().seconds().is_sign_positive());
    }

    #[test]
    fn union_intersection_and_difference() {
        let a = s(&[(0, 10), (20, 30)]);
        let b = s(&[(5, 25)]);
        assert_eq!(a.union(&b), s(&[(0, 30)]));
        assert_eq!(a.intersect(&b), s(&[(5, 10), (20, 25)]));
        assert_eq!(a.minus(&b), s(&[(0, 5), (25, 30)]));
        assert_eq!(b.minus(&a), s(&[(10, 20)]));
        assert_eq!(
            a.minus(&s(&[(2, 3), (4, 6), (22, 40)])),
            s(&[(0, 2), (3, 4), (6, 10), (20, 22)])
        );
        assert_eq!(a.minus(&Spans::new()), a);
        assert!(Spans::new().minus(&a).is_empty());
        assert_eq!(a.clip((t(8), t(22))), s(&[(8, 10), (20, 22)]));
    }
}
