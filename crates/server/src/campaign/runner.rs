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
//! ADR-0038); restores are queued for campaigns in any state. A package
//! update is preceded by a local snapshot of the server where one is
//! possible and it has no Proxmox VM (ADR-0094).
//!
//! A major upgrade campaign (ADR-0182) sends `major_upgrade` instead,
//! after a Proxmox or local snapshot, to as many servers of a batch at
//! once as it allows; a server is done once it came back on the new
//! release with its checks as they were ([`follow`]).
//!
//! A step only reads `commands` and the campaign's tables, so running it
//! twice, or again after a restart, queues nothing twice.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use monitorflow_common::command::{
    APPLY_UPDATES, CONFIGURE_SOURCES, MAJOR_UPGRADE, SELF_UPDATE, SOFTWARE, TAKE_SNAPSHOT,
};
use monitorflow_common::config::ServerConfig;
use monitorflow_common::proto::v1::TakeSnapshot;
use monitorflow_common::release::Version;
use prost::Message;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use tokio::sync::Notify;
use uuid::Uuid;

use super::model::{software_refusal, MajorUpgradeSpec, SourcesMode};
use super::pin::{self, Configurer, Next, Pending, PinSources};
use super::store::{cancel_queued, close_waves, Actor, CampaignRow, COLUMNS};
use crate::agent_releases;
use crate::api::audit::{self, Event};
use crate::commands::major_upgrade::{self as upgrade, Progress};
use crate::commands::{CommandStore, Commands, NewCommand};
use crate::live_settings::Live;
use crate::mirror::{relays, Mirrors};
use crate::remediation::snapshot::{
    choice, local, SnapshotError, SnapshotRequest, Snapshotter, Support,
};

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
    agents: agent_releases::Sources,
    /// `[proxmox]` is on: a server mapped to a guest is Proxmox's to
    /// snapshot, so it gets no local snapshot (ADR-0094).
    proxmox: bool,
    /// Takes the Proxmox snapshots of major upgrades (ADR-0182).
    snapshotter: Option<Arc<dyn Snapshotter>>,
    wake: Arc<Notify>,
}

impl std::fmt::Debug for dyn Snapshotter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Snapshotter")
    }
}

impl Runner {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            commands: None,
            sources: None,
            agents: agent_releases::Sources::from_config(&ServerConfig::default()),
            proxmox: false,
            snapshotter: None,
            wake: Arc::new(Notify::new()),
        }
    }

    /// Takes the Proxmox snapshots of major upgrade campaigns (ADR-0182).
    pub fn with_snapshotter(mut self, snapshotter: Arc<dyn Snapshotter>) -> Self {
        self.snapshotter = Some(snapshotter);
        self
    }

    /// Where agent update campaigns tell agents to download (ADR-0053).
    pub fn with_agent_releases(mut self, agents: agent_releases::Sources) -> Self {
        self.agents = agents;
        self
    }

    /// Whether `[proxmox]` is on, which leaves its mapped guests without a
    /// local snapshot (ADR-0094).
    pub fn with_proxmox(mut self, enabled: bool) -> Self {
        self.proxmox = enabled;
        self
    }

    /// Wakes `commands`' dispatcher for agents that got a command.
    pub fn with_commands(mut self, commands: Commands) -> Self {
        self.commands = Some(commands);
        self
    }

    /// Builds `configure_sources` commands for pinned campaigns with the
    /// signing key and layout of `mirrors` and `live`'s URL, and keeps
    /// the `.pinned` files of its mirrors (ADR-0038).
    pub fn with_sources(mut self, live: Live, mirrors: Mirrors) -> Self {
        self.sources = Some(PinSources { live, mirrors });
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
        let ids = match self.due().await {
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

    /// The campaigns [`Self::tick`] steps: the active ones, and those with
    /// sources to restore. A configuration that ended without being sent,
    /// as when a campaign stops, or of a removed server, is never restored
    /// ([`pin::restore`]), so it does not keep its campaign in the list.
    pub async fn due(&self) -> Result<Vec<Uuid>, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT id FROM campaigns
             WHERE state IN ('scheduled', 'running')
                OR id IN (SELECT t.campaign_id FROM campaign_targets t
                          JOIN commands cfg ON cfg.id = t.sources_command_id
                          JOIN servers s ON s.id = t.server_id
                          WHERE t.restore_command_id IS NULL AND s.revoked_at IS NULL
                            AND (cfg.sent_at IS NOT NULL OR cfg.state = 'queued'))
             ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await
    }

    /// One step of the campaign `id` at `now`. Restores of pinned sources
    /// are queued whatever its state, and not reported in the step.
    pub async fn advance(&self, id: Uuid, now: DateTime<Utc>) -> Result<Step, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        // SQL-safe: interpolates only the constant COLUMNS.
        let row: Option<CampaignRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM campaigns
             WHERE id = $1 AND (state IN ('scheduled', 'running') OR sources_mode IS NOT NULL)
             FOR UPDATE SKIP LOCKED"
        )))
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
        let mut snapshots = Vec::new();
        let step = if row.state.is_active() {
            if row.major_upgrade.is_some() {
                follow(&mut tx, &row, now).await?;
            }
            let context = Context {
                now,
                sources: self.sources.as_ref(),
                agents: &self.agents,
                proxmox: self.proxmox,
                vm_snapshots: &mut snapshots,
            };
            step(&mut tx, &row, context).await?
        } else {
            Step::Idle
        };
        tx.commit().await?;
        for server in snapshots {
            self.take_vm_snapshot(row.id, server);
        }
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

    /// Takes the Proxmox snapshot of `server` before its major upgrade in
    /// `campaign`, outside the runner's transaction: the API call blocks
    /// for minutes (ADR-0094). Its outcome goes to the server's row, then
    /// the runner looks again.
    fn take_vm_snapshot(&self, campaign: Uuid, server: Uuid) {
        let pool = self.pool.clone();
        let wake = self.wake.clone();
        let snapshotter = self.snapshotter.clone();
        tokio::spawn(async move {
            let (state, error) = match snapshotter {
                None => (
                    "limited",
                    Some("Proxmox snapshots are not set up".to_owned()),
                ),
                Some(snapshotter) => match snapshotter.support(server).await {
                    Ok(Support::Unavailable(reason)) => ("limited", Some(reason.to_string())),
                    Ok(Support::Available(_)) => {
                        match snapshotter
                            .take(server, SnapshotRequest::for_campaign(campaign))
                            .await
                        {
                            Ok(_) => ("taken", None),
                            Err(SnapshotError::Unsupported(reason))
                                if choice::proxmox_decides(reason) =>
                            {
                                ("limited", Some(reason.to_string()))
                            }
                            Err(error) => ("failed", Some(error.to_string())),
                        }
                    }
                    Err(error) => ("failed", Some(error.to_string())),
                },
            };
            let done = sqlx::query(
                "UPDATE campaign_targets SET vm_snapshot_state = $3, vm_snapshot_error = $4
                 WHERE campaign_id = $1 AND server_id = $2 AND vm_snapshot_state = 'taking'",
            )
            .bind(campaign)
            .bind(server)
            .bind(state)
            .bind(error.as_deref())
            .execute(&pool)
            .await;
            if let Err(error) = done {
                tracing::warn!(%campaign, %server, %error, "cannot record a Proxmox snapshot; it is asked again later");
            }
            wake.notify_one();
        });
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

/// What a step needs besides the campaign.
struct Context<'a> {
    now: DateTime<Utc>,
    sources: Option<&'a PinSources>,
    agents: &'a agent_releases::Sources,
    proxmox: bool,
    /// Servers whose Proxmox snapshot to take once committed.
    vm_snapshots: &'a mut Vec<Uuid>,
}

async fn step(
    conn: &mut PgConnection,
    row: &CampaignRow,
    mut context: Context<'_>,
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
                                             OR r.state IN ('succeeded', 'failed', 'expired', 'cancelled'))
                                        AND (c.kind IS DISTINCT FROM 'major_upgrade' OR c.state <> 'succeeded'
                                             OR t.error IS NOT NULL OR t.upgraded_at IS NOT NULL)) AS done
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
                return queue(conn, row, wave, &mut context).await;
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
    let step = queue(conn, row, next, &mut context).await?;
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
/// is open long enough; servers revoked since creation are skipped. A
/// server that lost its package manager since then leaves a package
/// campaign, so it does not count as failed (ADR-0063). In a pinned
/// campaign that command is first `configure_sources`, then the update
/// once it succeeded.
async fn queue(
    conn: &mut PgConnection,
    row: &CampaignRow,
    wave: i32,
    context: &mut Context<'_>,
) -> Result<Step, sqlx::Error> {
    let (now, sources, agents, proxmox) = (
        context.now,
        context.sources,
        context.agents,
        context.proxmox,
    );
    let missing: Vec<Pending> = sqlx::query_as(
        "SELECT t.server_id, s.peer_id, s.revoked_at IS NOT NULL AS revoked,
             s.os_family, s.package_updates, cfg.state AS configure_state, cfg.error AS configure_error,
             t.restore_command_id, r.state AS restore_state,
             t.snapshot_command_id, sn.state AS snapshot_state, sn.error AS snapshot_error,
             $3 AND EXISTS (SELECT 1 FROM server_proxmox_vms v WHERE v.server_id = t.server_id)
                 AS proxmox_mapped,
             st.report AS storage,
             EXISTS (SELECT 1 FROM server_package_managers m WHERE m.server_id = t.server_id)
                 AS reports_third_party,
             t.vm_snapshot_state, t.vm_snapshot_at, t.vm_snapshot_error,
             s.command_kinds->>'software' AS software_state
         FROM campaign_targets t JOIN servers s ON s.id = t.server_id
         LEFT JOIN commands cfg ON cfg.id = t.sources_command_id
         LEFT JOIN commands r ON r.id = t.restore_command_id
         LEFT JOIN commands sn ON sn.id = t.snapshot_command_id
         LEFT JOIN server_storage st ON st.server_id = t.server_id
         WHERE t.campaign_id = $1 AND t.wave = $2 AND t.command_id IS NULL AND t.error IS NULL
         ORDER BY s.hostname, s.peer_id",
    )
    .bind(row.id)
    .bind(wave)
    .bind(proxmox)
    .fetch_all(&mut *conn)
    .await?;
    if missing.is_empty() {
        return Ok(Step::Idle);
    }
    let Some(ttl) = window_ttl(conn, row, now).await? else {
        return Ok(Step::Idle);
    };
    // An agent update campaign sends self_update instead (ADR-0053).
    let agent_version = row.agent_version.as_deref().map(str::parse::<Version>);
    // A major upgrade campaign sends major_upgrade, to as many servers of
    // the batch at once as it allows (ADR-0182).
    let major: Option<&MajorUpgradeSpec> = row.major_upgrade.as_ref().map(|m| &m.0);
    let mut places = match major {
        Some(major) => {
            let busy = upgrading(conn, row.id, wave).await?;
            i64::from(major.parallel).saturating_sub(busy).max(0)
        }
        None => i64::MAX,
    };
    let spec = row.spec();
    let software = row.software();
    let mut configurer = None;
    let mut peers = Vec::new();
    let (mut skipped, mut configured, mut snapshots, mut vm_snapshots) = (0, 0, 0, 0);
    for target in missing {
        if major.is_some() && target.fresh() && !target.revoked {
            if places == 0 {
                continue;
            }
            places -= 1;
        }
        let next = match row.sources_mode {
            _ if target.revoked => Next::Fail("server revoked".into()),
            _ if major.is_some() => {
                target.before_major(major.is_some_and(|m| m.accept_limited_rollback), now)
            }
            _ if !target.package_updates
                && spec.include_os
                && agent_version.is_none()
                && software.is_none()
                && target.configure_state.is_none() =>
            {
                Next::Skip
            }
            None => Next::Update,
            // Windows servers take no sources (ADR-0049).
            Some(_) if target.os_family == "windows" => Next::Update,
            Some(_) => target.next(),
        };
        // A package update may take a local snapshot first (ADR-0094).
        let next = match next {
            Next::Update if agent_version.is_none() => target.before_update(),
            next => next,
        };
        let built = match next {
            Next::Wait => continue,
            Next::VmSnapshot => {
                sqlx::query(
                    "UPDATE campaign_targets
                     SET vm_snapshot_state = 'taking', vm_snapshot_at = $3, vm_snapshot_error = NULL
                     WHERE campaign_id = $1 AND server_id = $2",
                )
                .bind(row.id)
                .bind(target.server_id)
                .bind(now)
                .execute(&mut *conn)
                .await?;
                context.vm_snapshots.push(target.server_id);
                vm_snapshots += 1;
                continue;
            }
            Next::Fail(error) => Err(error),
            Next::Skip => {
                sqlx::query(
                    "DELETE FROM campaign_targets WHERE campaign_id = $1 AND server_id = $2",
                )
                .bind(row.id)
                .bind(target.server_id)
                .execute(&mut *conn)
                .await?;
                skipped += 1;
                continue;
            }
            Next::Snapshot => {
                let name = local::campaign_name(row.id);
                Ok((TAKE_SNAPSHOT, TakeSnapshot { name }.encode_to_vec()))
            }
            Next::Update if major.is_some() => Ok((
                MAJOR_UPGRADE,
                major.map(MajorUpgradeSpec::payload).unwrap_or_default(),
            )),
            Next::Update => match (&agent_version, software) {
                // Software from the catalogue (ADR-0188), where the host
                // allowed it.
                (None, Some(software)) => {
                    match software_refusal(target.software_state.as_deref()) {
                        Some(error) => Err(error.to_owned()),
                        None => Ok((SOFTWARE, software.change(spec.allow_reboot).encode_to_vec())),
                    }
                }
                (None, None) => spec
                    .for_server(
                        &target.os_family,
                        target.reports_third_party,
                        target.software_state.is_some(),
                    )
                    .map(|apply| (APPLY_UPDATES, apply.encode_to_vec())),
                (Some(Ok(version)), _) => {
                    let relay = relays::url_for_server(&mut *conn, target.server_id).await?;
                    Ok((
                        SELF_UPDATE,
                        agents.payload(version, relay.as_deref(), false, 0),
                    ))
                }
                (Some(Err(_)), _) => Err("invalid agent version".to_owned()),
            },
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
            ttl: if kind == TAKE_SNAPSHOT {
                ttl.min(local::TAKE_TTL)
            } else {
                ttl
            },
            requested_by: None,
            campaign_id: Some(row.id),
        };
        let command = CommandStore::enqueue_in(&mut *conn, &new).await?;
        let update = if kind == CONFIGURE_SOURCES {
            configured += 1;
            // Configured again after a restore: a new snapshot too.
            "UPDATE campaign_targets
             SET sources_command_id = $3, restore_command_id = NULL, snapshot_command_id = NULL
             WHERE campaign_id = $1 AND server_id = $2"
        } else if kind == TAKE_SNAPSHOT {
            snapshots += 1;
            "UPDATE campaign_targets SET snapshot_command_id = $3
             WHERE campaign_id = $1 AND server_id = $2"
        } else if kind == MAJOR_UPGRADE {
            // The checks failing now, which it must leave as they are.
            let before = upgrade::failing_checks(&mut *conn, target.server_id).await?;
            sqlx::query(
                "UPDATE campaign_targets SET checks_before = $3
                 WHERE campaign_id = $1 AND server_id = $2",
            )
            .bind(row.id)
            .bind(target.server_id)
            .bind(sqlx::types::Json(before))
            .execute(&mut *conn)
            .await?;
            "UPDATE campaign_targets SET command_id = $3
             WHERE campaign_id = $1 AND server_id = $2"
        } else {
            "UPDATE campaign_targets SET command_id = $3
             WHERE campaign_id = $1 AND server_id = $2"
        };
        sqlx::query(update)
            .bind(row.id)
            .bind(target.server_id)
            .bind(command.id)
            .execute(&mut *conn)
            .await?;
        peers.push(target.peer_id);
    }
    if peers.is_empty() && skipped == 0 && vm_snapshots == 0 {
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
            "take_snapshot": snapshots,
            "proxmox_snapshot": vm_snapshots,
            "skipped": skipped,
            "impact": if major.is_some() { Some("high_impact") } else { None },
            "ttl_s": ttl.as_secs(),
        }),
    )
    .await?;
    Ok(Step::Queued(peers))
}

/// How many servers of the batch are being upgraded now: a snapshot or
/// an upgrade under way, not yet done or failed (ADR-0182).
async fn upgrading(conn: &mut PgConnection, campaign: Uuid, wave: i32) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*) FROM campaign_targets t
         LEFT JOIN commands c ON c.id = t.command_id
         WHERE t.campaign_id = $1 AND t.wave = $2 AND t.error IS NULL AND t.upgraded_at IS NULL
           AND ((t.command_id IS NOT NULL AND c.state NOT IN ('failed', 'expired', 'cancelled'))
                OR (t.command_id IS NULL
                    AND (t.snapshot_command_id IS NOT NULL OR t.vm_snapshot_state IS NOT NULL)))",
    )
    .bind(campaign)
    .bind(wave)
    .fetch_one(conn)
    .await
}

/// A server back on the new release is judged after this long, so its
/// checks had time to run again.
pub const SETTLE: chrono::TimeDelta = chrono::TimeDelta::minutes(5);
/// Its checks must be back within this long of its return.
pub const CHECKS_BACK: chrono::TimeDelta = chrono::TimeDelta::minutes(30);
/// An upgrade not done this long after it started has failed.
pub const UPGRADE_LIMIT: chrono::TimeDelta = chrono::TimeDelta::hours(6);

/// A server whose upgrade started and is not judged yet.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Following {
    pub server_id: Uuid,
    pub command_id: Uuid,
    /// When the command ended: the upgrade started then.
    pub started_at: Option<DateTime<Utc>>,
    pub os_version: Option<String>,
    pub progress: Option<sqlx::types::Json<Progress>>,
    pub checks_before: Option<sqlx::types::Json<Vec<String>>>,
    pub came_back_at: Option<DateTime<Utc>>,
}

/// What [`judge`] makes of a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Wait,
    /// Back on the new release: its checks are given [`SETTLE`].
    CameBack,
    Done,
    Fail(String),
}

/// Whether the server of `f` is done with its upgrade to `to`, with the
/// checks `failing` on it now (read once it came back).
pub fn judge(f: &Following, to: &str, failing: &[String], now: DateTime<Utc>) -> Verdict {
    let command = f.command_id.to_string();
    let progress = f
        .progress
        .as_ref()
        .map(|p| &p.0)
        .filter(|p| p.command_id == command);
    if let Some(progress) = progress {
        if progress.phase == "failed" {
            let mut error = format!(
                "the upgrade failed: {}",
                progress.error.as_deref().unwrap_or("no reason given")
            );
            if progress.sources_restored && !error.contains("put back") {
                error.push_str(" (the sources were put back)");
            }
            return Verdict::Fail(error);
        }
        let on_target = f
            .os_version
            .as_deref()
            .is_some_and(|v| v == to || v.starts_with(&format!("{to}.")));
        if progress.phase == "done" && on_target {
            let Some(back) = f.came_back_at else {
                return Verdict::CameBack;
            };
            let before: &[String] = f.checks_before.as_ref().map_or(&[], |c| &c.0);
            let new: Vec<&String> = failing.iter().filter(|c| !before.contains(c)).collect();
            if new.is_empty() && now - back >= SETTLE {
                return Verdict::Done;
            }
            if !new.is_empty() && now - back >= CHECKS_BACK {
                let names: Vec<&str> = new.iter().map(|c| c.as_str()).collect();
                return Verdict::Fail(format!("checks not back: {}", names.join(", ")));
            }
            return Verdict::Wait;
        }
    }
    if f.started_at.is_some_and(|at| now - at > UPGRADE_LIMIT) {
        return Verdict::Fail(
            "the server did not come back on the new release 6 hours after its upgrade started"
                .into(),
        );
    }
    Verdict::Wait
}

/// Judges every server of a major upgrade campaign whose upgrade started:
/// done, failed with why, or still going (ADR-0182).
async fn follow(
    conn: &mut PgConnection,
    row: &CampaignRow,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let Some(spec) = row.major_upgrade.as_ref().map(|m| &m.0) else {
        return Ok(());
    };
    let rows: Vec<Following> = sqlx::query_as(
        "SELECT t.server_id, c.id AS command_id, c.finished_at AS started_at, s.os_version,
             m.report -> 'progress' AS progress, t.checks_before, t.came_back_at
         FROM campaign_targets t
         JOIN commands c ON c.id = t.command_id
         JOIN servers s ON s.id = t.server_id
         LEFT JOIN server_major_upgrades m ON m.server_id = t.server_id
                                          AND jsonb_typeof(m.report -> 'progress') = 'object'
         WHERE t.campaign_id = $1 AND c.kind = $2 AND c.state = 'succeeded'
           AND t.error IS NULL AND t.upgraded_at IS NULL",
    )
    .bind(row.id)
    .bind(MAJOR_UPGRADE)
    .fetch_all(&mut *conn)
    .await?;
    for f in rows {
        let failing = if f.came_back_at.is_some() {
            upgrade::failing_checks(&mut *conn, f.server_id).await?
        } else {
            Vec::new()
        };
        let verdict = judge(&f, &spec.to, &failing, now);
        let (column, action, error) = match &verdict {
            Verdict::Wait => continue,
            Verdict::CameBack => ("came_back_at", None, None),
            Verdict::Done => ("upgraded_at", Some("campaign.upgrade_done"), None),
            Verdict::Fail(error) => ("error", Some("campaign.upgrade_fail"), Some(error.clone())),
        };
        let update = match column {
            "error" => sqlx::query(
                "UPDATE campaign_targets SET error = $3 WHERE campaign_id = $1 AND server_id = $2",
            )
            .bind(row.id)
            .bind(f.server_id)
            .bind(error.clone()),
            "came_back_at" => sqlx::query(
                "UPDATE campaign_targets SET came_back_at = $3
                 WHERE campaign_id = $1 AND server_id = $2",
            )
            .bind(row.id)
            .bind(f.server_id)
            .bind(now),
            _ => sqlx::query(
                "UPDATE campaign_targets SET upgraded_at = $3
                 WHERE campaign_id = $1 AND server_id = $2",
            )
            .bind(row.id)
            .bind(f.server_id)
            .bind(now),
        };
        update.execute(&mut *conn).await?;
        if let Some(action) = action {
            let details = json!({
                "server_id": f.server_id,
                "command_id": f.command_id,
                "from": spec.from,
                "to": spec.to,
                "error": error,
            });
            record(conn, row.id, action, details).await?;
        }
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn following(phase: Option<&str>, os: &str) -> Following {
        let command_id = Uuid::new_v4();
        Following {
            server_id: Uuid::nil(),
            command_id,
            started_at: Some(Utc::now() - chrono::TimeDelta::minutes(30)),
            os_version: Some(os.into()),
            progress: phase.map(|phase| {
                sqlx::types::Json(Progress {
                    command_id: command_id.to_string(),
                    from: "debian 12".into(),
                    to: "debian 13".into(),
                    phase: phase.into(),
                    error: (phase == "failed")
                        .then(|| "apt-get full-upgrade exited with 100".into()),
                    sources_restored: false,
                    disabled_sources: vec![],
                    report: None,
                    started_at: None,
                    updated_at: None,
                })
            }),
            checks_before: Some(sqlx::types::Json(vec!["disk_used_percent (/srv)".into()])),
            came_back_at: None,
        }
    }

    #[test]
    fn a_server_is_done_once_back_on_the_release_with_its_checks() {
        let now = Utc::now();
        let mut f = following(Some("upgrading"), "debian 12");
        assert_eq!(judge(&f, "debian 13", &[], now), Verdict::Wait);
        f = following(Some("done"), "debian 13");
        assert_eq!(judge(&f, "debian 13", &[], now), Verdict::CameBack);
        f.came_back_at = Some(now - chrono::TimeDelta::minutes(1));
        assert_eq!(
            judge(&f, "debian 13", &[], now),
            Verdict::Wait,
            "checks get time"
        );
        f.came_back_at = Some(now - SETTLE);
        // A check failing before is no reason to fail.
        let before = vec!["disk_used_percent (/srv)".to_owned()];
        assert_eq!(judge(&f, "debian 13", &before, now), Verdict::Done);
        let new = vec!["cpu_percent".to_owned()];
        assert_eq!(judge(&f, "debian 13", &new, now), Verdict::Wait);
        f.came_back_at = Some(now - CHECKS_BACK);
        assert_eq!(
            judge(&f, "debian 13", &new, now),
            Verdict::Fail("checks not back: cpu_percent".into())
        );
    }

    #[test]
    fn a_failure_or_no_news_fails_the_server() {
        let now = Utc::now();
        let f = following(Some("failed"), "debian 12");
        assert_eq!(
            judge(&f, "debian 13", &[], now),
            Verdict::Fail("the upgrade failed: apt-get full-upgrade exited with 100".into())
        );
        // Done, but Hello still says the old release: wait for it.
        let f = following(Some("done"), "debian 12");
        assert_eq!(judge(&f, "debian 13", &[], now), Verdict::Wait);
        let mut late = following(None, "debian 12");
        late.started_at = Some(now - UPGRADE_LIMIT - chrono::TimeDelta::minutes(1));
        assert!(matches!(
            judge(&late, "debian 13", &[], now),
            Verdict::Fail(_)
        ));
        // The progress of another command says nothing about this one.
        let mut other = following(Some("failed"), "debian 12");
        if let Some(p) = other.progress.as_mut() {
            p.0.command_id = "other".into();
        }
        assert_eq!(judge(&other, "debian 13", &[], now), Verdict::Wait);
    }
}
