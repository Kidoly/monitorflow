//! Bounds sign-in attempts per source address and per account (ADR-0106).
//!
//! Every attempt is counted before the password is verified, so requests
//! arriving together cannot all reach argon2, and given back when the
//! password was right. State is in memory, per instance of the central
//! server, and reset on restart.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use ring::digest::{digest, SHA256};

use crate::enroll::limiter::{bucket, Attempts, FailureLimiter, MAX_FAILURES, MAX_SOURCES, WINDOW};

/// Wrong attempts allowed on one account per [`WINDOW`], from all sources
/// together. Above what one source may do, so a single address is stopped
/// by its own limit first.
pub const MAX_ACCOUNT_FAILURES: u32 = 20;
/// Sources remembered per account as having signed in to it.
const KNOWN_SOURCES: usize = 4;

/// An account as typed, known or not: the hash of its lowercase name, so
/// a long name costs no memory.
type AccountKey = [u8; 32];

/// Which limit a wrong attempt used up, for the audit log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    Source,
    Account,
}

impl Limit {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Account => "account",
        }
    }
}

/// An attempt that may go on to verify the password. Give it back with
/// [`LoginLimits::succeeded`] or [`LoginLimits::failed`].
#[derive(Debug)]
pub struct Attempt {
    source: IpAddr,
    account: AccountKey,
    /// False when the account was full and the source let through as known.
    counted_account: bool,
}

#[derive(Debug)]
pub struct LoginLimits {
    sources: FailureLimiter,
    accounts: Attempts<AccountKey>,
    /// Sources that signed in to an account, most recent last. Only a
    /// right password adds one, so this holds at most [`KNOWN_SOURCES`]
    /// per user.
    known: Mutex<HashMap<AccountKey, Vec<IpAddr>>>,
}

impl Default for LoginLimits {
    fn default() -> Self {
        Self::new(MAX_FAILURES, MAX_ACCOUNT_FAILURES, WINDOW)
    }
}

impl LoginLimits {
    pub fn new(per_source: u32, per_account: u32, window: Duration) -> Self {
        Self {
            sources: FailureLimiter::new(per_source, window, MAX_SOURCES),
            accounts: Attempts::new(per_account, window, MAX_SOURCES, [0; 32]),
            known: Mutex::default(),
        }
    }

    /// Counts an attempt on `username` from `source`, or `None` when
    /// either is over its limit. The answer depends only on what was
    /// typed and where from, never on whether the account exists.
    ///
    /// A full account still lets through a source that signed in to it
    /// before, within that source's own limit: wrong passwords sent from
    /// elsewhere do not lock people out of their usual place.
    pub fn begin(&self, source: IpAddr, username: &str) -> Option<Attempt> {
        if !self.sources.attempt(source) {
            return None;
        }
        let account = account_key(username);
        let counted_account = self.accounts.attempt(&account);
        if !counted_account && !self.is_known(&account, source) {
            // Refused without verifying anything: not held against the
            // source.
            self.sources.forgive(source);
            return None;
        }
        Some(Attempt {
            source,
            account,
            counted_account,
        })
    }

    /// The password was right: the attempt no longer counts, and the
    /// source is remembered for the account.
    pub fn succeeded(&self, attempt: Attempt) {
        self.sources.forgive(attempt.source);
        if attempt.counted_account {
            self.accounts.forgive(&attempt.account);
        }
        let source = bucket(attempt.source);
        let mut known = self.known.lock().unwrap_or_else(PoisonError::into_inner);
        let sources = known.entry(attempt.account).or_default();
        sources.retain(|s| *s != source);
        if sources.len() >= KNOWN_SOURCES {
            sources.remove(0);
        }
        sources.push(source);
    }

    /// The attempt failed and stays counted. Returns the limit it used
    /// up, if any: the next attempt is refused.
    pub fn failed(&self, attempt: &Attempt) -> Option<Limit> {
        if self.sources.is_blocked(attempt.source) {
            Some(Limit::Source)
        } else if attempt.counted_account && self.accounts.is_blocked(&attempt.account) {
            Some(Limit::Account)
        } else {
            None
        }
    }

    fn is_known(&self, account: &AccountKey, source: IpAddr) -> bool {
        let known = self.known.lock().unwrap_or_else(PoisonError::into_inner);
        known
            .get(account)
            .is_some_and(|sources| sources.contains(&bucket(source)))
    }
}

fn account_key(username: &str) -> AccountKey {
    let mut key = [0; 32];
    key.copy_from_slice(digest(&SHA256, username.to_lowercase().as_bytes()).as_ref());
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(last: u8) -> IpAddr {
        IpAddr::from([192, 0, 2, last])
    }

    fn fail(limits: &LoginLimits, source: IpAddr, username: &str) -> Option<Option<Limit>> {
        limits
            .begin(source, username)
            .map(|attempt| limits.failed(&attempt))
    }

    #[test]
    fn a_source_is_stopped_after_its_failures_whatever_the_account() {
        let limits = LoginLimits::new(3, 100, WINDOW);
        assert_eq!(fail(&limits, ip(1), "a"), Some(None));
        assert_eq!(fail(&limits, ip(1), "b"), Some(None));
        assert_eq!(fail(&limits, ip(1), "c"), Some(Some(Limit::Source)));
        assert!(limits.begin(ip(1), "d").is_none());
        // Another source is not concerned.
        assert_eq!(fail(&limits, ip(2), "a"), Some(None));
    }

    #[test]
    fn right_passwords_do_not_count() {
        let limits = LoginLimits::new(2, 2, WINDOW);
        for _ in 0..10 {
            let attempt = limits.begin(ip(1), "root").unwrap();
            limits.succeeded(attempt);
        }
        assert_eq!(fail(&limits, ip(1), "root"), Some(None));
    }

    #[test]
    fn attempts_are_counted_before_they_are_verified() {
        let limits = LoginLimits::new(2, 100, WINDOW);
        // Two requests in flight use the source up; a third does not start.
        let first = limits.begin(ip(1), "root").unwrap();
        let _second = limits.begin(ip(1), "root").unwrap();
        assert!(limits.begin(ip(1), "root").is_none());
        limits.succeeded(first);
        assert!(limits.begin(ip(1), "root").is_some());
    }

    #[test]
    fn an_account_is_stopped_across_sources_by_name_in_any_case() {
        let limits = LoginLimits::new(100, 3, WINDOW);
        assert_eq!(fail(&limits, ip(1), "Root"), Some(None));
        assert_eq!(fail(&limits, ip(2), "ROOT"), Some(None));
        assert_eq!(fail(&limits, ip(3), "root"), Some(Some(Limit::Account)));
        assert!(limits.begin(ip(4), "root").is_none());
        // Other accounts, real or not, are not concerned, and being
        // refused on a full account is not held against the source.
        for _ in 0..200 {
            assert!(limits.begin(ip(4), "rOOt").is_none());
        }
        assert_eq!(fail(&limits, ip(4), "bob"), Some(None));
    }

    #[test]
    fn a_full_account_still_lets_a_known_source_in_within_its_own_limit() {
        let limits = LoginLimits::new(3, 2, WINDOW);
        let attempt = limits.begin(ip(9), "root").unwrap();
        limits.succeeded(attempt);
        assert_eq!(fail(&limits, ip(1), "root"), Some(None));
        assert_eq!(fail(&limits, ip(2), "root"), Some(Some(Limit::Account)));
        assert!(limits.begin(ip(3), "root").is_none());

        // The usual place gets in, and a typo there is its own business.
        assert_eq!(fail(&limits, ip(9), "root"), Some(None));
        let attempt = limits.begin(ip(9), "root").unwrap();
        limits.succeeded(attempt);
        assert_eq!(fail(&limits, ip(9), "root"), Some(None));
        assert_eq!(fail(&limits, ip(9), "root"), Some(Some(Limit::Source)));
        assert!(limits.begin(ip(9), "root").is_none());
    }

    #[test]
    fn only_the_last_sources_of_an_account_stay_known() {
        let limits = LoginLimits::new(100, 1, WINDOW);
        for last in 1..=5 {
            let attempt = limits.begin(ip(last), "root").unwrap();
            limits.succeeded(attempt);
        }
        assert_eq!(fail(&limits, ip(50), "root"), Some(Some(Limit::Account)));
        assert!(limits.begin(ip(1), "root").is_none());
        assert!(limits.begin(ip(5), "root").is_some());
    }
}
