//! Advances campaigns (ADR-0033). One task for the whole server: every
//! [`POLL`], and when woken by the API, each `scheduled` or `running`
//! campaign takes one step in a transaction holding its row lock:
//!
//! 1. the wave in progress fails the campaign when its failures exceed
//!    the threshold, and is finished once every server has a result;
//! 2. the servers of the current wave without a command get one, only
//!    inside a maintenance window, with a TTL capped at its end;
//! 3. past the last wave, the campaign is completed.
//!
//! A step only reads `commands` and the campaign's tables, so running it
//! twice, or again after a restart, queues nothing twice.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use monitorflow_common::command::APPLY_UPDATES;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use tokio::sync::Notify;
use uuid::Uuid;

use super::store::{cancel_queued, close_waves, Actor, CampaignRow, COLUMNS};
use crate::api::audit::{self, Event};
use crate::commands::{CommandStore, Commands, NewCommand};

/// How often active campaigns are looked at.
pub const POLL: Duration = Duration::from_secs(10);
/// A window must stay open this long for commands to be queued: the
/// shortest command TTL.
pub const MIN_WINDOW_LEFT: Duration = Duration::from_secs(60);

const ACTOR: Actor<'static> = ("system", "campaign-runner");

/// What one step did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Locked elsewhere, no longer active, or nothing to do now.
    Idle,
    /// Commands queued for these agents.
    Queued(Vec<String>),
    Completed,
    Failed,
}

/// Handle on the runner; clones share it.
#[derive(Debug, Clone)]
pub struct Runner {
    pool: PgPool,
    commands: Option<Commands>,
    wake: Arc<Notify>,
}

impl Runner {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            commands: None,
            wake: Arc::new(Notify::new()),
        }
    }

    /// Wakes `commands`' dispatcher for agents that got a command.
    pub fn with_commands(mut self, commands: Commands) -> Self {
        self.commands = Some(commands);
        self
    }

    /// Asks [`Self::run`] for a step of every campaign now, as after a
    /// start or resume.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// One step of every active campaign, oldest first, at `now`.
    /// Failures are logged; the campaign is retried at the next tick.
    pub async fn tick(&self, now: DateTime<Utc>) {
        let ids: Vec<Uuid> = match sqlx::query_scalar(
            "SELECT id FROM campaigns WHERE state IN ('scheduled', 'running') ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await
        {
            Ok(ids) => ids,
            Err(error) => {
                tracing::warn!(%error, "cannot list active campaigns; will retry");
                return;
            }
        };
        for id in ids {
            if let Err(error) = self.advance(id, now).await {
                tracing::warn!(campaign = %id, %error, "cannot advance a campaign; will retry");
            }
        }
    }

    /// One step of the campaign `id` at `now`.
    pub async fn advance(&self, id: Uuid, now: DateTime<Utc>) -> Result<Step, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let row: Option<CampaignRow> = sqlx::query_as(&format!(
            "SELECT {COLUMNS} FROM campaigns
             WHERE id = $1 AND state IN ('scheduled', 'running')
             FOR UPDATE SKIP LOCKED"
        ))
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Ok(Step::Idle);
        };
        let step = step(&mut tx, &row, now).await?;
        tx.commit().await?;
        match &step {
            Step::Queued(peers) => {
                tracing::info!(campaign = %id, commands = peers.len(), "campaign commands queued");
                if let Some(commands) = &self.commands {
                    peers.iter().for_each(|peer| commands.dispatch(peer));
                }
            }
            Step::Completed => tracing::info!(campaign = %id, "campaign completed"),
            Step::Failed => tracing::warn!(campaign = %id, "campaign failed"),
            Step::Idle => {}
        }
        Ok(step)
    }

    /// Runs [`Self::tick`] every `poll` and on [`Self::wake`] until
    /// `shutdown` resolves.
    pub async fn run(self, poll: Duration, shutdown: impl Future<Output = ()>) {
        let mut interval = tokio::time::interval(poll);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                _ = &mut shutdown => break,
                _ = interval.tick() => {}
                _ = self.wake.notified() => {}
            }
            self.tick(Utc::now()).await;
        }
    }
}

/// Servers of one wave by outcome.
#[derive(Debug, sqlx::FromRow)]
struct Tally {
    size: i64,
    failed: i64,
    done: i64,
}

async fn step(
    conn: &mut PgConnection,
    row: &CampaignRow,
    now: DateTime<Utc>,
) -> Result<Step, sqlx::Error> {
    let next = match row.current_wave {
        None => 0,
        Some(wave) => {
            let tally: Tally = sqlx::query_as(
                "SELECT count(*) AS size,
                     count(*) FILTER (WHERE t.error IS NOT NULL
                                      OR c.state IN ('failed', 'expired', 'cancelled')) AS failed,
                     count(*) FILTER (WHERE t.error IS NOT NULL
                                      OR c.state IN ('succeeded', 'failed', 'expired', 'cancelled')) AS done
                 FROM campaign_targets t LEFT JOIN commands c ON c.id = t.command_id
                 WHERE t.campaign_id = $1 AND t.wave = $2",
            )
            .bind(row.id)
            .bind(wave)
            .fetch_one(&mut *conn)
            .await?;
            if row
                .threshold()
                .exceeded(tally.failed as usize, tally.size as usize)
            {
                fail(conn, row, wave, &tally).await?;
                return Ok(Step::Failed);
            }
            if tally.done < tally.size {
                return queue(conn, row, wave, now).await;
            }
            finish_wave(conn, row.id, wave, &tally).await?;
            wave + 1
        }
    };
    let waves: i64 =
        sqlx::query_scalar("SELECT count(*) FROM campaign_waves WHERE campaign_id = $1")
            .bind(row.id)
            .fetch_one(&mut *conn)
            .await?;
    if i64::from(next) >= waves {
        complete(conn, row.id).await?;
        return Ok(Step::Completed);
    }
    let step = queue(conn, row, next, now).await?;
    if matches!(step, Step::Queued(_)) {
        sqlx::query("UPDATE campaigns SET state = 'running', current_wave = $2 WHERE id = $1")
            .bind(row.id)
            .bind(next)
            .execute(&mut *conn)
            .await?;
    }
    Ok(step)
}

/// Queues a command for every server of `wave` without one, if a window
/// is open long enough; servers revoked since creation are skipped.
async fn queue(
    conn: &mut PgConnection,
    row: &CampaignRow,
    wave: i32,
    now: DateTime<Utc>,
) -> Result<Step, sqlx::Error> {
    let missing: Vec<(Uuid, String, bool)> = sqlx::query_as(
        "SELECT t.server_id, s.peer_id, s.revoked_at IS NOT NULL
         FROM campaign_targets t JOIN servers s ON s.id = t.server_id
         WHERE t.campaign_id = $1 AND t.wave = $2 AND t.command_id IS NULL AND t.error IS NULL
         ORDER BY s.hostname, s.peer_id",
    )
    .bind(row.id)
    .bind(wave)
    .fetch_all(&mut *conn)
    .await?;
    if missing.is_empty() {
        return Ok(Step::Idle);
    }
    let (open, end): (bool, Option<DateTime<Utc>>) = sqlx::query_as(
        "SELECT e IS NOT NULL, CASE WHEN e = 'infinity' THEN NULL ELSE e END
         FROM campaign_window_end($1, $2) e",
    )
    .bind(row.id)
    .bind(now)
    .fetch_one(&mut *conn)
    .await?;
    let mut ttl = Duration::from_secs(row.command_ttl_s.max(1) as u64);
    if let Some(end) = end {
        let left = (end - now).to_std().unwrap_or_default();
        ttl = ttl.min(left);
    }
    if !open || ttl < MIN_WINDOW_LEFT {
        return Ok(Step::Idle);
    }
    let payload = row.spec().payload();
    let mut peers = Vec::new();
    let mut skipped = 0;
    for (server_id, peer_id, revoked) in missing {
        if revoked {
            sqlx::query(
                "UPDATE campaign_targets SET error = 'server revoked'
                 WHERE campaign_id = $1 AND server_id = $2",
            )
            .bind(row.id)
            .bind(server_id)
            .execute(&mut *conn)
            .await?;
            skipped += 1;
            continue;
        }
        let new = NewCommand {
            server_id,
            kind: APPLY_UPDATES.into(),
            payload: payload.clone(),
            ttl,
            requested_by: None,
            campaign_id: Some(row.id),
        };
        let command = CommandStore::enqueue_in(&mut *conn, &new).await?;
        sqlx::query(
            "UPDATE campaign_targets SET command_id = $3 WHERE campaign_id = $1 AND server_id = $2",
        )
        .bind(row.id)
        .bind(server_id)
        .bind(command.id)
        .execute(&mut *conn)
        .await?;
        peers.push(peer_id);
    }
    sqlx::query(
        "UPDATE campaign_waves SET started_at = COALESCE(started_at, now())
         WHERE campaign_id = $1 AND position = $2",
    )
    .bind(row.id)
    .bind(wave)
    .execute(&mut *conn)
    .await?;
    record(
        conn,
        row.id,
        "campaign.queue",
        json!({
            "wave": wave,
            "commands": peers.len(),
            "skipped": skipped,
            "ttl_s": ttl.as_secs(),
        }),
    )
    .await?;
    Ok(Step::Queued(peers))
}

async fn finish_wave(
    conn: &mut PgConnection,
    campaign: Uuid,
    wave: i32,
    tally: &Tally,
) -> Result<(), sqlx::Error> {
    let finished = sqlx::query(
        "UPDATE campaign_waves SET finished_at = now()
         WHERE campaign_id = $1 AND position = $2 AND finished_at IS NULL",
    )
    .bind(campaign)
    .bind(wave)
    .execute(&mut *conn)
    .await?;
    if finished.rows_affected() == 1 {
        let details = json!({ "wave": wave, "servers": tally.size, "failed": tally.failed });
        record(conn, campaign, "campaign.wave_finish", details).await?;
    }
    Ok(())
}

async fn complete(conn: &mut PgConnection, campaign: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE campaigns SET state = 'completed', finished_at = now(),
             started_at = COALESCE(started_at, now())
         WHERE id = $1",
    )
    .bind(campaign)
    .execute(&mut *conn)
    .await?;
    record(conn, campaign, "campaign.complete", json!({})).await
}

/// Ends the campaign: nothing more is queued, unsent commands are
/// cancelled, sent ones finish.
async fn fail(
    conn: &mut PgConnection,
    row: &CampaignRow,
    wave: i32,
    tally: &Tally,
) -> Result<(), sqlx::Error> {
    let error = format!(
        "wave {wave}: {} of {} servers failed",
        tally.failed, tally.size
    );
    close_waves(conn, row.id).await?;
    let cancelled = cancel_queued(conn, row.id, "campaign failed", false).await?;
    sqlx::query(
        "UPDATE campaigns SET state = 'failed', finished_at = now(), error = $2 WHERE id = $1",
    )
    .bind(row.id)
    .bind(&error)
    .execute(&mut *conn)
    .await?;
    let details = json!({
        "wave": wave,
        "servers": tally.size,
        "failed": tally.failed,
        "threshold": row.threshold(),
        "cancelled_commands": cancelled.len(),
    });
    record(conn, row.id, "campaign.fail", details).await
}

async fn record(
    conn: &mut PgConnection,
    campaign: Uuid,
    action: &str,
    details: serde_json::Value,
) -> Result<(), sqlx::Error> {
    audit::record(
        conn,
        Event {
            actor_kind: ACTOR.0,
            actor_id: ACTOR.1,
            action,
            target: Some(("campaign", &campaign.to_string())),
            details,
        },
    )
    .await
}
