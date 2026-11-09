//! Bounds failed enrollment attempts per source address (ADR-0007).
//!
//! Tokens have 256 bits of entropy, so this is not what stops guessing: it
//! keeps a misbehaving client from loading the database. State is in memory
//! and bounded; when full, untracked sources share one bucket.

use std::collections::HashMap;
use std::hash::Hash;
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

/// A fixed-window count per key, in memory and bounded: when the table
/// is full, untracked keys share the `overflow` bucket.
#[derive(Debug)]
pub struct Attempts<K> {
    max: u32,
    window: Duration,
    max_keys: usize,
    overflow: K,
    /// Count and start of the current window, per key.
    entries: Mutex<HashMap<K, (u32, Instant)>>,
}

impl<K: Hash + Eq + Clone> Attempts<K> {
    pub fn new(max: u32, window: Duration, max_keys: usize, overflow: K) -> Self {
        Self {
            max,
            window,
            max_keys: max_keys.max(1),
            overflow,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Whether `key` used up the current window.
    pub fn is_blocked(&self, key: &K) -> bool {
        self.is_blocked_at(key, Instant::now())
    }

    /// Counts one more for `key`, blocked or not.
    pub fn record(&self, key: &K) {
        self.count_at(key, Instant::now(), false);
    }

    /// Counts one more for `key` unless it is blocked, in one step, so
    /// requests arriving together cannot all pass. False when blocked.
    pub fn attempt(&self, key: &K) -> bool {
        self.count_at(key, Instant::now(), true)
    }

    /// Takes back one count of `key`: an attempt that turned out fine.
    pub fn forgive(&self, key: &K) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let key = if entries.contains_key(key) {
            key
        } else {
            &self.overflow
        };
        if let Some(entry) = entries.get_mut(key) {
            entry.0 = entry.0.saturating_sub(1);
        }
    }

    pub(crate) fn is_blocked_at(&self, key: &K, now: Instant) -> bool {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let key = if entries.contains_key(key) || entries.len() < self.max_keys {
            key
        } else {
            &self.overflow
        };
        entries
            .get(key)
            .is_some_and(|&(count, start)| count >= self.max && now - start < self.window)
    }

    /// Counts one more for `key`; with `unless_blocked`, only below the
    /// limit. Whether it counted.
    pub(crate) fn count_at(&self, key: &K, now: Instant, unless_blocked: bool) -> bool {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let mut key = key;
        if !entries.contains_key(key) && entries.len() >= self.max_keys {
            entries.retain(|_, &mut (_, start)| now - start < self.window);
            if entries.len() >= self.max_keys {
                key = &self.overflow;
            }
        }
        let entry = entries.entry(key.clone()).or_insert((0, now));
        if now - entry.1 >= self.window {
            *entry = (0, now);
        }
        if unless_blocked && entry.0 >= self.max {
            return false;
        }
        entry.0 = entry.0.saturating_add(1);
        true
    }
}

/// [`Attempts`] per source address.
#[derive(Debug)]
pub struct FailureLimiter(Attempts<IpAddr>);

impl Default for FailureLimiter {
    fn default() -> Self {
        Self::new(MAX_FAILURES, WINDOW, MAX_SOURCES)
    }
}

impl FailureLimiter {
    pub fn new(max_failures: u32, window: Duration, max_sources: usize) -> Self {
        Self(Attempts::new(max_failures, window, max_sources, OVERFLOW))
    }

    /// Whether `source` used up its failures for the current window.
    pub fn is_blocked(&self, source: IpAddr) -> bool {
        self.0.is_blocked(&bucket(source))
    }

    pub fn record_failure(&self, source: IpAddr) {
        self.0.record(&bucket(source));
    }

    /// See [`Attempts::attempt`].
    pub fn attempt(&self, source: IpAddr) -> bool {
        self.0.attempt(&bucket(source))
    }

    /// See [`Attempts::forgive`].
    pub fn forgive(&self, source: IpAddr) {
        self.0.forgive(&bucket(source));
    }

    #[cfg(test)]
    fn is_blocked_at(&self, source: IpAddr, now: Instant) -> bool {
        self.0.is_blocked_at(&bucket(source), now)
    }

    #[cfg(test)]
    fn record_failure_at(&self, source: IpAddr, now: Instant) {
        self.0.count_at(&bucket(source), now, false);
    }
}

/// IPv4 addresses count alone; IPv6 by /64, the usual smallest allocation.
pub(crate) fn bucket(source: IpAddr) -> IpAddr {
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

    #[test]
    fn attempts_stop_at_the_limit_and_can_be_forgiven() {
        let attempts = Attempts::new(2, WINDOW, 100, "overflow");
        let t0 = Instant::now();
        assert!(attempts.count_at(&"a", t0, true));
        assert!(attempts.count_at(&"a", t0, true));
        // Refused attempts do not count further.
        assert!(!attempts.count_at(&"a", t0, true));
        assert!(attempts.is_blocked_at(&"a", t0));
        attempts.forgive(&"a");
        assert!(!attempts.is_blocked_at(&"a", t0));
        assert!(attempts.count_at(&"a", t0, true));
        assert!(attempts.count_at(&"a", t0 + WINDOW, true));
        // Forgiving a key never seen changes nothing.
        attempts.forgive(&"b");
        assert!(!attempts.is_blocked_at(&"b", t0));
    }
}
