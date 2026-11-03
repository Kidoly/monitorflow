//! A token bucket over caller-given times, in milliseconds: the journal
//! forwarder feeds it the lines' own timestamps, the server the time since
//! the stream opened (ADR-0091).

/// Lets `burst` items through at once, then `rate_per_second`.
#[derive(Debug, Clone)]
pub struct TokenBucket {
    tokens: f64,
    last_ms: Option<i64>,
    rate: f64,
    burst: f64,
}

impl TokenBucket {
    pub fn new(rate_per_second: f64, burst: f64) -> Self {
        Self {
            tokens: burst,
            last_ms: None,
            rate: rate_per_second,
            burst,
        }
    }

    /// Whether an item at `at_ms` passes. Time going backwards refills
    /// nothing.
    pub fn allow(&mut self, at_ms: i64) -> bool {
        if let Some(last) = self.last_ms {
            let elapsed = (at_ms - last).max(0) as f64 / 1000.0;
            self.tokens = (self.tokens + elapsed * self.rate).min(self.burst);
        }
        self.last_ms = Some(self.last_ms.map_or(at_ms, |last| last.max(at_ms)));
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_then_the_rate() {
        let mut bucket = TokenBucket::new(2.0, 3.0);
        assert!((0..3).all(|_| bucket.allow(1_000)));
        assert!(!bucket.allow(1_000));
        // Half a second refills one.
        assert!(bucket.allow(1_500));
        assert!(!bucket.allow(1_500));
        // Spread out, everything passes.
        assert!((1..=100).all(|i| bucket.allow(1_500 + i * 500)));
        // Never more than the burst after a long pause.
        assert_eq!((0..10).filter(|_| bucket.allow(1_000_000)).count(), 3);
        // Time going backwards refills nothing.
        assert!(!bucket.allow(10));
    }
}
