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
//! In a campaign pinned to snapshots, each server's sources are pointed
//! at them before its update and restored after it ([`super::pin`],
//! ADR-0038); restores are queued for campaigns in any state.
//!
//! A step only reads `commands` and the campaign's tables, so running it
//! twice, or again after a restart, queues nothing twice.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use monitorflow_common::command::{APPLY_UPDATES, CONFIGURE_SOURCES};
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use tokio::sync::Notify;
use uuid::Uuid;

use super::model::SourcesMode;
use super::pin::{self, Configurer, Next, Pending, PinSources};
use super::store::{cancel_queued, close_waves, Actor, CampaignRow, COLUMNS};
use crate::api::audit::{self, Event};
use crate::commands::{CommandStore, Commands, NewCommand};
use crate::mirror::{relays, Mirrors};

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
    sources: Option<PinSources>,
    wake: Arc<Notify>,
}

impl Runner {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            commands: None,
            sources: None,
            wake: Arc::new(Notify::new()),
        }
    }

    /// Wakes `commands`' dispatcher for agents that got a command.
    pub fn with_commands(mut self, commands: Commands) -> Self {
        self.commands = Some(commands);
        self
    }

    /// Builds `configure_sources` commands for pinned campaigns with the
    /// signing key and layout of `mirrors` and `public_url`, and keeps
    /// the `.pinned` files of its mirrors (ADR-0038).
    pub fn with_sources(mut self, public_url: Option<String>, mirrors: Mirrors) -> Self {
        self.sources = Some(PinSources {
            public_url,
            mirrors,
        });
        self
    }

    /// Asks [`Self::run`] for a step of every campaign now, as after a
    /// start or resume.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// One step of every active campaign, and of every campaign with
    /// sources to restore, oldest first, at `now`. Failures are logged;
    /// the campaign is retried at the next tick.
    pub async fn tick(&self, now: DateTime<Utc>) {
        if let Some(sources) = &self.sources {
            if let Err(error) = pin::write_pins(&self.pool, sources.mirrors.layout()).await {
                tracing::warn!(%error, "cannot record the snapshots campaigns pin; will retry");
            }
        }
        let ids: Vec<Uuid> = match sqlx::query_scalar(
            "SELECT id FROM campaigns
             WHERE state IN ('scheduled', 'running')
                OR id IN (SELECT campaign_id FROM campaign_targets
                          WHERE sources_command_id IS NOT NULL AND restore_command_id IS NULL)
             ORDER BY created_at",
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

    /// One step of the campaign `id` at `now`. Restores of pinned sources
    /// are queued whatever its state, and not reported in the step.
    pub async fn advance(&self, id: Uuid, now: DateTime<Utc>) -> Result<Step, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let row: Option<CampaignRow> = sqlx::query_as(&format!(
            "SELECT {COLUMNS} FROM campaigns
             WHERE id = $1 AND (state IN ('scheduled', 'running') OR sources_mode IS NOT NULL)
             FOR UPDATE SKIP LOCKED"
        ))
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Ok(Step::Idle);
        };
        let mut restored = Vec::new();
        if row.sources_mode.is_some() {
            let open = row.state.is_active() && window_ttl(&mut tx, &row, now).await?.is_some();
            restored = pin::restore(&mut tx, &row, open).await?;
        }
        let step = if row.state.is_active() {
            step(&mut tx, &row, now, self.sources.as_ref()).await?
        } else {
            Step::Idle
        };
        tx.commit().await?;
        if !restored.is_empty() {
            tracing::info!(campaign = %id, commands = restored.len(), "campaign sources restores queued");
            if let Some(commands) = &self.commands {
                restored.iter().for_each(|peer| commands.dispatch(peer));
            }
        }
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
    sources: Option<&PinSources>,
) -> Result<Step, sqlx::Error> {
    let next = match row.current_wave {
        None => 0,
        Some(wave) => {
            // With pinned sources, a server is done once restored too,
            // and failed if that failed after its update.
            let tally: Tally = sqlx::query_as(
                "SELECT count(*) AS size,
                     count(*) FILTER (WHERE t.error IS NOT NULL
                                      OR c.state IN ('failed', 'expired', 'cancelled')
                                      OR (c.id IS NOT NULL
                                          AND r.state IN ('failed', 'expired', 'cancelled'))) AS failed,
                     count(*) FILTER (WHERE (t.error IS NOT NULL
                                             OR c.state IN ('succeeded', 'failed', 'expired', 'cancelled'))
                                        AND (cfg.sent_at IS NULL OR s.revoked_at IS NOT NULL
                                             OR r.state IN ('succeeded', 'failed', 'expired', 'cancelled'))) AS done
                 FROM campaign_targets t
                 JOIN servers s ON s.id = t.server_id
                 LEFT JOIN commands c ON c.id = t.command_id
                 LEFT JOIN commands cfg ON cfg.id = t.sources_command_id
                 LEFT JOIN commands r ON r.id = t.restore_command_id
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
                return queue(conn, row, wave, now, sources).await;
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
    let step = queue(conn, row, next, now, sources).await?;
    if matches!(step, Step::Queued(_)) {
        sqlx::query("UPDATE campaigns SET state = 'running', current_wave = $2 WHERE id = $1")
            .bind(row.id)
            .bind(next)
            .execute(&mut *conn)
            .await?;
    }
    Ok(step)
}

/// The TTL of a command queued at `now`: the campaign's, capped at the
/// end of the open window; `None` when no window stays open for
/// [`MIN_WINDOW_LEFT`].
async fn window_ttl(
    conn: &mut PgConnection,
    row: &CampaignRow,
    now: DateTime<Utc>,
) -> Result<Option<Duration>, sqlx::Error> {
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
    Ok((open && ttl >= MIN_WINDOW_LEFT).then_some(ttl))
}

/// Queues a command for every server of `wave` without one, if a window
/// is open long enough; servers revoked since creation are skipped. In a
/// pinned campaign that command is first `configure_sources`, then the
/// update once it succeeded.
async fn queue(
    conn: &mut PgConnection,
    row: &CampaignRow,
    wave: i32,
    now: DateTime<Utc>,
    sources: Option<&PinSources>,
) -> Result<Step, sqlx::Error> {
    let missing: Vec<Pending> = sqlx::query_as(
        "SELECT t.server_id, s.peer_id, s.revoked_at IS NOT NULL AS revoked,
             s.os_family, cfg.state AS configure_state, cfg.error AS configure_error,
             t.restore_command_id, r.state AS restore_state
         FROM campaign_targets t JOIN servers s ON s.id = t.server_id
         LEFT JOIN commands cfg ON cfg.id = t.sources_command_id
         LEFT JOIN commands r ON r.id = t.restore_command_id
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
    let Some(ttl) = window_ttl(conn, row, now).await? else {
        return Ok(Step::Idle);
    };
    let payload = row.spec().payload();
    let mut configurer = None;
    let mut peers = Vec::new();
    let (mut skipped, mut configured) = (0, 0);
    for target in missing {
        let next = match row.sources_mode {
            _ if target.revoked => Next::Fail("server revoked".into()),
            None => Next::Update,
            // Windows servers take no sources (ADR-0049).
            Some(_) if target.os_family == "windows" => Next::Update,
            Some(_) => target.next(),
        };
        let built = match next {
            Next::Wait => continue,
            Next::Fail(error) => Err(error),
            Next::Update => Ok((APPLY_UPDATES, payload.clone())),
            Next::Configure | Next::Restart => {
                let configurer = match configurer {
                    Some(ref c) => c,
                    None => {
                        let mode = row.sources_mode.unwrap_or(SourcesMode::Replace);
                        configurer.insert(Configurer::new(conn, row, mode, sources).await?)
                    }
                };
                let relay = relays::for_server(&mut *conn, target.server_id).await?;
                configurer
                    .payload(relay.as_ref(), &target.os_family)
                    .map(|spec| (CONFIGURE_SOURCES, spec))
                    .map_err(|error| format!("cannot configure sources: {error}"))
            }
        };
        let (kind, payload) = match built {
            Ok(command) => command,
            Err(error) => {
                sqlx::query(
                    "UPDATE campaign_targets SET error = $3
                     WHERE campaign_id = $1 AND server_id = $2",
                )
                .bind(row.id)
                .bind(target.server_id)
                .bind(error)
                .execute(&mut *conn)
                .await?;
                skipped += 1;
                continue;
            }
        };
        let new = NewCommand {
            server_id: target.server_id,
            kind: kind.into(),
            payload,
            ttl,
            requested_by: None,
            campaign_id: Some(row.id),
        };
        let command = CommandStore::enqueue_in(&mut *conn, &new).await?;
        let column = if kind == CONFIGURE_SOURCES {
            configured += 1;
            "sources_command_id = $3, restore_command_id = NULL"
        } else {
            "command_id = $3"
        };
        sqlx::query(&format!(
            "UPDATE campaign_targets SET {column} WHERE campaign_id = $1 AND server_id = $2"
        ))
        .bind(row.id)
        .bind(target.server_id)
        .bind(command.id)
        .execute(&mut *conn)
        .await?;
        peers.push(target.peer_id);
    }
    if peers.is_empty() && skipped == 0 {
        return Ok(Step::Idle);
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
            "configure_sources": configured,
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

pub(super) async fn record(
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
