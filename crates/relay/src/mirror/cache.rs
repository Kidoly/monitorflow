//! On-demand packages on a relay (ADR-0142): what its agents ask for and
//! its snapshots do not hold is fetched from its upstream's listener at
//! the same URL, which fetches on a miss in turn, checked against the
//! relay's own package list and kept in its store under
//! `mirror.cache_max_bytes`, with the server's eviction rules.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use monitorflow_common::mirror::demand::{OnDemand, Source};
use monitorflow_common::mirror::evict;
use monitorflow_common::mirror::repo::fetch::Fetcher;
use monitorflow_common::mirror::store::Store;
use monitorflow_common::mirror::{Family, Served};
use tokio::sync::Notify;

use monitorflow_common::client::shutdown::Shutdown;

/// How often the store is checked when nothing grows it.
pub const EVERY: Duration = Duration::from_secs(3600);
/// Wait after a download, so that a burst of them makes one pass.
const SETTLE: Duration = Duration::from_secs(30);

/// The fetcher of the listener under `root`, and what it tells eviction.
pub fn ondemand(root: &Path, upstream: Fetcher) -> (OnDemand, Arc<Notify>) {
    let grew = Arc::new(Notify::new());
    let told = grew.clone();
    let ondemand = OnDemand::with_hook(
        Store::under(root),
        Source::Listener(upstream),
        Some(Box::new(move |_| told.notify_one())),
    );
    (ondemand, grew)
}

/// The cap: `configured`, or the default for the disk of `root`.
pub fn cap(root: &Path, configured: Option<u64>) -> u64 {
    configured.unwrap_or_else(|| {
        let total = rustix::fs::statvfs(root)
            .ok()
            .map(|s| s.f_blocks.saturating_mul(s.f_frsize));
        evict::default_cap(total)
    })
}

/// One eviction pass over the store under `root`. Blocking.
pub fn evict_once(root: &Path, cap: u64) -> std::io::Result<evict::Outcome> {
    let served = Served::under(root);
    let roots = [served.root(Family::Apt), served.root(Family::Dnf)];
    evict::evict(&Store::under(root), &roots, cap, SystemTime::now())
}

/// Evicts every [`EVERY`] and once downloads settle, until `stop`.
pub async fn run(root: PathBuf, configured: Option<u64>, grew: Arc<Notify>, mut stop: Shutdown) {
    loop {
        let dir = root.clone();
        let result =
            tokio::task::spawn_blocking(move || evict_once(&dir, cap(&dir, configured))).await;
        match result {
            Ok(Ok(outcome)) => {
                if outcome.evicted > 0 {
                    tracing::info!(
                        packages = outcome.evicted,
                        freed_bytes = outcome.freed_bytes,
                        used_bytes = outcome.used_bytes,
                        "on-demand packages evicted"
                    );
                }
                if outcome.over_cap {
                    tracing::warn!(used_bytes = outcome.used_bytes, cap_bytes = outcome.cap_bytes, "the on-demand store stays over mirror.cache_max_bytes: pinned and current snapshots hold more");
                }
            }
            Ok(Err(error)) => {
                tracing::warn!(%error, "cannot keep the on-demand store under its cap")
            }
            Err(error) => tracing::warn!(%error, "cannot keep the on-demand store under its cap"),
        }
        tokio::select! {
            () = stop.wait() => return,
            () = tokio::time::sleep(EVERY) => {}
            () = grew.notified() => {
                tokio::select! {
                    () = stop.wait() => return,
                    () = tokio::time::sleep(SETTLE) => {}
                }
            }
        }
    }
}
