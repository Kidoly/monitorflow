//! Campaigns in PostgreSQL. Every change of a campaign happens in a
//! transaction holding its row lock (`SELECT … FOR UPDATE`), so user
//! actions and the runner never interleave, and each one is audited in
//! that transaction.

use std::time::Duration;

use chrono::{DateTime, Utc};
use monitorflow_common::command;
use monitorflow_common::third_party;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::model::{
    plan_waves, Action, CampaignState, FailureThreshold, FailureUnit, LocalTime, PinnedMirror,
    SourcesMode, SourcesPin, ThirdPartySpec, UpdateSpec, WaveSpec, Window, MAX_TARGETS,
    MAX_WINDOWS,
};
use crate::api::audit::{self, Event};
use crate::api::commands::unfit_message;
use crate::commands::sources::os_family;
use crate::mirror::Family;
use monitorflow_common::release::Version;

pub(super) const COLUMNS: &str =
    "id, customer_id, name, site_id, packages, security_only, allow_reboot,
     command_ttl_s, failure_threshold, failure_threshold_unit, state, current_wave, error,
     created_by, created_at, started_at, finished_at, sources_mode, agent_version, include_os,
     third_party, requested_server_ids, requested_waves";

/// Who acts, as `(actor_kind, actor_id)` of the audit log.
pub type Actor<'a> = (&'a str, &'a str);

/// A campaign to create. Targets are `server_ids` and the unrevoked
/// servers of `site_id`, all of `customer_id`, without those that cannot
/// take package updates unless it updates agents (ADR-0063).
#[derive(Debug, Clone)]
pub struct NewCampaign {
    pub customer_id: Uuid,
    pub name: String,
    pub site_id: Option<Uuid>,
    pub server_ids: Vec<Uuid>,
    pub waves: Vec<WaveSpec>,
    pub spec: UpdateSpec,
    pub command_ttl: Duration,
    pub failure_threshold: FailureThreshold,
    pub windows: Vec<Window>,
    /// Snapshots to pin the servers to during their update (ADR-0038).
    pub sources: Option<SourcesPin>,
    /// Update the agents to this version instead of packages (ADR-0053).
    pub agent_version: Option<Version>,
    pub created_by: Option<Uuid>,
}

#[derive(Debug, thiserror::Error)]
pub enum CampaignError {
    /// The request is invalid; the message says why and is safe to show.
    #[error("{0}")]
    Invalid(String),
    #[error("campaign not found")]
    NotFound,
    #[error("cannot {action} a campaign that is {state}")]
    Transition {
        action: &'static str,
        state: CampaignState,
    },
    /// The campaign's state allows the action, what it holds does not;
    /// the message says why and is safe to show.
    #[error("{0}")]
    Busy(String),
    #[error("database query failed: {0}")]
    Database(#[from] sqlx::Error),
}

/// A row of `campaigns`.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct CampaignRow {
    pub id: Uuid,
    pub customer_id: Uuid,
    pub name: String,
    pub site_id: Option<Uuid>,
    pub packages: Vec<String>,
    pub security_only: bool,
    pub allow_reboot: bool,
    pub command_ttl_s: i32,
    pub failure_threshold: i32,
    pub failure_threshold_unit: FailureUnit,
    pub state: CampaignState,
    pub current_wave: Option<i32>,
    pub error: Option<String>,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    /// Set when the campaign pins snapshots (ADR-0038).
    pub sources_mode: Option<SourcesMode>,
    /// Set for an agent update campaign (ADR-0053).
    pub agent_version: Option<String>,
    /// Whether OS updates are included (ADR-0138).
    pub include_os: bool,
    pub third_party: sqlx::types::Json<Vec<ThirdPartySpec>>,
    /// The `server_ids` and `waves` asked; `None` before ADR-0179.
    pub requested_server_ids: Option<Vec<Uuid>>,
    pub requested_waves: Option<sqlx::types::Json<Vec<WaveSpec>>>,
}

impl CampaignRow {
    pub fn spec(&self) -> UpdateSpec {
        UpdateSpec {
            packages: self.packages.clone(),
            security_only: self.security_only,
            allow_reboot: self.allow_reboot,
            include_os: self.include_os,
            third_party: self.third_party.0.clone(),
        }
    }

    pub fn threshold(&self) -> FailureThreshold {
        FailureThreshold {
            value: self.failure_threshold.max(0) as u32,
            unit: self.failure_threshold_unit,
        }
    }
}

/// The columns of a [`WindowRow`].
pub(super) const WINDOW_COLUMNS: &str = "kind, weekdays, start_minute, end_minute, timezone, \
     starts_at, ends_at, month_day, month_week, months";

#[derive(sqlx::FromRow)]
pub(super) struct WindowRow {
    kind: String,
    month_day: Option<i16>,
    month_week: Option<i16>,
    months: Option<Vec<i16>>,
    weekdays: Option<Vec<i16>>,
    start_minute: Option<i16>,
    end_minute: Option<i16>,
    timezone: Option<String>,
    starts_at: Option<DateTime<Utc>>,
    ends_at: Option<DateTime<Utc>>,
}

impl WindowRow {
    pub(super) fn window(self) -> Option<Window> {
        match self.kind.as_str() {
            "weekly" => Some(Window::Weekly {
                weekdays: self.weekdays?.into_iter().map(|d| d as u8).collect(),
                start: LocalTime(self.start_minute? as u16),
                end: LocalTime(self.end_minute? as u16),
                timezone: self.timezone?,
            }),
            "monthly" => Some(Window::Monthly {
                month_day: self.month_day.map(|d| d as u8),
                month_week: self.month_week.map(|w| w as u8),
                weekday: self.weekdays.and_then(|d| d.first().map(|&d| d as u8)),
                months: self.months?.into_iter().map(|m| m as u8).collect(),
                start: LocalTime(self.start_minute? as u16),
                end: LocalTime(self.end_minute? as u16),
                timezone: self.timezone?,
            }),
            "once" => Some(Window::Once {
                starts_at: self.starts_at?,
                ends_at: self.ends_at?,
            }),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CampaignStore {
    pool: PgPool,
}

impl CampaignStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Validates `new`, freezes its targets into waves and stores it as a
    /// draft, audited as `campaign.create`.
    pub async fn create(
        &self,
        new: &NewCampaign,
        actor: Actor<'_>,
    ) -> Result<CampaignRow, CampaignError> {
        let windows = check(new)?;
        let mut tx = self.pool.begin().await?;
        let plan = plan(&mut tx, new, windows).await?;
        // SQL-safe: interpolates only the constant COLUMNS.
        let row: CampaignRow = bind_fields(
            sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "INSERT INTO campaigns (customer_id, name, site_id, packages, security_only,
                     allow_reboot, command_ttl_s, failure_threshold, failure_threshold_unit,
                     sources_mode, agent_version, include_os, third_party, requested_server_ids,
                     requested_waves, created_by)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)
                 RETURNING {COLUMNS}"
            ))),
            new,
        )
        .bind(new.created_by)
        .fetch_one(&mut *tx)
        .await?;
        write_plan(&mut tx, row.id, new, &plan).await?;
        audit::record(
            &mut *tx,
            Event {
                actor_kind: actor.0,
                actor_id: actor.1,
                action: "campaign.create",
                target: Some(("campaign", &row.id.to_string())),
                details: details(&row, new, &plan),
            },
        )
        .await?;
        tx.commit().await?;
        Ok(row)
    }

    /// Edits the campaign `id` to `new`, audited as `campaign.update`
    /// (ADR-0179, ADR-0173). What changes depends on where it is:
    ///
    /// - before its first batch is queued, everything but its state and
    ///   who made it;
    /// - while a run is under way (a batch was queued and it is not
    ///   final), only its name, windows and failure threshold: the rest is
    ///   what its batches are running, and a change to it is refused;
    /// - once finished, everything, for its next run: its last run moves
    ///   to the history and it becomes a draft again with new batches. A
    ///   change of its name alone renames it and leaves it as it is.
    pub async fn update(
        &self,
        id: Uuid,
        new: &NewCampaign,
        actor: Actor<'_>,
    ) -> Result<CampaignRow, CampaignError> {
        let windows = check(new)?;
        let mut tx = self.pool.begin().await?;
        let row = lock(&mut tx, id).await?.ok_or(CampaignError::NotFound)?;
        let started = started(&mut tx, &row).await?;
        if row.state.is_final() {
            settled(&mut tx, id, "edit").await?;
            let current = request_of(&mut tx, &row).await?;
            if locked_changes(&current, new, true).is_empty()
                && current.windows == windows
                && current.failure_threshold == new.failure_threshold
            {
                let row = rename(&mut tx, &row, &new.name, actor).await?;
                tx.commit().await?;
                return Ok(row);
            }
            let run = match started {
                true => Some(archive(&mut tx, &row, &current).await?),
                false => None,
            };
            replace(&mut tx, id, new, windows, actor, run).await?;
            // SQL-safe: interpolates only the constant COLUMNS.
            let row: CampaignRow = sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "UPDATE campaigns SET state = 'draft', current_wave = NULL, error = NULL,
                     started_at = NULL, finished_at = NULL
                 WHERE id = $1
                 RETURNING {COLUMNS}"
            )))
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(row);
        }
        if started {
            let current = request_of(&mut tx, &row).await?;
            let locked = locked_changes(&current, new, row.requested_server_ids.is_some());
            if !locked.is_empty() {
                return Err(CampaignError::Busy(format!(
                    "cannot change {} while the campaign runs: only its name, windows and \
                     failure threshold change now; stop it, or wait for it to end, then edit it \
                     for its next run",
                    locked.join(", ")
                )));
            }
            let row = edit_running(&mut tx, &row, new, windows, actor).await?;
            tx.commit().await?;
            return Ok(row);
        }
        let row = replace(&mut tx, id, new, windows, actor, None).await?;
        tx.commit().await?;
        Ok(row)
    }

    /// Runs the finished campaign `id` again, audited as
    /// `campaign.relaunch` (ADR-0173): its last run moves to the history,
    /// its targets are planned again from its settings, or only the
    /// servers that failed with `failed_only`, in one batch, and it is
    /// `scheduled` until a window opens.
    pub async fn relaunch(
        &self,
        id: Uuid,
        failed_only: bool,
        actor: Actor<'_>,
    ) -> Result<CampaignRow, CampaignError> {
        let mut tx = self.pool.begin().await?;
        let row = lock(&mut tx, id).await?.ok_or(CampaignError::NotFound)?;
        if !row.state.is_final() {
            return Err(CampaignError::Transition {
                action: "relaunch",
                state: row.state,
            });
        }
        settled(&mut tx, id, "relaunch").await?;
        let from = row.state;
        let current = request_of(&mut tx, &row).await?;
        let mut next = current.clone();
        if failed_only {
            let failed = failed_servers(&mut tx, id).await?;
            if failed.is_empty() {
                return Err(CampaignError::Busy(
                    "no server of the campaign failed in its last run".into(),
                ));
            }
            next.site_id = None;
            next.server_ids = failed;
            next.waves = Vec::new();
        }
        let windows = check(&next)?;
        let run = match started(&mut tx, &row).await? {
            true => Some(archive(&mut tx, &row, &current).await?),
            false => None,
        };
        clear_plan(&mut tx, id).await?;
        let plan = plan(&mut tx, &next, windows).await?;
        write_plan(&mut tx, id, &next, &plan).await?;
        // SQL-safe: interpolates only the constant COLUMNS.
        let row: CampaignRow = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "UPDATE campaigns SET state = 'scheduled', current_wave = NULL, error = NULL,
                 started_at = now(), finished_at = NULL
             WHERE id = $1
             RETURNING {COLUMNS}"
        )))
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        audit::record(
            &mut *tx,
            Event {
                actor_kind: actor.0,
                actor_id: actor.1,
                action: "campaign.relaunch",
                target: Some(("campaign", &id.to_string())),
                details: json!({
                    "from": from.as_str(),
                    "archived_run": run,
                    "failed_only": failed_only,
                    "servers": plan.waves.iter().map(Vec::len).sum::<usize>(),
                    "waves": plan.waves.iter().map(Vec::len).collect::<Vec<_>>(),
                }),
            },
        )
        .await?;
        tx.commit().await?;
        Ok(row)
    }

    /// Deletes a draft or a finished campaign with its batches, servers
    /// and windows, audited as `campaign.delete`; its commands stay in the
    /// history. Refused while some of them are still in flight or a
    /// server's package sources are still to be put back (ADR-0179).
    pub async fn delete(&self, id: Uuid, actor: Actor<'_>) -> Result<CampaignRow, CampaignError> {
        let mut tx = self.pool.begin().await?;
        let row = lock(&mut tx, id).await?.ok_or(CampaignError::NotFound)?;
        if !(row.state == CampaignState::Draft || row.state.is_final()) {
            return Err(CampaignError::Transition {
                action: "delete",
                state: row.state,
            });
        }
        settled(&mut tx, id, "delete").await?;
        let servers: i64 =
            sqlx::query_scalar("SELECT count(*) FROM campaign_targets WHERE campaign_id = $1")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
        sqlx::query("DELETE FROM campaigns WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        audit::record(
            &mut *tx,
            Event {
                actor_kind: actor.0,
                actor_id: actor.1,
                action: "campaign.delete",
                target: Some(("campaign", &id.to_string())),
                details: json!({
                    "name": row.name,
                    "customer_id": row.customer_id,
                    "state": row.state.as_str(),
                    "servers": servers,
                    "created_at": row.created_at,
                    "started_at": row.started_at,
                    "finished_at": row.finished_at,
                }),
            },
        )
        .await?;
        tx.commit().await?;
        Ok(row)
    }

    pub async fn get(&self, id: Uuid) -> Result<Option<CampaignRow>, sqlx::Error> {
        // SQL-safe: interpolates only the constant COLUMNS.
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM campaigns WHERE id = $1"
        )))
        .bind(id)
        .fetch_optional(&self.pool)
        .await
    }

    /// The campaign's windows, in the order they were given.
    pub async fn windows(&self, id: Uuid) -> Result<Vec<Window>, sqlx::Error> {
        // SQL-safe: interpolates only the constant WINDOW_COLUMNS.
        let rows: Vec<WindowRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {WINDOW_COLUMNS}
             FROM campaign_windows WHERE campaign_id = $1 ORDER BY position"
        )))
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().filter_map(WindowRow::window).collect())
    }

    /// Applies `action`, audited as `campaign.<action>`. Pausing and
    /// stopping cancel the campaign's commands not sent yet; those sent
    /// finish. After a pause their servers get a new command on resume.
    pub async fn act(
        &self,
        id: Uuid,
        action: Action,
        actor: Actor<'_>,
    ) -> Result<CampaignRow, CampaignError> {
        let mut tx = self.pool.begin().await?;
        let row = lock(&mut tx, id).await?.ok_or(CampaignError::NotFound)?;
        let from = row.state;
        let next = action.next(row.state, row.current_wave.is_some()).ok_or(
            CampaignError::Transition {
                action: action.as_str(),
                state: row.state,
            },
        )?;
        let cancelled = match action {
            Action::Pause => cancel_queued(&mut tx, id, "campaign paused", true).await?,
            Action::Stop => {
                close_waves(&mut tx, id).await?;
                cancel_queued(&mut tx, id, "campaign stopped", false).await?
            }
            Action::Start | Action::Resume => Vec::new(),
        };
        // SQL-safe: interpolates only the constant COLUMNS.
        let row: CampaignRow = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "UPDATE campaigns SET state = $2,
                 started_at = COALESCE(started_at, now()),
                 finished_at = CASE WHEN $3 THEN now() END,
                 error = CASE WHEN $3 THEN $4 ELSE error END
             WHERE id = $1
             RETURNING {COLUMNS}"
        )))
        .bind(id)
        .bind(next)
        .bind(next.is_final())
        .bind(format!("stopped by {}", actor.0))
        .fetch_one(&mut *tx)
        .await?;
        audit::record(
            &mut *tx,
            Event {
                actor_kind: actor.0,
                actor_id: actor.1,
                action: &format!("campaign.{}", action.as_str()),
                target: Some(("campaign", &id.to_string())),
                details: json!({
                    "from": from.as_str(),
                    "state": next.as_str(),
                    "cancelled_commands": cancelled.len(),
                }),
            },
        )
        .await?;
        tx.commit().await?;
        Ok(row)
    }
}

/// What a campaign freezes from its request, checked.
struct Plan {
    windows: Vec<Window>,
    waves: Vec<Vec<Uuid>>,
    /// `(mirror_id, snapshot_id)` of each pinned mirror.
    pinned: Vec<(Uuid, Uuid)>,
}

/// The checks of `new` that need no database; its windows, normalized.
fn check(new: &NewCampaign) -> Result<Vec<Window>, CampaignError> {
    new.spec.validate().map_err(CampaignError::Invalid)?;
    new.failure_threshold
        .validate()
        .map_err(CampaignError::Invalid)?;
    if new.windows.len() > MAX_WINDOWS {
        return Err(CampaignError::Invalid(format!(
            "at most {MAX_WINDOWS} windows"
        )));
    }
    let mut windows = new.windows.clone();
    for window in &mut windows {
        window.validate().map_err(CampaignError::Invalid)?;
    }
    if let Some(pin) = &new.sources {
        pin.validate().map_err(CampaignError::Invalid)?;
    }
    let spec = &new.spec;
    let package_options = !spec.packages.is_empty()
        || spec.security_only
        || spec.allow_reboot
        || !spec.include_os
        || spec.has_third_party()
        || new.sources.is_some();
    if new.agent_version.is_some() && package_options {
        return Err(CampaignError::Invalid(
            "an agent update campaign takes no packages, security_only, allow_reboot, \
             include_os, third_party or sources"
                .into(),
        ));
    }
    if !spec.include_os && new.sources.is_some() {
        return Err(CampaignError::Invalid(
            "pinned sources are for OS updates, which this campaign leaves out".into(),
        ));
    }
    Ok(windows)
}

/// Resolves the targets of `new` into waves and its pinned snapshots.
async fn plan(
    tx: &mut PgConnection,
    new: &NewCampaign,
    windows: Vec<Window>,
) -> Result<Plan, CampaignError> {
    check_timezones(&mut *tx, &windows).await?;
    let targets = targets(&mut *tx, new).await?;
    let waves =
        plan_waves(&targets, &new.waves).map_err(|e| CampaignError::Invalid(e.to_string()))?;
    check_names(&mut *tx, &new.spec.packages, &targets).await?;
    let pinned = match &new.sources {
        Some(pin) => pinned_snapshots(&mut *tx, pin, &targets).await?,
        None => Vec::new(),
    };
    if !pinned.is_empty() {
        check_pin_limit(&mut *tx, new.customer_id, &pinned).await?;
    }
    Ok(Plan {
        windows,
        waves,
        pinned,
    })
}

type RowQuery<'q> =
    sqlx::query::QueryAs<'q, sqlx::Postgres, CampaignRow, sqlx::postgres::PgArguments>;

/// Binds `$1` to `$15` of an insert or update of `campaigns` from `new`.
fn bind_fields<'q>(query: RowQuery<'q>, new: &'q NewCampaign) -> RowQuery<'q> {
    query
        .bind(new.customer_id)
        .bind(&new.name)
        .bind(new.site_id)
        .bind(&new.spec.packages)
        .bind(new.spec.security_only)
        .bind(new.spec.allow_reboot)
        .bind(i32::try_from(new.command_ttl.as_secs()).unwrap_or(i32::MAX))
        .bind(new.failure_threshold.value as i32)
        .bind(new.failure_threshold.unit)
        .bind(new.sources.as_ref().map(|p| p.mode))
        .bind(new.agent_version.as_ref().map(ToString::to_string))
        .bind(new.spec.include_os)
        .bind(sqlx::types::Json(&new.spec.third_party))
        .bind(&new.server_ids)
        .bind(sqlx::types::Json(&new.waves))
}

/// Stores the pins, waves, targets and windows of the campaign `id`.
async fn write_plan(
    tx: &mut PgConnection,
    id: Uuid,
    new: &NewCampaign,
    plan: &Plan,
) -> Result<(), sqlx::Error> {
    let (mirror_ids, snapshot_ids): (Vec<Uuid>, Vec<Uuid>) = plan.pinned.iter().copied().unzip();
    let (mirror_names, snapshot_names): (Vec<&str>, Vec<&str>) = new
        .sources
        .iter()
        .flat_map(|pin| &pin.mirrors)
        .map(|m| (m.name.as_str(), m.snapshot.as_str()))
        .unzip();
    sqlx::query(
        "INSERT INTO campaign_snapshots
             (campaign_id, mirror_id, snapshot_id, mirror_name, snapshot_name)
         SELECT $1, * FROM unnest($2::uuid[], $3::uuid[], $4::text[], $5::text[])",
    )
    .bind(id)
    .bind(&mirror_ids)
    .bind(&snapshot_ids)
    .bind(&mirror_names)
    .bind(&snapshot_names)
    .execute(&mut *tx)
    .await?;
    let (servers, positions): (Vec<Uuid>, Vec<i32>) = plan
        .waves
        .iter()
        .enumerate()
        .flat_map(|(i, wave)| wave.iter().map(move |s| (*s, i as i32)))
        .unzip();
    sqlx::query(
        "INSERT INTO campaign_waves (campaign_id, position)
         SELECT $1, generate_series(0, $2 - 1)",
    )
    .bind(id)
    .bind(plan.waves.len() as i32)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO campaign_targets (campaign_id, server_id, wave)
         SELECT $1, * FROM unnest($2::uuid[], $3::integer[])",
    )
    .bind(id)
    .bind(&servers)
    .bind(&positions)
    .execute(&mut *tx)
    .await?;
    for (position, window) in plan.windows.iter().enumerate() {
        insert_window(&mut *tx, id, position as i32, window).await?;
    }
    Ok(())
}

/// The audit details of a created or edited campaign.
fn details(row: &CampaignRow, new: &NewCampaign, plan: &Plan) -> serde_json::Value {
    json!({
        "name": row.name,
        "customer_id": row.customer_id,
        "site_id": row.site_id,
        "servers": plan.waves.iter().map(Vec::len).sum::<usize>(),
        "waves": plan.waves.iter().map(Vec::len).collect::<Vec<_>>(),
        "spec": new.spec,
        "command_ttl_s": row.command_ttl_s,
        "failure_threshold": new.failure_threshold,
        "windows": plan.windows,
        "sources": new.sources,
    })
}

/// The settings a campaign ran with, kept with each past run (ADR-0173):
/// its request as `PUT /api/campaigns/{id}` takes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = CampaignRunSettings)]
pub struct RunSettings {
    pub name: String,
    pub site_id: Option<Uuid>,
    pub server_ids: Vec<Uuid>,
    pub wave_specs: Vec<WaveSpec>,
    pub spec: UpdateSpec,
    /// Set for an agent update campaign (ADR-0053).
    pub agent_version: Option<String>,
    pub command_ttl_s: u64,
    pub failure_threshold: FailureThreshold,
    pub windows: Vec<Window>,
    pub sources: Option<SourcesPin>,
}

impl From<&NewCampaign> for RunSettings {
    fn from(new: &NewCampaign) -> Self {
        Self {
            name: new.name.clone(),
            site_id: new.site_id,
            server_ids: new.server_ids.clone(),
            wave_specs: new.waves.clone(),
            spec: new.spec.clone(),
            agent_version: new.agent_version.as_ref().map(ToString::to_string),
            command_ttl_s: new.command_ttl.as_secs(),
            failure_threshold: new.failure_threshold,
            windows: new.windows.clone(),
            sources: new.sources.clone(),
        }
    }
}

/// Whether the campaign's run queued anything: a batch started, or a
/// server got a command or an error.
async fn started(conn: &mut PgConnection, row: &CampaignRow) -> Result<bool, sqlx::Error> {
    if row.current_wave.is_some() {
        return Ok(true);
    }
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM campaign_targets WHERE campaign_id = $1
                        AND (command_id IS NOT NULL OR sources_command_id IS NOT NULL
                             OR snapshot_command_id IS NOT NULL OR error IS NOT NULL))",
    )
    .bind(row.id)
    .fetch_one(conn)
    .await
}

/// Refuses to `verb` a finished campaign while some of its commands are
/// in flight or a server's package sources are still to be put back, the
/// condition the runner keeps restoring under (ADR-0179).
async fn settled(conn: &mut PgConnection, id: Uuid, verb: &str) -> Result<(), CampaignError> {
    let (in_flight, unrestored): (bool, bool) = sqlx::query_as(
        "SELECT
             EXISTS (SELECT 1 FROM commands
                     WHERE campaign_id = $1 AND state IN ('queued', 'sent', 'running')),
             EXISTS (SELECT 1 FROM campaign_targets t
                     JOIN commands cfg ON cfg.id = t.sources_command_id
                     JOIN servers s ON s.id = t.server_id
                     WHERE t.campaign_id = $1 AND t.restore_command_id IS NULL
                       AND s.revoked_at IS NULL
                       AND (cfg.sent_at IS NOT NULL OR cfg.state = 'queued'))",
    )
    .bind(id)
    .fetch_one(conn)
    .await?;
    if in_flight {
        return Err(CampaignError::Busy(format!(
            "the campaign still has commands in flight: {verb} it once they end"
        )));
    }
    if unrestored {
        return Err(CampaignError::Busy(
            "the package sources of some servers are not restored yet".into(),
        ));
    }
    Ok(())
}

/// The request the campaign `row` holds now. For a campaign made before
/// its request was kept (ADR-0179): its site, the other servers of its
/// batches still there, and the size of each batch but the last, as the
/// web interface reads it.
async fn request_of(
    conn: &mut PgConnection,
    row: &CampaignRow,
) -> Result<NewCampaign, sqlx::Error> {
    let (server_ids, waves) = match (&row.requested_server_ids, &row.requested_waves) {
        (Some(ids), Some(waves)) => (ids.clone(), waves.0.clone()),
        _ => {
            let frozen: Vec<(Uuid, i32, bool)> = sqlx::query_as(
                "SELECT t.server_id, t.wave,
                     s.revoked_at IS NULL AND s.site_id IS DISTINCT FROM $2
                 FROM campaign_targets t JOIN servers s ON s.id = t.server_id
                 WHERE t.campaign_id = $1 ORDER BY t.wave",
            )
            .bind(row.id)
            .bind(row.site_id)
            .fetch_all(&mut *conn)
            .await?;
            let ids = frozen.iter().filter(|t| t.2).map(|t| t.0).collect();
            let last = frozen.last().map_or(0, |t| t.1);
            let sizes = (0..last)
                .map(|w| WaveSpec::Size(frozen.iter().filter(|t| t.1 == w).count() as u32))
                .collect();
            (ids, sizes)
        }
    };
    // SQL-safe: interpolates only the constant WINDOW_COLUMNS.
    let windows: Vec<WindowRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {WINDOW_COLUMNS}
         FROM campaign_windows WHERE campaign_id = $1 ORDER BY position"
    )))
    .bind(row.id)
    .fetch_all(&mut *conn)
    .await?;
    let mirrors: Vec<(String, String)> = sqlx::query_as(
        "SELECT mirror_name, snapshot_name FROM campaign_snapshots
         WHERE campaign_id = $1 ORDER BY mirror_name",
    )
    .bind(row.id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(NewCampaign {
        customer_id: row.customer_id,
        name: row.name.clone(),
        site_id: row.site_id,
        server_ids,
        waves,
        spec: row.spec(),
        command_ttl: Duration::from_secs(row.command_ttl_s.max(0) as u64),
        failure_threshold: row.threshold(),
        windows: windows.into_iter().filter_map(WindowRow::window).collect(),
        sources: row.sources_mode.map(|mode| SourcesPin {
            mode,
            mirrors: mirrors
                .into_iter()
                .map(|(name, snapshot)| PinnedMirror { name, snapshot })
                .collect(),
        }),
        agent_version: row.agent_version.as_deref().and_then(|v| v.parse().ok()),
        created_by: row.created_by,
    })
}

/// What `new` changes of `current` that a run under way holds fixed, in
/// words for the error; its targets only with `targets`.
fn locked_changes(current: &NewCampaign, new: &NewCampaign, targets: bool) -> Vec<&'static str> {
    let mut changed = Vec::new();
    if new.customer_id != current.customer_id {
        changed.push("the customer");
    }
    if targets
        && (new.site_id != current.site_id
            || new.server_ids != current.server_ids
            || new.waves != current.waves)
    {
        changed.push("the servers or batches");
    }
    if new.spec != current.spec || new.agent_version != current.agent_version {
        changed.push("the update");
    }
    if new.sources != current.sources {
        changed.push("the pinned snapshots");
    }
    if new.command_ttl != current.command_ttl {
        changed.push("how long a command may wait");
    }
    changed
}

/// Renames the campaign `row` and nothing else.
async fn rename(
    conn: &mut PgConnection,
    row: &CampaignRow,
    name: &str,
    actor: Actor<'_>,
) -> Result<CampaignRow, CampaignError> {
    // SQL-safe: interpolates only the constant COLUMNS.
    let renamed: CampaignRow = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "UPDATE campaigns SET name = $2 WHERE id = $1 RETURNING {COLUMNS}"
    )))
    .bind(row.id)
    .bind(name)
    .fetch_one(&mut *conn)
    .await?;
    audit::record(
        &mut *conn,
        Event {
            actor_kind: actor.0,
            actor_id: actor.1,
            action: "campaign.update",
            target: Some(("campaign", &row.id.to_string())),
            details: json!({
                "name": name,
                "previous_name": row.name,
                "state": row.state.as_str(),
            }),
        },
    )
    .await?;
    Ok(renamed)
}

/// Changes the name, windows and failure threshold of the campaign `row`
/// under way; the rest of `new` was checked to be unchanged.
async fn edit_running(
    conn: &mut PgConnection,
    row: &CampaignRow,
    new: &NewCampaign,
    windows: Vec<Window>,
    actor: Actor<'_>,
) -> Result<CampaignRow, CampaignError> {
    check_timezones(&mut *conn, &windows).await?;
    sqlx::query("DELETE FROM campaign_windows WHERE campaign_id = $1")
        .bind(row.id)
        .execute(&mut *conn)
        .await?;
    for (position, window) in windows.iter().enumerate() {
        insert_window(&mut *conn, row.id, position as i32, window).await?;
    }
    // SQL-safe: interpolates only the constant COLUMNS.
    let edited: CampaignRow = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "UPDATE campaigns SET name = $2, failure_threshold = $3, failure_threshold_unit = $4
         WHERE id = $1 RETURNING {COLUMNS}"
    )))
    .bind(row.id)
    .bind(&new.name)
    .bind(new.failure_threshold.value as i32)
    .bind(new.failure_threshold.unit)
    .fetch_one(&mut *conn)
    .await?;
    audit::record(
        &mut *conn,
        Event {
            actor_kind: actor.0,
            actor_id: actor.1,
            action: "campaign.update",
            target: Some(("campaign", &row.id.to_string())),
            details: json!({
                "name": new.name,
                "state": row.state.as_str(),
                "failure_threshold": new.failure_threshold,
                "windows": windows,
            }),
        },
    )
    .await?;
    Ok(edited)
}

/// Deletes the batches, targets, windows and pins of the campaign `id`,
/// before planning it again, so its own pins do not count twice.
async fn clear_plan(conn: &mut PgConnection, id: Uuid) -> Result<(), sqlx::Error> {
    for table in [
        "campaign_targets",
        "campaign_waves",
        "campaign_windows",
        "campaign_snapshots",
    ] {
        // SQL-safe: interpolates only the constant table names above.
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM {table} WHERE campaign_id = $1"
        )))
        .bind(id)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Replaces everything of the campaign `id` by `new` but its state and
/// who made it, audited as `campaign.update`; `archived_run` names the
/// run just moved to the history, if any.
async fn replace(
    conn: &mut PgConnection,
    id: Uuid,
    new: &NewCampaign,
    windows: Vec<Window>,
    actor: Actor<'_>,
    archived_run: Option<i32>,
) -> Result<CampaignRow, CampaignError> {
    clear_plan(&mut *conn, id).await?;
    let plan = plan(&mut *conn, new, windows).await?;
    // SQL-safe: interpolates only the constant COLUMNS.
    let row: CampaignRow = bind_fields(
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "UPDATE campaigns SET customer_id = $1, name = $2, site_id = $3, packages = $4,
                 security_only = $5, allow_reboot = $6, command_ttl_s = $7,
                 failure_threshold = $8, failure_threshold_unit = $9, sources_mode = $10,
                 agent_version = $11, include_os = $12, third_party = $13,
                 requested_server_ids = $14, requested_waves = $15
             WHERE id = $16
             RETURNING {COLUMNS}"
        ))),
        new,
    )
    .bind(id)
    .fetch_one(&mut *conn)
    .await?;
    write_plan(&mut *conn, id, new, &plan).await?;
    let mut details = details(&row, new, &plan);
    details["state"] = json!(row.state.as_str());
    if let Some(run) = archived_run {
        details["archived_run"] = json!(run);
    }
    audit::record(
        &mut *conn,
        Event {
            actor_kind: actor.0,
            actor_id: actor.1,
            action: "campaign.update",
            target: Some(("campaign", &id.to_string())),
            details,
        },
    )
    .await?;
    Ok(row)
}

/// Moves the last run of the finished campaign `row`, which ran with
/// `settings`, to the history, and returns its number (ADR-0173). Its
/// batches and servers leave `campaign_waves` and `campaign_targets`.
async fn archive(
    conn: &mut PgConnection,
    row: &CampaignRow,
    settings: &NewCampaign,
) -> Result<i32, sqlx::Error> {
    let number: i32 = sqlx::query_scalar(
        "INSERT INTO campaign_runs
             (campaign_id, number, state, error, started_at, finished_at, settings)
         SELECT $1, COALESCE(max(number), 0) + 1, $2, $3, $4, $5, $6
         FROM campaign_runs WHERE campaign_id = $1
         RETURNING number",
    )
    .bind(row.id)
    .bind(row.state)
    .bind(&row.error)
    .bind(row.started_at)
    .bind(row.finished_at.unwrap_or_else(Utc::now))
    .bind(sqlx::types::Json(RunSettings::from(settings)))
    .fetch_one(&mut *conn)
    .await?;
    sqlx::query(
        "INSERT INTO campaign_run_waves (campaign_id, run, position, started_at, finished_at)
         SELECT campaign_id, $2, position, started_at, finished_at
         FROM campaign_waves WHERE campaign_id = $1",
    )
    .bind(row.id)
    .bind(number)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "INSERT INTO campaign_run_targets (campaign_id, run, server_id, wave, command_id, error,
             sources_command_id, restore_command_id, snapshot_command_id)
         SELECT campaign_id, $2, server_id, wave, command_id, error,
             sources_command_id, restore_command_id, snapshot_command_id
         FROM campaign_targets WHERE campaign_id = $1",
    )
    .bind(row.id)
    .bind(number)
    .execute(&mut *conn)
    .await?;
    sqlx::query("DELETE FROM campaign_waves WHERE campaign_id = $1")
        .bind(row.id)
        .execute(&mut *conn)
        .await?;
    Ok(number)
}

/// The servers of the campaign's run that failed, as `failed` counts
/// them, still there, by hostname.
async fn failed_servers(conn: &mut PgConnection, id: Uuid) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT t.server_id FROM campaign_targets t
         JOIN servers s ON s.id = t.server_id
         LEFT JOIN commands c ON c.id = t.command_id
         LEFT JOIN commands r ON r.id = t.restore_command_id
         WHERE t.campaign_id = $1 AND s.revoked_at IS NULL
           AND (t.error IS NOT NULL
                OR c.state IN ('failed', 'expired', 'cancelled')
                OR (c.state = 'succeeded' AND r.state IN ('failed', 'expired', 'cancelled')))
         ORDER BY s.hostname, s.peer_id",
    )
    .bind(id)
    .fetch_all(conn)
    .await
}

/// The campaign `id`, locked until the end of the transaction.
pub(super) async fn lock(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<Option<CampaignRow>, sqlx::Error> {
    // SQL-safe: interpolates only the constant COLUMNS.
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM campaigns WHERE id = $1 FOR UPDATE"
    )))
    .bind(id)
    .fetch_optional(conn)
    .await
}

/// Marks the campaign's started waves finished; for a campaign that ends.
pub(super) async fn close_waves(
    conn: &mut PgConnection,
    campaign: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE campaign_waves SET finished_at = now()
         WHERE campaign_id = $1 AND started_at IS NOT NULL AND finished_at IS NULL",
    )
    .bind(campaign)
    .execute(conn)
    .await?;
    Ok(())
}

/// Cancels the campaign's commands still queued, with `reason` as their
/// error, and returns their IDs. With `detach`, their targets forget them
/// so the runner queues new ones later.
pub(super) async fn cancel_queued(
    conn: &mut PgConnection,
    campaign: Uuid,
    reason: &str,
    detach: bool,
) -> Result<Vec<Uuid>, sqlx::Error> {
    // Restoring sources is never cancelled: it puts a server back.
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "UPDATE commands SET state = 'cancelled', finished_at = now(), error = $2
         WHERE campaign_id = $1 AND state = 'queued'
           AND id NOT IN (SELECT restore_command_id FROM campaign_targets
                          WHERE campaign_id = $1 AND restore_command_id IS NOT NULL)
         RETURNING id",
    )
    .bind(campaign)
    .bind(reason)
    .fetch_all(&mut *conn)
    .await?;
    if detach && !ids.is_empty() {
        sqlx::query(
            "UPDATE campaign_targets SET
                 command_id = CASE WHEN command_id = ANY($1) THEN NULL ELSE command_id END,
                 sources_command_id = CASE WHEN sources_command_id = ANY($1) THEN NULL
                                           ELSE sources_command_id END,
                 snapshot_command_id = CASE WHEN snapshot_command_id = ANY($1) THEN NULL
                                            ELSE snapshot_command_id END
             WHERE command_id = ANY($1) OR sources_command_id = ANY($1)
                OR snapshot_command_id = ANY($1)",
        )
        .bind(&ids)
        .execute(&mut *conn)
        .await?;
    }
    Ok(ids)
}

/// Named updates must fit every OS family among `targets`: KB numbers or
/// update IDs for Windows, package names for Linux (ADR-0049).
async fn check_names(
    conn: &mut PgConnection,
    packages: &[String],
    targets: &[Uuid],
) -> Result<(), CampaignError> {
    if packages.is_empty() {
        return Ok(());
    }
    let families: Vec<String> =
        sqlx::query_scalar("SELECT DISTINCT os_family FROM servers WHERE id = ANY($1)")
            .bind(targets)
            .fetch_all(conn)
            .await?;
    for family in &families {
        if let Some(bad) = command::unfit_name(family, packages) {
            return Err(CampaignError::Invalid(unfit_message(family, bad)));
        }
    }
    Ok(())
}

/// `(mirror_id, snapshot_id)` of each mirror `pin` names: the snapshot
/// must be published, and every Linux family among `targets` needs a
/// mirror; Windows servers take no sources (ADR-0049).
async fn pinned_snapshots(
    conn: &mut PgConnection,
    pin: &SourcesPin,
    targets: &[Uuid],
) -> Result<Vec<(Uuid, Uuid)>, CampaignError> {
    let mut pinned = Vec::with_capacity(pin.mirrors.len());
    let mut families = Vec::new();
    for m in &pin.mirrors {
        let found: Option<(Uuid, Family, Option<Uuid>)> = sqlx::query_as(
            "SELECT m.id, m.family, s.id FROM mirrors m
             LEFT JOIN mirror_snapshots s
                 ON s.mirror_id = m.id AND s.name = $2 AND s.state = 'published'
             WHERE m.name = $1 AND m.deleting_at IS NULL",
        )
        .bind(&m.name)
        .bind(&m.snapshot)
        .fetch_optional(&mut *conn)
        .await?;
        let invalid = |message: String| CampaignError::Invalid(message);
        let (mirror, family, snapshot) =
            found.ok_or_else(|| invalid(format!("no mirror named `{}`", m.name.escape_debug())))?;
        // The row lock makes retention wait for this campaign, or this
        // campaign see what retention chose (ADR-0074).
        let snapshot = match snapshot {
            Some(id) => sqlx::query_scalar(
                "SELECT id FROM mirror_snapshots WHERE id = $1 AND state = 'published' FOR SHARE",
            )
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?,
            None => None,
        };
        let snapshot: Uuid = snapshot.ok_or_else(|| {
            invalid(format!(
                "mirror `{}` has no published snapshot `{}`",
                m.name,
                m.snapshot.escape_debug()
            ))
        })?;
        pinned.push((mirror, snapshot));
        families.push(os_family(family));
    }
    let uncovered: Option<String> = sqlx::query_scalar(
        "SELECT os_family FROM servers
         WHERE id = ANY($1) AND os_family <> 'windows' AND NOT (os_family = ANY($2))
         LIMIT 1",
    )
    .bind(targets)
    .bind(&families)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(os) = uncovered {
        return Err(CampaignError::Invalid(format!(
            "sources pin no mirror for the campaign's {os} servers"
        )));
    }
    Ok(pinned)
}

/// The most mirror snapshots the unfinished campaigns of one customer pin
/// together. A pin keeps a snapshot from retention and on every relay,
/// whoever's they are, so a customer's drafts must not keep them all.
pub const MAX_PINNED_SNAPSHOTS: usize = 64;

/// Refuses `pinned` when the customer's unfinished campaigns would then
/// pin more than [`MAX_PINNED_SNAPSHOTS`] snapshots. Holds a lock per
/// customer to the end of the transaction, so two campaigns created at
/// once cannot both pass.
async fn check_pin_limit(
    conn: &mut PgConnection,
    customer: Uuid,
    pinned: &[(Uuid, Uuid)],
) -> Result<(), CampaignError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('campaign-pins:' || $1::text))")
        .bind(customer)
        .execute(&mut *conn)
        .await?;
    let snapshots: Vec<Uuid> = pinned.iter().map(|(_, snapshot)| *snapshot).collect();
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM (
             SELECT cs.snapshot_id FROM campaign_snapshots cs
             JOIN campaigns c ON c.id = cs.campaign_id
             WHERE c.customer_id = $1 AND c.state NOT IN ('stopped', 'completed', 'failed')
             UNION SELECT unnest($2::uuid[])
         ) pins",
    )
    .bind(customer)
    .bind(&snapshots)
    .fetch_one(&mut *conn)
    .await?;
    if total > MAX_PINNED_SNAPSHOTS as i64 {
        return Err(CampaignError::Invalid(format!(
            "unfinished campaigns of this customer would pin more than \
             {MAX_PINNED_SNAPSHOTS} mirror snapshots: stop or finish some first"
        )));
    }
    Ok(())
}

async fn check_timezones(conn: &mut PgConnection, windows: &[Window]) -> Result<(), CampaignError> {
    let zones: Vec<&str> = windows
        .iter()
        .filter_map(|w| match w {
            Window::Weekly { timezone, .. } | Window::Monthly { timezone, .. } => {
                Some(timezone.as_str())
            }
            Window::Once { .. } => None,
        })
        .collect();
    if zones.is_empty() {
        return Ok(());
    }
    let unknown: Option<String> = sqlx::query_scalar(
        "SELECT z FROM unnest($1::text[]) z
         WHERE NOT EXISTS (SELECT 1 FROM pg_timezone_names WHERE name = z)
         LIMIT 1",
    )
    .bind(&zones)
    .fetch_optional(conn)
    .await?;
    match unknown {
        Some(zone) => Err(CampaignError::Invalid(format!(
            "unknown time zone `{}`",
            zone.escape_debug()
        ))),
        None => Ok(()),
    }
}

/// The targets of `new`, by hostname: its servers, which must be the
/// customer's and unrevoked, and the unrevoked servers of its site. A
/// package campaign leaves out the servers without a package manager,
/// and one without OS updates those of a platform none of its managers
/// is for (ADR-0138).
async fn targets(conn: &mut PgConnection, new: &NewCampaign) -> Result<Vec<Uuid>, CampaignError> {
    if let Some(site) = new.site_id {
        let found: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM sites WHERE id = $1 AND customer_id = $2")
                .bind(site)
                .bind(new.customer_id)
                .fetch_optional(&mut *conn)
                .await?;
        if found.is_none() {
            return Err(CampaignError::Invalid(format!("unknown site {site}")));
        }
    }
    let named: Vec<(Uuid, bool)> = sqlx::query_as(
        "SELECT s.id, s.revoked_at IS NOT NULL FROM servers s JOIN sites t ON t.id = s.site_id
         WHERE s.id = ANY($1) AND t.customer_id = $2",
    )
    .bind(&new.server_ids)
    .bind(new.customer_id)
    .fetch_all(&mut *conn)
    .await?;
    if let Some(missing) = new
        .server_ids
        .iter()
        .find(|id| !named.iter().any(|(n, _)| n == *id))
    {
        return Err(CampaignError::Invalid(format!("unknown server {missing}")));
    }
    if let Some((revoked, _)) = named.iter().find(|(_, revoked)| *revoked) {
        return Err(CampaignError::Invalid(format!(
            "server {revoked} is revoked"
        )));
    }
    let targets: Vec<Uuid> = sqlx::query_scalar(
        "SELECT s.id FROM servers s JOIN sites t ON t.id = s.site_id
         WHERE t.customer_id = $1 AND s.revoked_at IS NULL
           AND (s.id = ANY($2) OR s.site_id = $3)
           AND (s.package_updates OR $4 OR NOT $5)
           AND ($5 OR (s.os_family = 'windows') = ANY($6))
         ORDER BY s.hostname, s.peer_id",
    )
    .bind(new.customer_id)
    .bind(&new.server_ids)
    .bind(new.site_id)
    .bind(new.agent_version.is_some())
    .bind(new.spec.include_os)
    .bind(platforms(&new.spec))
    .fetch_all(&mut *conn)
    .await?;
    if targets.is_empty() {
        return Err(CampaignError::Invalid(
            "the campaign targets no server that can take it".into(),
        ));
    }
    if targets.len() > MAX_TARGETS {
        return Err(CampaignError::Invalid(format!(
            "at most {MAX_TARGETS} servers"
        )));
    }
    Ok(targets)
}

/// For each selected manager, whether it is a Windows one: the
/// platforms an update without the OS can reach.
fn platforms(spec: &UpdateSpec) -> Vec<bool> {
    spec.managers()
        .into_iter()
        .map(|m| third_party::managers_for("windows").contains(&m))
        .collect()
}

async fn insert_window(
    conn: &mut PgConnection,
    campaign: Uuid,
    position: i32,
    window: &Window,
) -> Result<(), sqlx::Error> {
    let query = sqlx::query(
        "INSERT INTO campaign_windows (campaign_id, position, kind, weekdays, start_minute,
             end_minute, timezone, starts_at, ends_at, month_day, month_week, months)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(campaign)
    .bind(position);
    let small = |values: &[u8]| values.iter().map(|&v| i16::from(v)).collect::<Vec<_>>();
    let query = match window {
        Window::Weekly {
            weekdays,
            start,
            end,
            timezone,
        } => query
            .bind("weekly")
            .bind(small(weekdays))
            .bind(start.0 as i16)
            .bind(end.0 as i16)
            .bind(timezone)
            .bind(None::<DateTime<Utc>>)
            .bind(None::<DateTime<Utc>>)
            .bind(None::<i16>)
            .bind(None::<i16>)
            .bind(None::<Vec<i16>>),
        Window::Monthly {
            month_day,
            month_week,
            weekday,
            months,
            start,
            end,
            timezone,
        } => query
            .bind("monthly")
            .bind(weekday.map(|d| vec![i16::from(d)]))
            .bind(start.0 as i16)
            .bind(end.0 as i16)
            .bind(timezone)
            .bind(None::<DateTime<Utc>>)
            .bind(None::<DateTime<Utc>>)
            .bind(month_day.map(i16::from))
            .bind(month_week.map(i16::from))
            .bind(small(months)),
        Window::Once { starts_at, ends_at } => query
            .bind("once")
            .bind(None::<Vec<i16>>)
            .bind(None::<i16>)
            .bind(None::<i16>)
            .bind(None::<String>)
            .bind(starts_at)
            .bind(ends_at)
            .bind(None::<i16>)
            .bind(None::<i16>)
            .bind(None::<Vec<i16>>),
    };
    query.execute(conn).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new() -> NewCampaign {
        NewCampaign {
            customer_id: Uuid::nil(),
            name: "october".into(),
            site_id: Some(Uuid::nil()),
            server_ids: Vec::new(),
            waves: vec![WaveSpec::Size(1)],
            spec: UpdateSpec::default(),
            command_ttl: Duration::from_secs(3600),
            failure_threshold: FailureThreshold::default(),
            windows: Vec::new(),
            sources: None,
            agent_version: None,
            created_by: None,
        }
    }

    #[test]
    fn a_run_under_way_keeps_what_its_batches_run() {
        let current = new();
        let mut edited = new();
        edited.name = "renamed".into();
        edited.failure_threshold.value = 3;
        assert!(locked_changes(&current, &edited, true).is_empty());
        edited.waves = Vec::new();
        edited.spec.allow_reboot = true;
        edited.command_ttl = Duration::from_secs(60);
        assert_eq!(
            locked_changes(&current, &edited, true),
            [
                "the servers or batches",
                "the update",
                "how long a command may wait"
            ]
        );
        // A campaign made before its request was kept: targets are not compared.
        assert_eq!(
            locked_changes(&current, &edited, false),
            ["the update", "how long a command may wait"]
        );
    }

    #[test]
    fn run_settings_keep_the_request() {
        let mut campaign = new();
        campaign.agent_version = Some("0.3.1".parse().unwrap());
        let settings = RunSettings::from(&campaign);
        assert_eq!(settings.agent_version.as_deref(), Some("0.3.1"));
        assert_eq!(settings.wave_specs, [WaveSpec::Size(1)]);
        assert_eq!(settings.command_ttl_s, 3600);
        let json = serde_json::to_value(&settings).unwrap();
        assert_eq!(
            serde_json::from_value::<RunSettings>(json).unwrap(),
            settings
        );
    }
}
