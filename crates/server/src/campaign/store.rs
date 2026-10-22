//! Campaigns in PostgreSQL. Every change of a campaign happens in a
//! transaction holding its row lock (`SELECT … FOR UPDATE`), so user
//! actions and the runner never interleave, and each one is audited in
//! that transaction.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::model::{
    plan_waves, Action, CampaignState, FailureThreshold, FailureUnit, LocalTime, UpdateSpec,
    WaveSpec, Window, MAX_TARGETS, MAX_WINDOWS,
};
use crate::api::audit::{self, Event};

const COLUMNS: &str = "id, customer_id, name, site_id, packages, security_only, allow_reboot,
     command_ttl_s, failure_threshold, failure_threshold_unit, state, current_wave, error,
     created_by, created_at, started_at, finished_at";

/// Who acts, as `(actor_kind, actor_id)` of the audit log.
pub type Actor<'a> = (&'a str, &'a str);

/// A campaign to create. Targets are `server_ids` and the unrevoked
/// servers of `site_id`, all of `customer_id`.
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
}

impl CampaignRow {
    pub fn spec(&self) -> UpdateSpec {
        UpdateSpec {
            packages: self.packages.clone(),
            security_only: self.security_only,
            allow_reboot: self.allow_reboot,
        }
    }

    pub fn threshold(&self) -> FailureThreshold {
        FailureThreshold {
            value: self.failure_threshold.max(0) as u32,
            unit: self.failure_threshold_unit,
        }
    }
}

#[derive(sqlx::FromRow)]
struct WindowRow {
    kind: String,
    weekdays: Option<Vec<i16>>,
    start_minute: Option<i16>,
    end_minute: Option<i16>,
    timezone: Option<String>,
    starts_at: Option<DateTime<Utc>>,
    ends_at: Option<DateTime<Utc>>,
}

impl WindowRow {
    fn window(self) -> Option<Window> {
        match self.kind.as_str() {
            "weekly" => Some(Window::Weekly {
                weekdays: self.weekdays?.into_iter().map(|d| d as u8).collect(),
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

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Validates `new`, freezes its targets into waves and stores it as a
    /// draft, audited as `campaign.create`.
    pub async fn create(
        &self,
        new: &NewCampaign,
        actor: Actor<'_>,
    ) -> Result<CampaignRow, CampaignError> {
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
        let mut tx = self.pool.begin().await?;
        check_timezones(&mut tx, &windows).await?;
        let targets = targets(&mut tx, new).await?;
        let waves =
            plan_waves(&targets, &new.waves).map_err(|e| CampaignError::Invalid(e.to_string()))?;

        let row: CampaignRow = sqlx::query_as(&format!(
            "INSERT INTO campaigns (customer_id, name, site_id, packages, security_only,
                 allow_reboot, command_ttl_s, failure_threshold, failure_threshold_unit, created_by)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
             RETURNING {COLUMNS}"
        ))
        .bind(new.customer_id)
        .bind(&new.name)
        .bind(new.site_id)
        .bind(&new.spec.packages)
        .bind(new.spec.security_only)
        .bind(new.spec.allow_reboot)
        .bind(i32::try_from(new.command_ttl.as_secs()).unwrap_or(i32::MAX))
        .bind(new.failure_threshold.value as i32)
        .bind(new.failure_threshold.unit)
        .bind(new.created_by)
        .fetch_one(&mut *tx)
        .await?;
        let (servers, positions): (Vec<Uuid>, Vec<i32>) = waves
            .iter()
            .enumerate()
            .flat_map(|(i, wave)| wave.iter().map(move |s| (*s, i as i32)))
            .unzip();
        sqlx::query(
            "INSERT INTO campaign_waves (campaign_id, position)
             SELECT $1, generate_series(0, $2 - 1)",
        )
        .bind(row.id)
        .bind(waves.len() as i32)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO campaign_targets (campaign_id, server_id, wave)
             SELECT $1, * FROM unnest($2::uuid[], $3::integer[])",
        )
        .bind(row.id)
        .bind(&servers)
        .bind(&positions)
        .execute(&mut *tx)
        .await?;
        for (position, window) in windows.iter().enumerate() {
            insert_window(&mut tx, row.id, position as i32, window).await?;
        }
        audit::record(
            &mut *tx,
            Event {
                actor_kind: actor.0,
                actor_id: actor.1,
                action: "campaign.create",
                target: Some(("campaign", &row.id.to_string())),
                details: json!({
                    "name": row.name,
                    "customer_id": row.customer_id,
                    "site_id": row.site_id,
                    "servers": servers.len(),
                    "waves": waves.iter().map(Vec::len).collect::<Vec<_>>(),
                    "spec": new.spec,
                    "command_ttl_s": row.command_ttl_s,
                    "failure_threshold": new.failure_threshold,
                    "windows": windows,
                }),
            },
        )
        .await?;
        tx.commit().await?;
        Ok(row)
    }

    pub async fn get(&self, id: Uuid) -> Result<Option<CampaignRow>, sqlx::Error> {
        sqlx::query_as(&format!("SELECT {COLUMNS} FROM campaigns WHERE id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await
    }

    /// The campaign's windows, in the order they were given.
    pub async fn windows(&self, id: Uuid) -> Result<Vec<Window>, sqlx::Error> {
        let rows: Vec<WindowRow> = sqlx::query_as(
            "SELECT kind, weekdays, start_minute, end_minute, timezone, starts_at, ends_at
             FROM campaign_windows WHERE campaign_id = $1 ORDER BY position",
        )
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
        let row: CampaignRow = sqlx::query_as(&format!(
            "UPDATE campaigns SET state = $2,
                 started_at = COALESCE(started_at, now()),
                 finished_at = CASE WHEN $3 THEN now() END,
                 error = CASE WHEN $3 THEN $4 ELSE error END
             WHERE id = $1
             RETURNING {COLUMNS}"
        ))
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

/// The campaign `id`, locked until the end of the transaction.
pub(super) async fn lock(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<Option<CampaignRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM campaigns WHERE id = $1 FOR UPDATE"
    ))
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
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "UPDATE commands SET state = 'cancelled', finished_at = now(), error = $2
         WHERE campaign_id = $1 AND state = 'queued'
         RETURNING id",
    )
    .bind(campaign)
    .bind(reason)
    .fetch_all(&mut *conn)
    .await?;
    if detach && !ids.is_empty() {
        sqlx::query("UPDATE campaign_targets SET command_id = NULL WHERE command_id = ANY($1)")
            .bind(&ids)
            .execute(&mut *conn)
            .await?;
    }
    Ok(ids)
}

async fn check_timezones(conn: &mut PgConnection, windows: &[Window]) -> Result<(), CampaignError> {
    let zones: Vec<&str> = windows
        .iter()
        .filter_map(|w| match w {
            Window::Weekly { timezone, .. } => Some(timezone.as_str()),
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
/// customer's and unrevoked, and the unrevoked servers of its site.
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
         ORDER BY s.hostname, s.peer_id",
    )
    .bind(new.customer_id)
    .bind(&new.server_ids)
    .bind(new.site_id)
    .fetch_all(&mut *conn)
    .await?;
    if targets.is_empty() {
        return Err(CampaignError::Invalid(
            "the campaign targets no server".into(),
        ));
    }
    if targets.len() > MAX_TARGETS {
        return Err(CampaignError::Invalid(format!(
            "at most {MAX_TARGETS} servers"
        )));
    }
    Ok(targets)
}

async fn insert_window(
    conn: &mut PgConnection,
    campaign: Uuid,
    position: i32,
    window: &Window,
) -> Result<(), sqlx::Error> {
    let query = sqlx::query(
        "INSERT INTO campaign_windows (campaign_id, position, kind, weekdays, start_minute,
             end_minute, timezone, starts_at, ends_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(campaign)
    .bind(position);
    let query = match window {
        Window::Weekly {
            weekdays,
            start,
            end,
            timezone,
        } => query
            .bind("weekly")
            .bind(weekdays.iter().map(|&d| i16::from(d)).collect::<Vec<_>>())
            .bind(start.0 as i16)
            .bind(end.0 as i16)
            .bind(timezone)
            .bind(None::<DateTime<Utc>>)
            .bind(None::<DateTime<Utc>>),
        Window::Once { starts_at, ends_at } => query
            .bind("once")
            .bind(None::<Vec<i16>>)
            .bind(None::<i16>)
            .bind(None::<i16>)
            .bind(None::<String>)
            .bind(starts_at)
            .bind(ends_at),
    };
    query.execute(conn).await?;
    Ok(())
}
