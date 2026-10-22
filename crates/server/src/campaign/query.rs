//! Read-side queries for the API: campaigns with their customer, windows,
//! per-wave counts and per-server status (ADR-0033).

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use super::model::{CampaignState, Window};
use super::store::{CampaignRow, WindowRow, COLUMNS};
use crate::commands::State as CommandState;

/// What `GET /api/campaigns` filters on.
#[derive(Debug, Clone)]
pub struct ListFilter {
    /// Customers the user reads, as from `Scope::bind`.
    pub customers: (bool, Vec<Uuid>),
    pub customer_id: Option<Uuid>,
    pub state: Option<CampaignState>,
    /// `(created_at, id)` of the last campaign of the previous page.
    pub before: Option<(DateTime<Utc>, Uuid)>,
    pub limit: i64,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Listed {
    #[sqlx(flatten)]
    pub campaign: CampaignRow,
    pub customer_name: String,
}

/// Servers of one wave by status.
#[derive(Debug, Clone, Default, PartialEq, Eq, sqlx::FromRow)]
pub struct WaveCounts {
    pub campaign_id: Uuid,
    pub wave: i32,
    pub total: i64,
    pub pending: i64,
    pub in_progress: i64,
    pub succeeded: i64,
    pub failed: i64,
    pub cancelled: i64,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct WaveRow {
    pub position: i32,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// One server of a campaign and its current command.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TargetRow {
    pub wave: i32,
    pub server_id: Uuid,
    pub peer_id: String,
    pub hostname: String,
    pub site_id: Uuid,
    pub command_id: Option<Uuid>,
    pub command_state: Option<CommandState>,
    pub exit_code: Option<i32>,
    /// The target's error, else its command's.
    pub error: Option<String>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl TargetRow {
    /// `pending`, the command's state, or `skipped`.
    pub fn status(&self) -> &'static str {
        match (self.command_state, &self.error) {
            (Some(state), _) => state.as_str(),
            (None, Some(_)) => "skipped",
            (None, None) => "pending",
        }
    }
}

pub async fn list(pool: &PgPool, filter: &ListFilter) -> Result<Vec<Listed>, sqlx::Error> {
    let (before_at, before_id) = filter.before.unzip();
    sqlx::query_as(&format!(
        "SELECT c.*, cu.name AS customer_name
         FROM (SELECT {COLUMNS} FROM campaigns) c JOIN customers cu ON cu.id = c.customer_id
         WHERE ($1 OR c.customer_id = ANY($2))
           AND ($3::uuid IS NULL OR c.customer_id = $3)
           AND ($4::text IS NULL OR c.state = $4)
           AND ($5::timestamptz IS NULL OR (c.created_at, c.id) < ($5, $6))
         ORDER BY c.created_at DESC, c.id DESC
         LIMIT $7"
    ))
    .bind(filter.customers.0)
    .bind(&filter.customers.1)
    .bind(filter.customer_id)
    .bind(filter.state)
    .bind(before_at)
    .bind(before_id)
    .bind(filter.limit)
    .fetch_all(pool)
    .await
}

pub async fn get(pool: &PgPool, id: Uuid) -> Result<Option<Listed>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT c.*, cu.name AS customer_name
         FROM (SELECT {COLUMNS} FROM campaigns WHERE id = $1) c
         JOIN customers cu ON cu.id = c.customer_id"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Windows of `campaigns`, each campaign's in the order given.
pub async fn windows(
    pool: &PgPool,
    campaigns: &[Uuid],
) -> Result<Vec<(Uuid, Window)>, sqlx::Error> {
    let rows: Vec<WindowWithCampaign> = sqlx::query_as(
        "SELECT campaign_id, kind, weekdays, start_minute, end_minute, timezone, starts_at, ends_at
         FROM campaign_windows WHERE campaign_id = ANY($1) ORDER BY campaign_id, position",
    )
    .bind(campaigns)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|w| Some((w.campaign_id, w.window.window()?)))
        .collect())
}

#[derive(sqlx::FromRow)]
struct WindowWithCampaign {
    campaign_id: Uuid,
    #[sqlx(flatten)]
    window: WindowRow,
}

/// Counts per wave of `campaigns`. `failed` counts failed and expired
/// commands and skipped servers.
pub async fn counts(pool: &PgPool, campaigns: &[Uuid]) -> Result<Vec<WaveCounts>, sqlx::Error> {
    sqlx::query_as(
        "SELECT t.campaign_id, t.wave, count(*) AS total,
             count(*) FILTER (WHERE t.command_id IS NULL AND t.error IS NULL) AS pending,
             count(*) FILTER (WHERE c.state IN ('queued', 'sent', 'running')) AS in_progress,
             count(*) FILTER (WHERE c.state = 'succeeded') AS succeeded,
             count(*) FILTER (WHERE (t.command_id IS NULL AND t.error IS NOT NULL)
                              OR c.state IN ('failed', 'expired')) AS failed,
             count(*) FILTER (WHERE c.state = 'cancelled') AS cancelled
         FROM campaign_targets t LEFT JOIN commands c ON c.id = t.command_id
         WHERE t.campaign_id = ANY($1)
         GROUP BY t.campaign_id, t.wave
         ORDER BY t.campaign_id, t.wave",
    )
    .bind(campaigns)
    .fetch_all(pool)
    .await
}

pub async fn waves(pool: &PgPool, campaign: Uuid) -> Result<Vec<WaveRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT position, started_at, finished_at FROM campaign_waves
         WHERE campaign_id = $1 ORDER BY position",
    )
    .bind(campaign)
    .fetch_all(pool)
    .await
}

/// The campaign's servers by wave and hostname.
pub async fn targets(pool: &PgPool, campaign: Uuid) -> Result<Vec<TargetRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT t.wave, t.server_id, s.peer_id, s.hostname, s.site_id, t.command_id,
             c.state AS command_state, c.exit_code, COALESCE(t.error, c.error) AS error,
             c.finished_at
         FROM campaign_targets t
         JOIN servers s ON s.id = t.server_id
         LEFT JOIN commands c ON c.id = t.command_id
         WHERE t.campaign_id = $1
         ORDER BY t.wave, s.hostname, s.peer_id",
    )
    .bind(campaign)
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_status_prefers_the_command() {
        let mut row = TargetRow {
            wave: 0,
            server_id: Uuid::nil(),
            peer_id: "agent-1".into(),
            hostname: "web-1".into(),
            site_id: Uuid::nil(),
            command_id: None,
            command_state: None,
            exit_code: None,
            error: None,
            finished_at: None,
        };
        assert_eq!(row.status(), "pending");
        row.error = Some("server revoked".into());
        assert_eq!(row.status(), "skipped");
        row.command_state = Some(CommandState::Failed);
        assert_eq!(row.status(), "failed");
    }
}
