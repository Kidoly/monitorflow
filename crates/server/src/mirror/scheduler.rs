//! Periodic syncs (ADR-0030): every poll, a sync of each mirror whose
//! `sync_interval_s` has passed since its last sync started, at most
//! `max_concurrent_syncs` at a time. Failures are in the runs; the next
//! attempt comes at the next interval.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

use super::jobs::{JobError, JobKind, Mirrors};
use super::model::Trigger;
use super::store;

/// How often due mirrors are looked for.
pub const POLL: Duration = Duration::from_secs(60);

/// Starts the syncs due now that `slots` has room for, and returns them.
pub async fn tick(mirrors: &Mirrors, slots: &Arc<Semaphore>) -> Vec<JoinHandle<()>> {
    let due = match store::due(mirrors.pool()).await {
        Ok(due) => due,
        Err(error) => {
            tracing::warn!(%error, "cannot list mirrors due for a sync");
            return Vec::new();
        }
    };
    let mut started = Vec::new();
    for mirror in due {
        let Ok(slot) = slots.clone().try_acquire_owned() else {
            break;
        };
        match mirrors
            .begin(&mirror, JobKind::Sync, Trigger::Schedule, None)
            .await
        {
            Ok(job) => started.push(tokio::spawn(async move {
                job.execute().await;
                drop(slot);
            })),
            // Started from elsewhere in the meantime.
            Err(JobError::Busy) => {}
            Err(error) => {
                tracing::warn!(mirror = %mirror.name, %error, "cannot start a scheduled sync")
            }
        }
    }
    started
}

/// Runs [`tick`] every `poll` until `shutdown` resolves. Jobs still
/// running then are dropped with the runtime; their runs are marked
/// interrupted by the next job of their mirror.
pub async fn run(
    mirrors: Mirrors,
    max_concurrent: usize,
    poll: Duration,
    shutdown: impl Future<Output = ()>,
) {
    let slots = Arc::new(Semaphore::new(max_concurrent));
    let mut interval = tokio::time::interval(poll);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = interval.tick() => {
                tick(&mirrors, &slots).await;
            }
        }
    }
}
