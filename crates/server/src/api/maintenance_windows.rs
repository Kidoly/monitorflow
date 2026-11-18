//! Maintenance windows (ADR-0151): silences that come back every day, week
//! or month, for a site, chosen servers of it, or chosen checks.
//!
//! Anyone who reads the site sees its windows; operators and above of the
//! site make, change and remove them, as they silence the site (ADR-0079).
//! Each change writes the window's coming occurrences as silences in the
//! same transaction (`notify::maintenance::write`).

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgConnection;
use uuid::Uuid;

use super::access::authorize;
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::sensors::{body, parse_id};
use super::silences::{known_checks, Place, MAX_REASON};
use super::{ApiError, ApiState};
use crate::notify::maintenance::write::{self, Stored};
use crate::notify::maintenance::{Repeat, Schedule, EVERY_MONTH};
use crate::notify::silence::CheckRef;

/// Most chosen servers of one window.
pub const MAX_SERVERS: usize = 500;
/// How many coming windows a view lists.
pub const NEXT: usize = 3;

const WEEKDAYS: [&str; 7] = [
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];

/// Query string of `GET /api/maintenance-windows`: at most one.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ListParams {
    /// The windows of this site.
    pub site_id: Option<Uuid>,
    /// The windows covering this server: of its whole site, or naming it.
    pub server_id: Option<Uuid>,
}

/// Body of `POST /api/maintenance-windows` and `PUT
/// /api/maintenance-windows/{id}`; a change replaces every field.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[schema(as = MaintenanceWindowRequest)]
#[serde(deny_unknown_fields)]
pub struct WindowRequest {
    /// The site it belongs to.
    pub site_id: Uuid,
    /// Only these servers of the site, at most 500; empty or absent: the
    /// whole site, servers added later included.
    #[serde(default)]
    pub server_ids: Vec<Uuid>,
    /// Only these checks of the servers covered, 1 to 20; absent or `null`:
    /// everything of them.
    #[serde(default)]
    pub checks: Option<Vec<CheckRef>>,
    /// `daily`, `weekly` or `monthly`.
    pub repeat: String,
    /// Every N days (daily, 1 to 365) or weeks (weekly, 1 to 52), counted
    /// from `starts_on`; 1 (the default) for monthly.
    #[serde(default = "one")]
    pub every: u16,
    /// `monday` to `sunday`: the days of a weekly window, or the one
    /// weekday of a monthly window by week.
    #[serde(default)]
    pub weekdays: Vec<String>,
    /// Monthly: on this day, 1 to 31. A month without it has no window.
    #[serde(default)]
    pub month_day: Option<u8>,
    /// Monthly: on `weekdays` of this week of the month, 1 to 4, or 5 for
    /// the last one ("the last Friday").
    #[serde(default)]
    pub month_week: Option<u8>,
    /// Monthly: in these months, 1 (January) to 12; absent or empty: every
    /// month.
    #[serde(default)]
    pub months: Vec<u8>,
    /// Local time each window starts, `HH:MM`.
    pub start_time: String,
    /// 1 minute to 7 days (10080).
    pub duration_minutes: u32,
    /// IANA time zone the times are in, like `Europe/Paris`; daylight
    /// saving is followed.
    pub time_zone: String,
    /// The first local date a window may start on; today in the zone when
    /// absent.
    #[serde(default)]
    pub starts_on: Option<NaiveDate>,
    /// The last one; absent or `null` for no end.
    #[serde(default)]
    pub ends_on: Option<NaiveDate>,
    /// Why, for others: 1 to 200 characters, trimmed.
    pub reason: String,
}

fn one() -> u16 {
    1
}

/// A server a window names.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = MaintenanceWindowServer)]
pub struct ServerRef {
    pub id: Uuid,
    pub hostname: String,
}

/// One window, from start to end, excluded.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = MaintenanceOccurrence)]
pub struct Occurrence {
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
}

/// One maintenance window.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = MaintenanceWindow)]
pub struct WindowView {
    pub id: Uuid,
    pub site_id: Uuid,
    pub site_name: String,
    pub customer_id: Uuid,
    /// The whole site, servers added later included.
    pub whole_site: bool,
    /// The chosen servers, by name, when not the whole site; none left
    /// when they were all removed, and then it covers nothing.
    pub servers: Vec<ServerRef>,
    /// The chosen checks; `null` for everything of the servers.
    pub checks: Option<Vec<CheckRef>>,
    /// `daily`, `weekly` or `monthly`.
    pub repeat: String,
    pub every: u16,
    /// `monday` to `sunday`, in week order.
    pub weekdays: Vec<String>,
    pub month_day: Option<u8>,
    /// 1 to 4, or 5 for the last week.
    pub month_week: Option<u8>,
    /// 1 to 12; all twelve for every month.
    pub months: Vec<u8>,
    /// `HH:MM`, local.
    pub start_time: String,
    pub duration_minutes: u32,
    pub time_zone: String,
    pub starts_on: NaiveDate,
    pub ends_on: Option<NaiveDate>,
    pub reason: String,
    /// When the one running now ends; `null` when none runs.
    pub active_until: Option<DateTime<Utc>>,
    /// The running window, if any, then the next ones: three in all, fewer
    /// when the rule ends.
    pub next: Vec<Occurrence>,
    /// `null` once the user is deleted.
    pub created_by_username: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Whether this user may change or remove it.
    pub can_change: bool,
}

/// Body of `GET /api/maintenance-windows`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = MaintenanceWindowList)]
pub struct WindowList {
    /// The first to come first.
    pub windows: Vec<WindowView>,
}

/// A request turned into what is stored: the rule, servers and checks.
#[derive(Debug)]
struct Valid {
    schedule: Schedule,
    servers: Vec<Uuid>,
    checks: Option<Vec<CheckRef>>,
    reason: String,
}

/// The rule of `request`, or why not, as a sentence for the form.
fn rule(request: &WindowRequest, now: DateTime<Utc>) -> Result<Schedule, String> {
    let repeat = Repeat::parse(&request.repeat)
        .ok_or_else(|| "Repeat daily, weekly or monthly.".to_owned())?;
    let time_zone: Tz = request.time_zone.parse().map_err(|_| {
        format!(
            "{} is not a time zone, like Europe/Paris.",
            request.time_zone
        )
    })?;
    let start_time = NaiveTime::parse_from_str(&request.start_time, "%H:%M")
        .map_err(|_| "Give the start as a time such as 02:00.".to_owned())?;
    let mut weekdays = 0u8;
    for day in &request.weekdays {
        let index = WEEKDAYS
            .iter()
            .position(|d| d == day)
            .ok_or_else(|| format!("{day} is not a weekday; use monday to sunday."))?;
        weekdays |= 1 << index;
    }
    let mut months = 0u16;
    for month in &request.months {
        if !(1..=12).contains(month) {
            return Err("Months are 1 (January) to 12.".into());
        }
        months |= 1 << (month - 1);
    }
    let schedule = Schedule {
        repeat,
        every: request.every,
        weekdays,
        month_day: request.month_day,
        month_week: request.month_week,
        months: if months == 0 { EVERY_MONTH } else { months },
        start_time,
        duration_minutes: request.duration_minutes,
        time_zone,
        starts_on: request
            .starts_on
            .unwrap_or_else(|| now.with_timezone(&time_zone).date_naive()),
        ends_on: request.ends_on,
    };
    schedule.validate()?;
    Ok(schedule)
}

/// Checks `request` for `user`: the site may be changed by them, the
/// servers are of it, the checks run there.
async fn validate(
    state: &ApiState,
    user: &CurrentUser,
    request: &WindowRequest,
) -> Result<Valid, ApiError> {
    let site: Option<Uuid> = sqlx::query_scalar("SELECT id FROM sites WHERE id = $1")
        .bind(request.site_id)
        .fetch_optional(&state.pool)
        .await?;
    authorize(user, site, true)?;
    let schedule = rule(request, Utc::now()).map_err(ApiError::Invalid)?;
    let reason = request.reason.trim();
    if reason.is_empty() || reason.chars().count() > MAX_REASON {
        return Err(ApiError::Invalid(format!(
            "Say why in 1 to {MAX_REASON} characters, so others know what this window is for."
        )));
    }
    let mut servers = request.server_ids.clone();
    servers.sort_unstable();
    servers.dedup();
    if servers.len() > MAX_SERVERS {
        return Err(ApiError::Invalid(format!(
            "Choose at most {MAX_SERVERS} servers, or none for the whole site."
        )));
    }
    let found: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM servers
         WHERE id = ANY($1) AND site_id = $2 AND revoked_at IS NULL",
    )
    .bind(&servers)
    .bind(request.site_id)
    .fetch_one(&state.pool)
    .await?;
    if usize::try_from(found).ok() != Some(servers.len()) {
        return Err(ApiError::Invalid(
            "Choose servers of the window's site.".into(),
        ));
    }
    let checks = match &request.checks {
        None => None,
        Some(checks) => {
            let place = match servers.as_slice() {
                [one] => Place::Server(*one),
                _ => Place::Site(request.site_id),
            };
            Some(known_checks(&state.pool, checks, place).await?)
        }
    };
    Ok(Valid {
        schedule,
        servers,
        checks,
        reason: reason.to_owned(),
    })
}

/// The stored row of `window` as a view for `user`.
async fn view(
    db: &mut PgConnection,
    user: &CurrentUser,
    window: Stored,
    now: DateTime<Utc>,
) -> Result<WindowView, ApiError> {
    type Extra = (
        String,
        Uuid,
        Option<String>,
        DateTime<Utc>,
        Option<DateTime<Utc>>,
    );
    let (site_name, customer_id, created_by, created_at, active_until): Extra = sqlx::query_as(
        "SELECT si.name, si.customer_id, u.username, w.created_at,
                (SELECT max(coalesce(x.ended_at, x.ends_at)) FROM silences x
                 WHERE x.window_id = w.id AND x.starts_at <= $2
                   AND coalesce(x.ended_at, x.ends_at) > $2)
         FROM maintenance_windows w
         JOIN sites si ON si.id = w.site_id
         LEFT JOIN users u ON u.id = w.created_by
         WHERE w.id = $1",
    )
    .bind(window.id)
    .bind(now)
    .fetch_one(&mut *db)
    .await?;
    let servers: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, hostname FROM servers WHERE id = ANY($1) ORDER BY hostname, id")
            .bind(&window.servers)
            .fetch_all(&mut *db)
            .await?;
    let next = window
        .schedule()
        .map(|s| s.next(now, NEXT))
        .unwrap_or_default();
    let bits = |value: i16, names: usize| (0..names).filter(move |i| value & (1 << i) != 0);
    let small = |v: Option<i16>| v.and_then(|v| u8::try_from(v).ok());
    Ok(WindowView {
        id: window.id,
        site_id: window.site_id,
        site_name,
        customer_id,
        whole_site: window.whole_site,
        servers: servers
            .into_iter()
            .map(|(id, hostname)| ServerRef { id, hostname })
            .collect(),
        checks: window
            .checks
            .as_ref()
            .and_then(|c| serde_json::from_value(c.clone()).ok()),
        repeat: window.repeat.clone(),
        every: u16::try_from(window.every).unwrap_or(1),
        weekdays: bits(window.weekdays, 7)
            .map(|i| WEEKDAYS[i].to_owned())
            .collect(),
        month_day: small(window.month_day),
        month_week: small(window.month_week),
        months: bits(window.months, 12)
            .filter_map(|i| u8::try_from(i + 1).ok())
            .collect(),
        start_time: window.start_time.format("%H:%M").to_string(),
        duration_minutes: u32::try_from(window.duration_minutes).unwrap_or(0),
        time_zone: window.time_zone.clone(),
        starts_on: window.starts_on,
        ends_on: window.ends_on,
        reason: window.reason.clone(),
        active_until,
        next: next
            .into_iter()
            .map(|(starts_at, ends_at)| Occurrence { starts_at, ends_at })
            .collect(),
        created_by_username: created_by,
        created_at,
        updated_at: window.updated_at,
        can_change: user.modifiable_sites().contains(window.site_id),
    })
}

/// `GET /api/maintenance-windows`: the windows of the sites the user
/// reads, the next to come first.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
    params: Result<Query<ListParams>, QueryRejection>,
) -> Result<Json<WindowList>, ApiError> {
    let Query(params) = params.map_err(|r| ApiError::Invalid(r.body_text()))?;
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let (site, server) = match (params.site_id, params.server_id) {
        (Some(_), Some(_)) => {
            return Err(ApiError::Invalid(
                "give at most one of site_id and server_id".into(),
            ))
        }
        (Some(site), None) => {
            let found: Option<Uuid> = sqlx::query_scalar("SELECT id FROM sites WHERE id = $1")
                .bind(site)
                .fetch_optional(&state.pool)
                .await?;
            (Some(authorize(&user, found, false)?), None)
        }
        (None, Some(server)) => {
            let found: Option<Uuid> =
                sqlx::query_scalar("SELECT site_id FROM servers WHERE id = $1")
                    .bind(server)
                    .fetch_optional(&state.pool)
                    .await?;
            (Some(authorize(&user, found, false)?), Some(server))
        }
        (None, None) => (None, None),
    };
    let (all, sites) = user.readable_sites().bind();
    let mut conn = state.pool.acquire().await?;
    // SQL-safe: interpolates only the constant SELECT.
    let stored: Vec<Stored> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{} WHERE w.removed_at IS NULL AND ($1 OR w.site_id = ANY($2))
           AND ($3::uuid IS NULL OR w.site_id = $3)
           AND ($4::uuid IS NULL OR w.whole_site
                OR EXISTS (SELECT 1 FROM maintenance_window_servers ws
                           WHERE ws.window_id = w.id AND ws.server_id = $4))
         ORDER BY w.created_at, w.id",
        write::SELECT
    )))
    .bind(all)
    .bind(&sites)
    .bind(site)
    .bind(server)
    .fetch_all(&mut *conn)
    .await?;
    let now = Utc::now();
    let mut windows = Vec::with_capacity(stored.len());
    for window in stored {
        windows.push(view(&mut conn, &user, window, now).await?);
    }
    windows.sort_by_key(|w| w.next.first().map(|n| n.starts_at));
    Ok(Json(WindowList { windows }))
}

/// Writes `valid` into the row `id` and its servers.
async fn store(db: &mut PgConnection, id: Uuid, valid: &Valid) -> Result<(), sqlx::Error> {
    let s = &valid.schedule;
    sqlx::query(
        "UPDATE maintenance_windows SET checks = $2, repeat = $3, every = $4, weekdays = $5,
                month_day = $6, month_week = $7, months = $8, start_time = $9,
                duration_minutes = $10, time_zone = $11, starts_on = $12, ends_on = $13,
                reason = $14, whole_site = $15, updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(valid.checks.as_ref().map(sqlx::types::Json))
    .bind(s.repeat.as_str())
    .bind(i16::try_from(s.every).unwrap_or(1))
    .bind(i16::from(s.weekdays))
    .bind(s.month_day.map(i16::from))
    .bind(s.month_week.map(i16::from))
    .bind(i16::try_from(s.months).unwrap_or(0xfff))
    .bind(s.start_time)
    .bind(i32::try_from(s.duration_minutes).unwrap_or(1))
    .bind(s.time_zone.name())
    .bind(s.starts_on)
    .bind(s.ends_on)
    .bind(&valid.reason)
    .bind(valid.servers.is_empty())
    .execute(&mut *db)
    .await?;
    sqlx::query("DELETE FROM maintenance_window_servers WHERE window_id = $1")
        .bind(id)
        .execute(&mut *db)
        .await?;
    sqlx::query(
        "INSERT INTO maintenance_window_servers (window_id, server_id)
         SELECT $1, unnest($2::uuid[])",
    )
    .bind(id)
    .bind(&valid.servers)
    .execute(&mut *db)
    .await?;
    Ok(())
}

/// What the audit log keeps of a window.
fn audited(valid: &Valid, site: Uuid) -> serde_json::Value {
    let s = &valid.schedule;
    json!({
        "site_id": site,
        "server_ids": valid.servers,
        "checks": valid.checks,
        "repeat": s.repeat.as_str(),
        "every": s.every,
        "weekdays": s.weekdays,
        "month_day": s.month_day,
        "month_week": s.month_week,
        "months": s.months,
        "start_time": s.start_time.format("%H:%M").to_string(),
        "duration_minutes": s.duration_minutes,
        "time_zone": s.time_zone.name(),
        "starts_on": s.starts_on,
        "ends_on": s.ends_on,
        "reason": valid.reason,
    })
}

async fn record(
    db: &mut PgConnection,
    user: &CurrentUser,
    action: &str,
    id: Uuid,
    details: serde_json::Value,
) -> Result<(), sqlx::Error> {
    let actor = user.id.to_string();
    audit::record(
        &mut *db,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action,
            target: Some(("maintenance_window", &id.to_string())),
            details,
        },
    )
    .await
}

/// `POST /api/maintenance-windows`.
pub async fn create(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<WindowRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<WindowView>), ApiError> {
    let request = body(request)?;
    let valid = validate(&state, &user, &request).await?;
    let mut tx = state.pool.begin().await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO maintenance_windows (site_id, repeat, start_time, duration_minutes,
                                          time_zone, starts_on, reason, created_by, weekdays,
                                          month_day, month_week)
         VALUES ($1, 'weekly', '00:00', 1, 'UTC', $2, $3, $4, 1, NULL, NULL) RETURNING id",
    )
    .bind(request.site_id)
    .bind(valid.schedule.starts_on)
    .bind(&valid.reason)
    .bind(user.id)
    .fetch_one(&mut *tx)
    .await?;
    store(&mut tx, id, &valid).await?;
    record(
        &mut tx,
        &user,
        "maintenance_window.create",
        id,
        audited(&valid, request.site_id),
    )
    .await?;
    let now = Utc::now();
    let stored = write::load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    write::write(&mut tx, &stored, now).await?;
    let view = view(&mut tx, &user, stored, now).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(view)))
}

/// The window `id` the user sees, not removed, or 404.
async fn visible(db: &mut PgConnection, user: &CurrentUser, id: &str) -> Result<Stored, ApiError> {
    let id = parse_id(id)?;
    let stored = write::load(db, id)
        .await?
        .filter(|w| w.removed_at.is_none());
    authorize(user, stored.as_ref().map(|w| w.site_id), false)?;
    stored.ok_or(ApiError::NotFound)
}

/// `PUT /api/maintenance-windows/{id}`: a new rule, place or reason. The
/// running occurrence ends and the new rule's are written at once, so
/// nothing held is released unless the new rule leaves it uncovered.
pub async fn update(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<WindowRequest>, JsonRejection>,
) -> Result<Json<WindowView>, ApiError> {
    let request = body(request)?;
    let mut conn = state.pool.acquire().await?;
    let before = visible(&mut conn, &user, &id).await?;
    drop(conn);
    authorize(&user, Some(before.site_id), true)?;
    let valid = validate(&state, &user, &request).await?;
    let now = Utc::now();
    let mut tx = state.pool.begin().await?;
    write::stop(&mut tx, before.id, now).await?;
    sqlx::query("UPDATE maintenance_windows SET site_id = $2 WHERE id = $1")
        .bind(before.id)
        .bind(request.site_id)
        .execute(&mut *tx)
        .await?;
    store(&mut tx, before.id, &valid).await?;
    record(
        &mut tx,
        &user,
        "maintenance_window.update",
        before.id,
        audited(&valid, request.site_id),
    )
    .await?;
    let stored = write::load(&mut tx, before.id)
        .await?
        .ok_or(ApiError::NotFound)?;
    write::write(&mut tx, &stored, now).await?;
    let view = view(&mut tx, &user, stored, now).await?;
    tx.commit().await?;
    Ok(Json(view))
}

/// `DELETE /api/maintenance-windows/{id}`: no more windows; the running
/// one ends now and what it held is told at the dispatcher's next look.
pub async fn remove(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let mut tx = state.pool.begin().await?;
    let window = visible(&mut tx, &user, &id).await?;
    authorize(&user, Some(window.site_id), true)?;
    write::stop(&mut tx, window.id, Utc::now()).await?;
    sqlx::query(
        "UPDATE maintenance_windows SET removed_at = now(), removed_by = $2
         WHERE id = $1 AND removed_at IS NULL",
    )
    .bind(window.id)
    .bind(user.id)
    .execute(&mut *tx)
    .await?;
    record(
        &mut tx,
        &user,
        "maintenance_window.remove",
        window.id,
        json!({"site_id": window.site_id, "reason": window.reason}),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> WindowRequest {
        WindowRequest {
            site_id: Uuid::nil(),
            server_ids: vec![],
            checks: None,
            repeat: "weekly".into(),
            every: 1,
            weekdays: vec!["sunday".into(), "monday".into()],
            month_day: None,
            month_week: None,
            months: vec![],
            start_time: "02:00".into(),
            duration_minutes: 120,
            time_zone: "Europe/Paris".into(),
            starts_on: None,
            ends_on: None,
            reason: "Patch night".into(),
        }
    }

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn a_request_becomes_a_rule() {
        // 23:30 UTC on the 18th is already the 19th in Paris.
        let rule = rule(&request(), at("2026-11-18T23:30:00Z")).unwrap();
        assert_eq!(rule.weekdays, 0b100_0001);
        assert_eq!(rule.months, EVERY_MONTH);
        assert_eq!(rule.starts_on, "2026-11-19".parse::<NaiveDate>().unwrap());
        let monthly = WindowRequest {
            repeat: "monthly".into(),
            weekdays: vec!["tuesday".into()],
            month_week: Some(2),
            months: vec![1, 7],
            ..request()
        };
        let rule = super::rule(&monthly, at("2026-11-18T10:00:00Z")).unwrap();
        assert_eq!((rule.weekdays, rule.months), (0b10, 0b100_0001));
    }

    #[test]
    fn requests_that_say_nothing_sensible() {
        let now = at("2026-11-18T10:00:00Z");
        for bad in [
            WindowRequest {
                repeat: "yearly".into(),
                ..request()
            },
            WindowRequest {
                weekdays: vec!["sundy".into()],
                ..request()
            },
            WindowRequest {
                weekdays: vec![],
                ..request()
            },
            WindowRequest {
                start_time: "2am".into(),
                ..request()
            },
            WindowRequest {
                time_zone: "Paris".into(),
                ..request()
            },
            WindowRequest {
                duration_minutes: 0,
                ..request()
            },
            WindowRequest {
                months: vec![13],
                ..request()
            },
            WindowRequest {
                repeat: "monthly".into(),
                ..request()
            },
        ] {
            assert!(rule(&bad, now).is_err(), "{bad:?}");
        }
    }
}
