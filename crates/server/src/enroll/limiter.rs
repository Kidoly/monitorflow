//! Bounds failed enrollment attempts per source address (ADR-0006).
//!
//! Tokens have 256 bits of entropy, so this is not what stops guessing: it
//! keeps a misbehaving client from loading the database. State is in memory
//! and bounded; when full, untracked sources share one bucket.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Failed attempts allowed per source and window.
pub const MAX_FAILURES: u32 = 10;
pub const WINDOW: Duration = Duration::from_secs(10 * 60);
/// Sources tracked at once.
pub const MAX_SOURCES: usize = 10_000;

/// Source used when the connection has no address.
pub const UNKNOWN_SOURCE: IpAddr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);

/// Bucket shared by sources that do not fit in the table.
const OVERFLOW: IpAddr = IpAddr::V6(Ipv6Addr::UNSPECIFIED);

#[derive(Debug)]
pub struct FailureLimiter {
    max_failures: u32,
    window: Duration,
    max_sources: usize,
    /// Failures and start of the current window, per source.
    sources: Mutex<HashMap<IpAddr, (u32, Instant)>>,
}

impl Default for FailureLimiter {
    fn default() -> Self {
        Self::new(MAX_FAILURES, WINDOW, MAX_SOURCES)
    }
}

impl FailureLimiter {
    pub fn new(max_failures: u32, window: Duration, max_sources: usize) -> Self {
        Self {
            max_failures,
            window,
            max_sources: max_sources.max(1),
            sources: Mutex::new(HashMap::new()),
        }
    }

    /// Whether `source` used up its failures for the current window.
    pub fn is_blocked(&self, source: IpAddr) -> bool {
        self.is_blocked_at(source, Instant::now())
    }

    pub fn record_failure(&self, source: IpAddr) {
        self.record_failure_at(source, Instant::now());
    }

    fn is_blocked_at(&self, source: IpAddr, now: Instant) -> bool {
        let sources = self.sources.lock().unwrap_or_else(PoisonError::into_inner);
        let own = bucket(source);
        let key = if sources.contains_key(&own) || sources.len() < self.max_sources {
            own
        } else {
            OVERFLOW
        };
        sources
            .get(&key)
            .is_some_and(|&(count, start)| count >= self.max_failures && now - start < self.window)
    }

    fn record_failure_at(&self, source: IpAddr, now: Instant) {
        let mut sources = self.sources.lock().unwrap_or_else(PoisonError::into_inner);
        let mut key = bucket(source);
        if !sources.contains_key(&key) && sources.len() >= self.max_sources {
            sources.retain(|_, &mut (_, start)| now - start < self.window);
            if sources.len() >= self.max_sources {
                key = OVERFLOW;
            }
        }
        let entry = sources.entry(key).or_insert((0, now));
        if now - entry.1 >= self.window {
            *entry = (0, now);
        }
        entry.0 = entry.0.saturating_add(1);
    }
}

/// IPv4 addresses count alone; IPv6 by /64, the usual smallest allocation.
fn bucket(source: IpAddr) -> IpAddr {
    match source {
        IpAddr::V4(v4) => IpAddr::V4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => {
                let prefix = u128::from(v6) & !(u128::from(u64::MAX));
                IpAddr::V6(Ipv6Addr::from(prefix))
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn blocks_after_max_failures_until_the_window_ends() {
        let limiter = FailureLimiter::new(3, WINDOW, 100);
        let t0 = Instant::now();
        let a = ip("192.0.2.1");
        for _ in 0..3 {
            assert!(!limiter.is_blocked_at(a, t0));
            limiter.record_failure_at(a, t0);
        }
        assert!(limiter.is_blocked_at(a, t0 + WINDOW / 2));
        assert!(!limiter.is_blocked_at(ip("192.0.2.2"), t0));
        assert!(!limiter.is_blocked_at(a, t0 + WINDOW));
        // A new window starts from zero.
        limiter.record_failure_at(a, t0 + WINDOW);
        assert!(!limiter.is_blocked_at(a, t0 + WINDOW));
    }

    #[test]
    fn ipv6_counts_per_64_and_mapped_ipv4_as_ipv4() {
        let limiter = FailureLimiter::new(2, WINDOW, 100);
        let t0 = Instant::now();
        limiter.record_failure_at(ip("2001:db8:1:2::1"), t0);
        limiter.record_failure_at(ip("2001:db8:1:2:ffff::9"), t0);
        assert!(limiter.is_blocked_at(ip("2001:db8:1:2::42"), t0));
        assert!(!limiter.is_blocked_at(ip("2001:db8:1:3::1"), t0));

        limiter.record_failure_at(ip("::ffff:192.0.2.7"), t0);
        limiter.record_failure_at(ip("192.0.2.7"), t0);
        assert!(limiter.is_blocked_at(ip("192.0.2.7"), t0));
    }

    #[test]
    fn a_full_table_sends_new_sources_to_a_shared_bucket() {
        let limiter = FailureLimiter::new(2, WINDOW, 2);
        let t0 = Instant::now();
        limiter.record_failure_at(ip("192.0.2.1"), t0);
        limiter.record_failure_at(ip("192.0.2.2"), t0);
        limiter.record_failure_at(ip("192.0.2.3"), t0);
        limiter.record_failure_at(ip("192.0.2.4"), t0);
        // .3 and .4 share the overflow bucket, now exhausted, and so does
        // any other untracked source; tracked ones keep their own count.
        assert!(limiter.is_blocked_at(ip("192.0.2.3"), t0));
        assert!(limiter.is_blocked_at(ip("192.0.2.99"), t0));
        assert!(!limiter.is_blocked_at(ip("192.0.2.1"), t0));
        // Expired entries make room again.
        let later = t0 + WINDOW;
        limiter.record_failure_at(ip("192.0.2.5"), later);
        assert!(!limiter.is_blocked_at(ip("192.0.2.5"), later));
    }
}
