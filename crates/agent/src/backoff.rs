//! Exponential backoff with full jitter: the n-th retry waits a uniformly
//! random time between zero and `min(cap, base * 2^n)`, so a fleet that lost
//! its server at the same moment does not reconnect in lockstep.

use std::time::Duration;

use ring::rand::{SecureRandom, SystemRandom};

/// Reconnect delays for links to the upstream and to the enroll endpoint.
#[derive(Debug, Clone)]
pub struct Backoff {
    base: Duration,
    cap: Duration,
    attempt: u32,
}

impl Default for Backoff {
    /// 1 s doubling up to 60 s.
    fn default() -> Self {
        Self::new(Duration::from_secs(1), Duration::from_secs(60))
    }
}

impl Backoff {
    pub fn new(base: Duration, cap: Duration) -> Self {
        Self {
            base,
            cap: cap.max(base),
            attempt: 0,
        }
    }

    /// Upper bound of the next delay.
    pub fn ceiling(&self) -> Duration {
        // 2^31 times any useful base is far above any useful cap.
        let factor = 1u32 << self.attempt.min(31);
        self.base.saturating_mul(factor).min(self.cap)
    }

    /// The delay before the next attempt, then counts the attempt.
    pub fn next_delay(&mut self) -> Duration {
        let delay = self.delay_with(random_u64());
        self.attempt = self.attempt.saturating_add(1);
        delay
    }

    /// The delay [`Self::next_delay`] would return for this random value.
    pub fn delay_with(&self, random: u64) -> Duration {
        let ceiling = self.ceiling().as_millis().min(u128::from(u64::MAX - 1)) as u64;
        Duration::from_millis(random % (ceiling + 1))
    }

    /// Back to `base`, after a connection proved healthy.
    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    pub fn attempt(&self) -> u32 {
        self.attempt
    }
}

/// A random value for jitter, nothing secret depends on it.
fn random_u64() -> u64 {
    let mut bytes = [0u8; 8];
    // SystemRandom only fails if the OS has no entropy source at all;
    // a fixed value then just means no jitter.
    let _ = SystemRandom::new().fill(&mut bytes);
    u64::from_le_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEC: Duration = Duration::from_secs(1);

    #[test]
    fn ceiling_doubles_up_to_the_cap() {
        let mut backoff = Backoff::default();
        let ceilings: Vec<u64> = (0..9)
            .map(|_| {
                let ceiling = backoff.ceiling().as_secs();
                backoff.next_delay();
                ceiling
            })
            .collect();
        assert_eq!(ceilings, [1, 2, 4, 8, 16, 32, 60, 60, 60]);
        for _ in 0..100 {
            backoff.next_delay();
        }
        assert_eq!(backoff.ceiling(), 60 * SEC);
    }

    #[test]
    fn delay_is_full_jitter_within_the_ceiling() {
        let mut backoff = Backoff::default();
        for _ in 0..3 {
            backoff.next_delay();
        }
        assert_eq!(backoff.delay_with(0), Duration::ZERO);
        assert_eq!(backoff.delay_with(8000), 8 * SEC);
        assert_eq!(backoff.delay_with(8001), Duration::ZERO);
        assert_eq!(
            backoff.delay_with(u64::MAX),
            Duration::from_millis(u64::MAX % 8001)
        );
        for _ in 0..1000 {
            assert!(backoff.clone().next_delay() <= 8 * SEC);
        }
    }

    #[test]
    fn reset_starts_over() {
        let mut backoff = Backoff::new(Duration::from_millis(100), SEC);
        for _ in 0..10 {
            backoff.next_delay();
        }
        assert_eq!((backoff.attempt(), backoff.ceiling()), (10, SEC));
        backoff.reset();
        assert_eq!(backoff.ceiling(), Duration::from_millis(100));
    }

    #[test]
    fn cap_below_base_is_raised() {
        let backoff = Backoff::new(5 * SEC, SEC);
        assert_eq!(backoff.ceiling(), 5 * SEC);
    }
}
