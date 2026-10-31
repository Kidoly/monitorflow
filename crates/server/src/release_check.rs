//! The daily release check (ADR-0061): asks the release source what the
//! latest release is, verified as the updater will, remembers it for the
//! administrators' banner, and queues `update.available` the first time a
//! newer version is seen. A server without Internet simply never hears of
//! one: a failed check is not an error anyone needs to read.

use std::future::Future;
use std::time::Duration;

use chrono::{DateTime, Utc};
use monitorflow_common::release::Version;
use monitorflow_common::server_update as update;
use serde::Serialize;
use serde_json::json;
use sqlx::PgPool;
use tokio::time::sleep;

use crate::api::server_update::ServerUpdateSettings;

/// Time between two checks.
pub const DAILY: Duration = Duration::from_secs(24 * 60 * 60);
/// Time before the first check, so a start is not held up by the network.
pub const AFTER_START: Duration = Duration::from_secs(60);
/// Time before trying again after a check without an answer, in case the
/// network was only down for a moment.
const RETRY: Duration = Duration::from_secs(60 * 60);

/// A release newer than the running server, as `GET /api/server-update`
/// shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Available {
    pub version: String,
    pub release_notes_url: Option<String>,
    pub first_seen_at: DateTime<Utc>,
}

/// The version of this server process.
pub fn running_version() -> Version {
    monitorflow_common::VERSION
        .parse()
        .expect("the build version is a version")
}

/// Remembers that the source offers `version`, and queues `update.available`
/// if it is new and newer than `running`; returns whether it did. Server-wide,
/// so it goes only to channels for all customers.
pub async fn record(
    pool: &PgPool,
    version: &Version,
    release_notes_url: Option<&str>,
    running: &Version,
) -> Result<bool, sqlx::Error> {
    let mut tx = pool.begin().await?;
    // Only a fresh row has both times equal: an update moves `last_seen_at`.
    let first_seen: bool = sqlx::query_scalar(
        "INSERT INTO seen_releases (version, release_notes_url) VALUES ($1, $2)
         ON CONFLICT (version) DO UPDATE
             SET last_seen_at = now(), release_notes_url = EXCLUDED.release_notes_url
         RETURNING first_seen_at = last_seen_at",
    )
    .bind(version.to_string())
    .bind(release_notes_url)
    .fetch_one(&mut *tx)
    .await?;
    let notify = first_seen && version > running;
    if notify {
        sqlx::query(
            "INSERT INTO notification_outbox (event, details) VALUES ('update.available', $1)",
        )
        .bind(json!({
            "occurred_at": Utc::now(),
            "release": {
                "version": version.to_string(),
                "running_version": running.to_string(),
                "release_notes_url": release_notes_url,
            },
        }))
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(notify)
}

/// What the source last offered, if it is newer than `running`.
pub async fn available(pool: &PgPool, running: &Version) -> Result<Option<Available>, sqlx::Error> {
    let latest: Option<(String, Option<String>, DateTime<Utc>)> = sqlx::query_as(
        "SELECT version, release_notes_url, first_seen_at FROM seen_releases
         ORDER BY last_seen_at DESC LIMIT 1",
    )
    .fetch_optional(pool)
    .await?;
    Ok(
        latest.and_then(|(version, release_notes_url, first_seen_at)| {
            version
                .parse::<Version>()
                .is_ok_and(|v| v > *running)
                .then_some(Available {
                    version,
                    release_notes_url,
                    first_seen_at,
                })
        }),
    )
}

/// One check; returns whether it got an answer.
pub async fn check(pool: &PgPool, settings: &ServerUpdateSettings, running: &Version) -> bool {
    let base = update::release_base(&settings.source, None);
    let candidate = match update::fetch(&settings.client, &base, &settings.keys, None).await {
        Ok(candidate) => candidate,
        Err(error) => {
            tracing::debug!(%error, "no answer from the release source");
            return false;
        }
    };
    let version = candidate.release.version;
    let notes = update::release_notes_url(&settings.source, &version);
    match record(pool, &version, notes.as_deref(), running).await {
        Ok(true) => tracing::info!(%version, "a new release is available"),
        Ok(false) => {}
        Err(error) => {
            tracing::warn!(%error, "could not record the release check");
            return false;
        }
    }
    true
}

/// Checks after `first`, then daily, until `stop` resolves.
pub async fn run(
    pool: PgPool,
    settings: ServerUpdateSettings,
    first: Duration,
    stop: impl Future<Output = ()>,
) {
    tokio::pin!(stop);
    let running = running_version();
    let mut wait = first;
    loop {
        tokio::select! {
            _ = &mut stop => return,
            _ = sleep(wait) => {}
        }
        wait = if check(&pool, &settings, &running).await {
            DAILY
        } else {
            RETRY
        };
    }
}
