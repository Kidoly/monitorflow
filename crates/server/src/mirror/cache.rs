//! The storage cap of on-demand mirrors (ADR-0142): set by a global
//! administrator in `server_settings.mirror_cache_bytes`, by default 100
//! GiB or half the disk of `mirror_dir`. [`run`] evicts down to it every
//! [`EVERY`] and soon after a download makes the store grow.

use std::future::Future;
use std::time::{Duration, SystemTime};

use monitorflow_common::mirror::evict::{self, Outcome};
use serde_json::json;
use sqlx::PgPool;

use super::catalog;
use super::model::Family;
use super::{Layout, Mirrors};
use crate::api::audit::{self, Event};

/// How often the store is checked when nothing grows it.
pub const EVERY: Duration = Duration::from_secs(3600);
/// Smallest cap an administrator can set.
pub const MIN_CAP: u64 = evict::GIB;
/// Wait after a download, so that a burst of them makes one pass.
const SETTLE: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("database query failed: {0}")]
    Database(#[from] sqlx::Error),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// The cap in force and where it comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cap {
    pub bytes: u64,
    pub default_bytes: u64,
    /// Set by an administrator rather than the default.
    pub set: bool,
}

/// The default cap for the disk of `layout`.
pub async fn default_cap(layout: &Layout) -> u64 {
    let root = layout.root().to_owned();
    let disk = tokio::task::spawn_blocking(move || catalog::disk(&root))
        .await
        .ok()
        .flatten();
    evict::default_cap(disk.map(|d| d.total_bytes))
}

/// The cap in force.
pub async fn cap(pool: &PgPool, layout: &Layout) -> Result<Cap, sqlx::Error> {
    let stored: Option<Option<i64>> =
        sqlx::query_scalar("SELECT mirror_cache_bytes FROM server_settings")
            .fetch_optional(pool)
            .await?;
    let default_bytes = default_cap(layout).await;
    let stored = stored.flatten().and_then(|b| u64::try_from(b).ok());
    Ok(Cap {
        bytes: stored.unwrap_or(default_bytes),
        default_bytes,
        set: stored.is_some(),
    })
}

/// Sets the cap, `None` for the default, audited as `mirror.cache_cap`.
pub async fn set_cap(
    pool: &PgPool,
    bytes: Option<u64>,
    actor: (&str, &str),
) -> Result<(), sqlx::Error> {
    let stored = bytes.map(|b| i64::try_from(b).unwrap_or(i64::MAX));
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO server_settings (singleton, mirror_cache_bytes) VALUES (true, $1)
         ON CONFLICT (singleton) DO UPDATE SET mirror_cache_bytes = EXCLUDED.mirror_cache_bytes",
    )
    .bind(stored)
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut *tx,
        Event {
            actor_kind: actor.0,
            actor_id: actor.1,
            action: "mirror.cache_cap",
            target: None,
            details: json!({ "cap_bytes": bytes }),
        },
    )
    .await?;
    tx.commit().await
}

/// Evicts down to the cap now, and remembers the outcome for the page.
pub async fn evict_now(mirrors: &Mirrors) -> Result<Outcome, CacheError> {
    let layout = mirrors.layout().clone();
    let cap = cap(mirrors.pool(), &layout).await?.bytes;
    let outcome = tokio::task::spawn_blocking(move || {
        let (apt, dnf) = (layout.served(Family::Apt), layout.served(Family::Dnf));
        evict::evict(&layout.store(), &[&apt, &dnf], cap, SystemTime::now())
    })
    .await
    .map_err(std::io::Error::other)??;
    if outcome.evicted > 0 {
        tracing::info!(
            packages = outcome.evicted,
            freed_bytes = outcome.freed_bytes,
            used_bytes = outcome.used_bytes,
            cap_bytes = cap,
            "on-demand packages evicted"
        );
    }
    if outcome.over_cap {
        tracing::warn!(
            used_bytes = outcome.used_bytes,
            protected_bytes = outcome.protected_bytes,
            cap_bytes = cap,
            "the on-demand store stays over its cap: campaigns and current snapshots hold more"
        );
    }
    mirrors.set_last_eviction(outcome);
    Ok(outcome)
}

/// Evicts every [`EVERY`], and once downloads settle after the store
/// grew, until `shutdown` resolves.
pub async fn run(mirrors: Mirrors, shutdown: impl Future<Output = ()>) {
    tokio::pin!(shutdown);
    loop {
        if let Err(error) = evict_now(&mirrors).await {
            tracing::warn!(%error, "cannot keep the on-demand store under its cap");
        }
        tokio::select! {
            () = &mut shutdown => return,
            () = tokio::time::sleep(EVERY) => {}
            () = mirrors.grew() => {
                tokio::select! {
                    () = &mut shutdown => return,
                    () = tokio::time::sleep(SETTLE) => {}
                }
            }
        }
    }
}
