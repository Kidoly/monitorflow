//! The rules of automatic updates (ADR-0055), without storage: which
//! release peers move to, which peers move, and how fast.
//!
//! - The target is the newest **stable** release the server holds, never
//!   newer than the server itself (upstream ≥ downstream, ADR-0055).
//! - A peer moves only forward: never to its own version or an older one,
//!   and never when it reports something that is not a version.
//! - Each site starts with [`CANARIES`] peer; once one succeeded, it moves
//!   [`BATCH_PERCENT`] of its peers at a time. A site whose canary failed
//!   waits until an administrator resumes the rollout.
//! - A rollout halts once [`Settings::failure_threshold`] of its updates
//!   failed since it started or was last resumed.

use std::str::FromStr;

use monitorflow_common::release::Version;
use serde::Serialize;

/// Peers of a site updated first, alone.
pub const CANARIES: usize = 1;
/// Share of a site's peers updated at once after its canary, in percent;
/// at least one.
pub const BATCH_PERCENT: usize = 25;
/// Default of [`Settings::failure_threshold`].
pub const DEFAULT_FAILURE_THRESHOLD: u32 = 2;
pub const MAX_FAILURE_THRESHOLD: u32 = 1000;

/// The server-wide settings, as they apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Settings {
    /// The default of sites that inherit it.
    pub enabled: bool,
    /// Whether the configuration file sets `enabled`, which the web
    /// interface then cannot change.
    pub enabled_locked: bool,
    /// Nothing automatic is queued while paused.
    pub paused: bool,
    pub failure_threshold: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            enabled_locked: false,
            paused: false,
            failure_threshold: DEFAULT_FAILURE_THRESHOLD,
        }
    }
}

impl Settings {
    /// Whether a site with this override updates automatically.
    pub fn site_enabled(&self, site: Option<bool>) -> bool {
        site.unwrap_or(self.enabled)
    }
}

/// What peers move to: the newest stable version of `held` that is not
/// newer than `ceiling`. Pre-releases are never automatic.
pub fn target<'a>(held: &'a [Version], ceiling: &Version) -> Option<&'a Version> {
    held.iter()
        .filter(|v| v.pre.is_empty() && *v <= ceiling)
        .max()
}

/// Whether a peer running `running` moves to `target`: only forward, and
/// only from a version (a development build is left alone).
pub fn needs_update(running: &str, target: &Version) -> bool {
    Version::from_str(running).is_ok_and(|v| v < *target)
}

/// Where a site stands in a rollout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SitePhase {
    /// No update succeeded yet: one peer at a time.
    Canary,
    /// The canary succeeded: batches.
    Rolling,
    /// The canary failed and nothing succeeded since.
    Blocked,
}

/// The automatic updates of one site to one version.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Tries {
    /// Queued and not finished.
    pub pending: usize,
    pub succeeded: usize,
    /// Failed since the rollout started or was resumed.
    pub failed: usize,
}

impl Tries {
    pub fn phase(&self) -> SitePhase {
        if self.succeeded > 0 {
            SitePhase::Rolling
        } else if self.failed > 0 {
            SitePhase::Blocked
        } else {
            SitePhase::Canary
        }
    }

    /// How many more peers of a site of `size` peers may start now.
    pub fn allowance(&self, size: usize) -> usize {
        match self.phase() {
            SitePhase::Blocked => 0,
            SitePhase::Canary => CANARIES.saturating_sub(self.pending),
            SitePhase::Rolling => batch(size).saturating_sub(self.pending),
        }
    }
}

/// Peers updated at once in a site of `size` after its canary.
pub fn batch(size: usize) -> usize {
    (size * BATCH_PERCENT).div_ceil(100).max(1)
}

/// Whether `failed` updates halt a rollout.
pub fn halts(failed: usize, threshold: u32) -> bool {
    failed >= threshold.max(1) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        text.parse().unwrap()
    }

    #[test]
    fn the_target_is_the_newest_stable_release_up_to_the_server() {
        let held = [v("0.2.0"), v("0.3.0"), v("0.4.0-rc.1"), v("0.5.0")];
        assert_eq!(target(&held, &v("0.5.0")), Some(&v("0.5.0")));
        assert_eq!(target(&held, &v("1.0.0")), Some(&v("0.5.0")));
        // The server is older than the newest release: it holds peers back.
        assert_eq!(target(&held, &v("0.4.0")), Some(&v("0.3.0")));
        // Pre-releases never, even when they are all there is.
        assert_eq!(target(&[v("0.4.0-rc.1")], &v("1.0.0")), None);
        assert_eq!(target(&[], &v("1.0.0")), None);
        assert_eq!(target(&held, &v("0.1.0")), None);
    }

    #[test]
    fn peers_only_move_forward_from_a_version() {
        let to = v("0.3.0");
        assert!(needs_update("0.2.9", &to));
        assert!(needs_update("0.3.0-rc.1", &to));
        assert!(!needs_update("0.3.0", &to), "same version");
        assert!(!needs_update("0.4.0", &to), "never a downgrade");
        assert!(!needs_update("dev", &to), "not a version");
        assert!(!needs_update("", &to));
    }

    #[test]
    fn sites_inherit_unless_they_override() {
        let on = Settings::default();
        let off = Settings {
            enabled: false,
            ..on
        };
        assert!(on.enabled, "on by default");
        assert!(on.site_enabled(None));
        assert!(!on.site_enabled(Some(false)));
        assert!(!off.site_enabled(None));
        assert!(off.site_enabled(Some(true)));
    }

    #[test]
    fn a_site_starts_with_one_canary() {
        let fresh = Tries::default();
        assert_eq!(fresh.phase(), SitePhase::Canary);
        assert_eq!(fresh.allowance(40), 1);
        let waiting = Tries {
            pending: 1,
            ..fresh
        };
        assert_eq!(waiting.allowance(40), 0, "one canary at a time");
    }

    #[test]
    fn a_failed_canary_blocks_the_site() {
        let failed = Tries {
            failed: 1,
            ..Tries::default()
        };
        assert_eq!(failed.phase(), SitePhase::Blocked);
        assert_eq!(failed.allowance(40), 0);
    }

    #[test]
    fn after_the_canary_a_site_moves_in_batches() {
        let rolling = Tries {
            succeeded: 1,
            ..Tries::default()
        };
        assert_eq!(rolling.phase(), SitePhase::Rolling);
        assert_eq!(rolling.allowance(40), 10);
        let busy = Tries {
            pending: 7,
            succeeded: 1,
            failed: 1,
        };
        assert_eq!(busy.phase(), SitePhase::Rolling);
        assert_eq!(busy.allowance(40), 3);
        assert_eq!(batch(1), 1);
        assert_eq!(batch(3), 1);
        assert_eq!(batch(5), 2);
        assert_eq!(batch(0), 1);
    }

    #[test]
    fn failures_halt_at_the_threshold() {
        assert!(!halts(0, 2));
        assert!(!halts(1, 2));
        assert!(halts(2, 2));
        assert!(halts(1, 1));
        assert!(halts(1, 0), "a threshold of 0 acts as 1");
    }
}
