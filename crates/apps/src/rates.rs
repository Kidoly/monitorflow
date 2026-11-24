//! What a check keeps between collections to report changes: counters
//! that only grow, turned into a difference or a rate per second.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

/// The counters of the last collection and when it was.
#[derive(Debug, Default)]
pub struct Rates {
    last: Mutex<Option<(Instant, HashMap<String, f64>)>>,
}

/// The counters of one collection, compared with the last one.
#[derive(Debug)]
pub struct Round {
    elapsed: Option<f64>,
    before: HashMap<String, f64>,
    now: HashMap<String, f64>,
}

impl Rates {
    /// Starts a round; [`Rates::finish`] keeps its counters.
    pub fn round(&self) -> Round {
        let last = self.last.lock().unwrap_or_else(|e| e.into_inner()).take();
        let (elapsed, before) = match last {
            Some((at, before)) => (Some(at.elapsed().as_secs_f64()), before),
            None => (None, HashMap::new()),
        };
        Round {
            elapsed,
            before,
            now: HashMap::new(),
        }
    }

    pub fn finish(&self, round: Round) {
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), round.now));
    }

    /// Forgets the last round, after a failure or a restart.
    pub fn reset(&self) {
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

impl Round {
    /// How much `key` grew since the last round; `None` the first time,
    /// or when it went back (a restart, statistics reset).
    pub fn delta(&mut self, key: &str, value: f64) -> Option<f64> {
        self.now.insert(key.to_owned(), value);
        let before = *self.before.get(key)?;
        (value >= before).then_some(value - before)
    }

    /// [`Round::delta`] per second.
    pub fn rate(&mut self, key: &str, value: f64) -> Option<f64> {
        let delta = self.delta(key, value)?;
        let elapsed = self.elapsed.filter(|e| *e > 0.0)?;
        Some(delta / elapsed)
    }

    /// The share of hits among hits and misses since the last round.
    pub fn hit_ratio(&mut self, key: &str, hits: f64, misses: f64) -> Option<f64> {
        // Both kept for the next round before either can stop here.
        let hits = self.delta(&format!("{key}_hits"), hits);
        let misses = self.delta(&format!("{key}_misses"), misses);
        let (hits, misses) = (hits?, misses?);
        let total = hits + misses;
        (total > 0.0).then(|| hits / total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deltas_rates_and_ratios() {
        let rates = Rates::default();
        let mut round = rates.round();
        assert_eq!(round.delta("a", 10.0), None);
        assert_eq!(round.hit_ratio("c", 90.0, 10.0), None);
        rates.finish(round);
        let mut round = rates.round();
        assert_eq!(round.delta("a", 15.0), Some(5.0));
        assert!(round.rate("b", 1.0).is_none());
        assert_eq!(round.hit_ratio("c", 180.0, 20.0), Some(0.9));
        rates.finish(round);
        let mut round = rates.round();
        // A counter that went back gives nothing.
        assert_eq!(round.delta("a", 3.0), None);
        // No traffic: no ratio.
        assert_eq!(round.hit_ratio("c", 180.0, 20.0), None);
        rates.finish(round);
        rates.reset();
        assert_eq!(rates.round().delta("a", 4.0), None);
    }
}
