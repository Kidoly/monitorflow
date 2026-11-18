//! Maintenance windows written as silences (ADR-0151): each occurrence is
//! a row of `silences` with `window_id`, one per covered server or one for
//! the site, written [`AHEAD`] of its start so that it exists before it
//! starts. Everything that reads silences then needs no change.
//!
//! A window is written under a lock on it, and an occurrence its current
//! rule (`updated_at`) wrote already, even one ended early by hand, is not
//! written again, so every central server instance may run [`run`].

use std::time::Duration;

use chrono::{DateTime, NaiveDate, NaiveTime, TimeDelta, Utc};
use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::{Repeat, Schedule};

/// How far ahead occurrences are written.
pub const AHEAD: TimeDelta = TimeDelta::days(2);
/// How often every window is written again, which moves `AHEAD` along.
pub const POLL: Duration = Duration::from_secs(600);

/// A window as stored.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Stored {
    pub id: Uuid,
    pub site_id: Uuid,
    /// The whole site; else [`Stored::servers`], which may be none left.
    pub whole_site: bool,
    pub checks: Option<Value>,
    pub repeat: String,
    pub every: i16,
    pub weekdays: i16,
    pub month_day: Option<i16>,
    pub month_week: Option<i16>,
    pub months: i16,
    pub start_time: NaiveTime,
    pub duration_minutes: i32,
    pub time_zone: String,
    pub starts_on: NaiveDate,
    pub ends_on: Option<NaiveDate>,
    pub reason: String,
    pub created_by: Option<Uuid>,
    pub updated_at: DateTime<Utc>,
    pub removed_at: Option<DateTime<Utc>>,
    /// Chosen servers of the site, when not the whole site.
    pub servers: Vec<Uuid>,
}

/// Selects [`Stored`] rows from `maintenance_windows w`.
pub const SELECT: &str =
    "SELECT w.id, w.site_id, w.whole_site, w.checks, w.repeat, w.every, w.weekdays,
        w.month_day, w.month_week, w.months, w.start_time, w.duration_minutes, w.time_zone,
        w.starts_on, w.ends_on, w.reason, w.created_by, w.updated_at, w.removed_at,
        ARRAY(SELECT server_id FROM maintenance_window_servers ws
              WHERE ws.window_id = w.id ORDER BY server_id) AS servers
    FROM maintenance_windows w";

impl Stored {
    /// Its rule, or `None` when a stored value is not one (a time zone
    /// unknown to this build).
    pub fn schedule(&self) -> Option<Schedule> {
        let small = |v: i16| u8::try_from(v).ok();
        Some(Schedule {
            repeat: Repeat::parse(&self.repeat)?,
            every: u16::try_from(self.every).ok()?,
            weekdays: small(self.weekdays)?,
            month_day: self.month_day.and_then(small),
            month_week: self.month_week.and_then(small),
            months: u16::try_from(self.months).ok()?,
            start_time: self.start_time,
            duration_minutes: u32::try_from(self.duration_minutes).ok()?,
            time_zone: self.time_zone.parse().ok()?,
            starts_on: self.starts_on,
            ends_on: self.ends_on,
        })
    }
}

/// The window `id`, removed or not.
pub async fn load(db: &mut PgConnection, id: Uuid) -> Result<Option<Stored>, sqlx::Error> {
    // SQL-safe: interpolates only the constant SELECT.
    sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} WHERE w.id = $1")))
        .bind(id)
        .fetch_optional(&mut *db)
        .await
}

/// Writes the occurrences of `window` that end after `now` and start
/// within [`AHEAD`], those of its current rule it has not written yet.
/// Returns how many rows were written.
pub async fn write(
    db: &mut PgConnection,
    window: &Stored,
    now: DateTime<Utc>,
) -> Result<u64, sqlx::Error> {
    if window.removed_at.is_some() {
        return Ok(0);
    }
    let Some(schedule) = window.schedule() else {
        tracing::warn!(window = %window.id, zone = %window.time_zone, "a maintenance window's rule cannot be read; nothing written");
        return Ok(0);
    };
    lock(db, window.id).await?;
    let targets: Vec<Option<Uuid>> = if window.whole_site {
        vec![None]
    } else {
        window.servers.iter().copied().map(Some).collect()
    };
    let mut written = 0;
    for (starts_at, ends_at) in schedule.windows(now, now + AHEAD) {
        for server in &targets {
            written += sqlx::query(
                "INSERT INTO silences (server_id, site_id, starts_at, ends_at, reason, created_by,
                                       checks, window_id)
                 SELECT $1, CASE WHEN $1::uuid IS NULL THEN $2::uuid END, $3, $4, $5, $6, $7, $8
                 WHERE NOT EXISTS (
                     SELECT 1 FROM silences x
                     WHERE x.window_id = $8 AND x.starts_at = $3 AND x.created_at >= $9
                       AND x.server_id IS NOT DISTINCT FROM $1)",
            )
            .bind(*server)
            .bind(window.site_id)
            .bind(starts_at)
            .bind(ends_at)
            .bind(&window.reason)
            .bind(window.created_by)
            .bind(&window.checks)
            .bind(window.id)
            .bind(window.updated_at)
            .execute(&mut *db)
            .await?
            .rows_affected();
        }
    }
    Ok(written)
}

/// Before a window changes or goes: its occurrences not started yet,
/// which held nothing, are deleted, and the running ones end now. What
/// they hold is released at the dispatcher's next look only if no
/// silence covers it then, such as the occurrence the new rule writes.
pub async fn stop(db: &mut PgConnection, id: Uuid, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    lock(db, id).await?;
    sqlx::query("DELETE FROM silences WHERE window_id = $1 AND starts_at > $2")
        .bind(id)
        .bind(now)
        .execute(&mut *db)
        .await?;
    sqlx::query(
        "UPDATE silences SET ended_at = $2
         WHERE window_id = $1 AND ended_at IS NULL AND ends_at > $2",
    )
    .bind(id)
    .bind(now)
    .execute(&mut *db)
    .await?;
    Ok(())
}

pub async fn lock(db: &mut PgConnection, id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('maintenance:' || $1::text))")
        .bind(id)
        .execute(&mut *db)
        .await?;
    Ok(())
}

/// Writes every window's next occurrences, one transaction per window.
/// Returns how many rows were written.
pub async fn refresh(pool: &PgPool, now: DateTime<Utc>) -> Result<u64, sqlx::Error> {
    let ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM maintenance_windows WHERE removed_at IS NULL")
            .fetch_all(pool)
            .await?;
    let mut written = 0;
    for id in ids {
        let mut tx = pool.begin().await?;
        // Read under the lock, so that a rule changed meanwhile is the one
        // written.
        lock(&mut tx, id).await?;
        if let Some(window) = load(&mut tx, id).await? {
            written += write(&mut tx, &window, now).await?;
        }
        tx.commit().await?;
    }
    Ok(written)
}

/// Writes every window's occurrences now and every `every` until `until`
/// completes.
pub async fn run(pool: PgPool, every: Duration, until: impl std::future::Future<Output = ()>) {
    tokio::pin!(until);
    let mut ticks = tokio::time::interval(every);
    loop {
        tokio::select! {
            () = &mut until => return,
            _ = ticks.tick() => match refresh(&pool, Utc::now()).await {
                Ok(0) => {}
                Ok(written) => tracing::info!(written, "maintenance windows written as silences"),
                Err(error) => tracing::warn!(%error, "could not write maintenance windows"),
            },
        }
    }
}
