//! On call (ADR-0081): schedules of who is on duty for everything, a
//! customer or a site, their overrides, and the contact each user keeps.
//!
//! A schedule is managed like a channel of the same coverage (ADR-0078):
//! by the administrators of what it covers; others do not see it, except
//! its members, who see it without changing it. Only someone who sees the
//! whole place through a grant on it, its customer or (for global
//! administrators) everything can be put on it. A user changes their own
//! contact, nobody else's.

use std::collections::HashMap;

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Datelike, NaiveTime, TimeDelta, Utc, Weekday};
use chrono_tz::Tz;
use lettre::message::Mailbox;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgConnection, PgExecutor, Row};
use uuid::Uuid;

use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::notifications::{checked, is_global_admin, manages, require_administrator};
use super::sensors::{body, parse_id};
use super::{ApiError, ApiState};
use crate::notify::on_call::{
    self, mail_channel, Directory, Problem, Rota, Rotation, Schedule, Source,
};
use crate::notify::{self, Channel, Coverage, Destination, Dispatcher};

/// How far ahead the screen shows turns.
pub const HORIZON_DAYS: i64 = 28;
/// Longest override, and how far ahead one may start.
pub const MAX_OVERRIDE_DAYS: i64 = 90;
const MAX_OVERRIDE_START_DAYS: i64 = 366;
/// Most members of one schedule.
pub const MAX_MEMBERS: usize = 50;
const MAX_NAME: usize = 80;

/// One period with one person on call.
#[derive(Debug, Clone, Serialize)]
pub struct TurnView {
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    /// `null` for a removed user's turn or a schedule without members.
    pub user_id: Option<Uuid>,
    pub username: Option<String>,
    /// Set when an override puts this person on.
    pub override_id: Option<Uuid>,
    /// Why this person cannot be told: the period is a gap.
    pub problem: Option<Problem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemberView {
    pub user_id: Option<Uuid>,
    pub username: Option<String>,
    pub problem: Option<Problem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OverrideView {
    pub id: Uuid,
    pub user_id: Uuid,
    pub username: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
}

/// A schedule as the screen shows it.
#[derive(Debug, Clone, Serialize)]
pub struct ScheduleView {
    pub id: Uuid,
    pub name: String,
    /// The customer it covers, or the customer of its site; `null` for
    /// everything.
    pub customer_id: Option<Uuid>,
    pub customer_name: Option<String>,
    pub site_id: Option<Uuid>,
    pub site_name: Option<String>,
    pub time_zone: String,
    /// `daily` or `weekly`.
    pub rotation: Rotation,
    /// Local time of the hand-off, `HH:MM`.
    pub handoff_time: String,
    /// ISO weekday of a weekly hand-off, 1 for Monday; `null` when daily.
    pub handoff_day: Option<u32>,
    pub members: Vec<MemberView>,
    /// Overrides not over yet, by start.
    pub overrides: Vec<OverrideView>,
    /// Who is on call now; `null` only without members.
    pub now: Option<TurnView>,
    /// The next one who takes over.
    pub next: Option<TurnView>,
    /// From now to [`HORIZON_DAYS`] ahead.
    pub turns: Vec<TurnView>,
    /// Who is told during a gap; empty when nobody can be, and for members
    /// who do not manage it.
    pub fallback: Vec<String>,
    /// The mail server personal email goes through; `null` when there is
    /// none, and for members who do not manage it.
    pub mail_host: Option<String>,
    pub can_manage: bool,
}

#[derive(Debug, Serialize)]
pub struct ScheduleList {
    pub schedules: Vec<ScheduleView>,
}

/// Body of `POST /api/on-call/schedules` and of `PUT` on one.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleRequest {
    pub name: String,
    #[serde(default)]
    pub customer_id: Option<Uuid>,
    #[serde(default)]
    pub site_id: Option<Uuid>,
    /// An IANA name, like `Europe/Paris`.
    pub time_zone: String,
    pub rotation: String,
    /// `HH:MM`, local.
    pub handoff_time: String,
    /// ISO weekday, 1 for Monday; needed when weekly.
    #[serde(default)]
    pub handoff_day: Option<u32>,
    /// In order of their turns.
    pub members: Vec<Uuid>,
}

/// Body of `POST /api/on-call/schedules/{id}/overrides`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverrideRequest {
    pub user_id: Uuid,
    /// Now when absent or past.
    #[serde(default)]
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: DateTime<Utc>,
}

/// Query of `GET /api/on-call/people`: the coverage of a schedule.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeopleParams {
    pub customer_id: Option<Uuid>,
    pub site_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
pub struct PersonView {
    pub id: Uuid,
    pub username: String,
    /// Whether they set an email address or a chat.
    pub has_contact: bool,
}

#[derive(Debug, Serialize)]
pub struct PeopleList {
    pub people: Vec<PersonView>,
}

/// A user's own contact.
#[derive(Debug, Serialize, Default)]
pub struct ContactView {
    pub email: Option<String>,
    /// `webhook`, `slack`, `teams` or `discord`; `null` without one.
    pub kind: Option<String>,
    pub settings: Option<Value>,
    /// Whether a chat URL or signing secret is stored.
    pub has_secret: bool,
    pub last_sent_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub last_error_at: Option<DateTime<Utc>>,
}

/// Body of `PUT /api/on-call/contact`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContactRequest {
    #[serde(default)]
    pub email: Option<String>,
    /// `null` removes the chat or webhook.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub settings: Option<Value>,
    /// Absent, `null` or empty keeps the stored one.
    #[serde(default)]
    pub secret: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ContactTest {
    /// `email` or the chat's kind.
    pub kind: String,
    pub ok: bool,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ContactTests {
    pub results: Vec<ContactTest>,
}

/// Names of the places of schedules.
struct Names {
    name: String,
    customer_id: Option<Uuid>,
    customer_name: Option<String>,
    site_name: Option<String>,
}

async fn names(db: impl PgExecutor<'_>) -> Result<HashMap<Uuid, Names>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT o.id, o.name, coalesce(o.customer_id, s.customer_id) AS customer_id,
                c.name AS customer_name, s.name AS site_name
         FROM on_call_schedules o
         LEFT JOIN sites s ON s.id = o.site_id
         LEFT JOIN customers c ON c.id = coalesce(o.customer_id, s.customer_id)",
    )
    .fetch_all(db)
    .await?;
    rows.iter()
        .map(|row| {
            Ok((
                row.try_get("id")?,
                Names {
                    name: row.try_get("name")?,
                    customer_id: row.try_get("customer_id")?,
                    customer_name: row.try_get("customer_name")?,
                    site_name: row.try_get("site_name")?,
                },
            ))
        })
        .collect()
}

fn username(directory: &Directory, user: Option<Uuid>) -> Option<String> {
    user.and_then(|id| directory.people.get(&id))
        .map(|p| p.username.clone())
}

/// The screen's view of `schedule` at `now`.
fn view(
    schedule: &Schedule,
    names: &Names,
    directory: &Directory,
    channels: &[Channel],
    now: DateTime<Utc>,
    can_manage: bool,
) -> ScheduleView {
    let coverage = schedule.coverage;
    let mail = mail_channel(channels, directory, coverage);
    let problem = |user, source| match source {
        Source::Nobody => Some(Problem::Nobody),
        _ => directory.problem(user, coverage, mail.is_some()),
    };
    let turn = |span: &on_call::Span| TurnView {
        starts_at: span.starts_at,
        ends_at: span.ends_at,
        user_id: span.user,
        username: username(directory, span.user),
        override_id: match span.source {
            Source::Override(id) => Some(id),
            _ => None,
        },
        problem: problem(span.user, span.source),
    };
    // From far enough back that the turn running now shows its start.
    let back = now - TimeDelta::days(MAX_OVERRIDE_DAYS + 7);
    let spans = schedule.spans(back, now + TimeDelta::days(HORIZON_DAYS));
    let turns: Vec<TurnView> = spans.iter().filter(|s| s.ends_at > now).map(turn).collect();
    let current = turns.first().cloned();
    let next = current.as_ref().and_then(|c| {
        turns
            .iter()
            .skip(1)
            .find(|t| t.user_id != c.user_id || t.problem != c.problem)
            .cloned()
    });
    let (fallback, mail_host) = if can_manage {
        let fallback = directory
            .fallback(coverage, mail.is_some())
            .into_iter()
            .filter_map(|id| username(directory, Some(id)))
            .collect();
        let host = mail.and_then(|c| match &c.destination {
            Destination::Email(email) => Some(email.host.clone()),
            _ => None,
        });
        (fallback, host)
    } else {
        (Vec::new(), None)
    };
    let (_, site_id) = coverage.columns();
    let rota = &schedule.rota;
    ScheduleView {
        id: schedule.id,
        name: names.name.clone(),
        customer_id: names.customer_id,
        customer_name: names.customer_name.clone(),
        site_id,
        site_name: names.site_name.clone(),
        time_zone: rota.time_zone.name().to_owned(),
        rotation: rota.rotation,
        handoff_time: rota.handoff.format("%H:%M").to_string(),
        handoff_day: (rota.rotation == Rotation::Weekly)
            .then(|| rota.anchor.weekday().number_from_monday()),
        members: rota
            .members
            .iter()
            .map(|user| MemberView {
                user_id: *user,
                username: username(directory, *user),
                problem: directory.problem(*user, coverage, mail.is_some()),
            })
            .collect(),
        overrides: schedule
            .overrides
            .iter()
            .filter(|o| o.ends_at > now)
            .map(|o| OverrideView {
                id: o.id,
                user_id: o.user,
                username: username(directory, Some(o.user)),
                starts_at: o.starts_at,
                ends_at: o.ends_at,
            })
            .collect(),
        now: current,
        next,
        turns,
        fallback,
        mail_host,
        can_manage,
    }
}

/// Views of the schedules `user` manages or is a member of, or only
/// `only`.
async fn views(
    conn: &mut PgConnection,
    user: &CurrentUser,
    only: Option<Uuid>,
) -> Result<Vec<ScheduleView>, ApiError> {
    let now = Utc::now();
    let schedules = on_call::schedules(conn, now, only).await?;
    let directory = Directory::load(conn).await?;
    let channels = notify::channels(&mut *conn).await?;
    let names = names(&mut *conn).await?;
    Ok(schedules
        .iter()
        .filter_map(|s| {
            let can_manage = manages(user, s.coverage);
            let member = s.rota.members.contains(&Some(user.id));
            let names = names.get(&s.id)?;
            (can_manage || member).then(|| view(s, names, &directory, &channels, now, can_manage))
        })
        .collect())
}

/// The users `user` may put on a schedule for `coverage`: those with a
/// grant on its site or its customer, and, for a global administrator,
/// those with a global grant. Disabled users are left out.
async fn candidates(
    db: impl PgExecutor<'_>,
    user: &CurrentUser,
    coverage: Coverage,
) -> Result<Vec<PersonView>, ApiError> {
    let (customer, site) = match coverage {
        Coverage::Everything => (None, None),
        Coverage::Customer(id) => (Some(id), None),
        Coverage::Site(id) => (None, Some(id)),
    };
    Ok(sqlx::query(
        "SELECT DISTINCT u.id, u.username, c.user_id IS NOT NULL AS has_contact
         FROM users u
         JOIN user_roles r ON r.user_id = u.id
         LEFT JOIN user_contacts c ON c.user_id = u.id
         WHERE u.disabled_at IS NULL AND (
             ($1 AND r.customer_id IS NULL AND r.site_id IS NULL)
             OR r.customer_id = coalesce($2, (SELECT customer_id FROM sites WHERE id = $3))
             OR r.site_id = $3)
         ORDER BY u.username",
    )
    .bind(is_global_admin(user))
    .bind(customer)
    .bind(site)
    .fetch_all(db)
    .await?
    .iter()
    .map(|row| {
        Ok(PersonView {
            id: row.try_get("id")?,
            username: row.try_get("username")?,
            has_contact: row.try_get("has_contact")?,
        })
    })
    .collect::<Result<_, sqlx::Error>>()?)
}

/// The coverage of a request's columns, which `user` must manage: 403
/// before any lookup otherwise, as for channels.
fn coverage_of(
    user: &CurrentUser,
    customer_id: Option<Uuid>,
    site_id: Option<Uuid>,
) -> Result<Coverage, ApiError> {
    let coverage = Coverage::from_columns(customer_id, site_id)
        .ok_or_else(|| ApiError::Invalid("choose a customer or a site, not both".into()))?;
    if manages(user, coverage) {
        Ok(coverage)
    } else {
        Err(ApiError::Forbidden)
    }
}

/// A valid schedule request.
struct Valid {
    name: String,
    coverage: Coverage,
    time_zone: Tz,
    rotation: Rotation,
    handoff: NaiveTime,
    weekday: Weekday,
    members: Vec<Uuid>,
}

/// Checks a request; `current` holds the members already on the schedule,
/// who may stay though they could not be added now.
async fn validate(
    conn: &mut PgConnection,
    user: &CurrentUser,
    request: ScheduleRequest,
    current: &[Option<Uuid>],
) -> Result<Valid, ApiError> {
    let coverage = coverage_of(user, request.customer_id, request.site_id)?;
    let name = request.name.trim().to_owned();
    if name.is_empty() || name.chars().count() > MAX_NAME {
        return Err(ApiError::Invalid(format!(
            "Give the schedule a name of at most {MAX_NAME} characters."
        )));
    }
    let time_zone: Tz = request.time_zone.parse().map_err(|_| {
        ApiError::Invalid(format!(
            "`{}` is not a time zone; choose one like Europe/Paris.",
            request.time_zone
        ))
    })?;
    let rotation = Rotation::parse(&request.rotation)
        .ok_or_else(|| ApiError::Invalid("Hand off daily or weekly.".into()))?;
    let handoff = NaiveTime::parse_from_str(request.handoff_time.trim(), "%H:%M")
        .map_err(|_| ApiError::Invalid("Give the hand-off time as HH:MM, like 09:00.".into()))?;
    let weekday = match (rotation, request.handoff_day) {
        (Rotation::Daily, _) => Weekday::Mon,
        (Rotation::Weekly, Some(day @ 1..=7)) => {
            Weekday::try_from(u8::try_from(day - 1).unwrap_or(0)).unwrap_or(Weekday::Mon)
        }
        (Rotation::Weekly, _) => {
            return Err(ApiError::Invalid(
                "Choose the day of the weekly hand-off.".into(),
            ))
        }
    };
    if request.members.is_empty() {
        return Err(ApiError::Invalid("Add at least one person.".into()));
    }
    if request.members.len() > MAX_MEMBERS {
        return Err(ApiError::Invalid(format!(
            "At most {MAX_MEMBERS} people per schedule."
        )));
    }
    let mut seen = std::collections::HashSet::new();
    if !request.members.iter().all(|m| seen.insert(*m)) {
        return Err(ApiError::Invalid("Each person takes one turn.".into()));
    }
    let unknown = match coverage {
        Coverage::Everything => None,
        Coverage::Customer(id) => {
            Some(sqlx::query_scalar::<_, Uuid>("SELECT id FROM customers WHERE id = $1").bind(id))
        }
        Coverage::Site(id) => {
            Some(sqlx::query_scalar::<_, Uuid>("SELECT id FROM sites WHERE id = $1").bind(id))
        }
    };
    if let Some(query) = unknown {
        if query.fetch_optional(&mut *conn).await?.is_none() {
            return Err(ApiError::Invalid("unknown customer or site".into()));
        }
    }
    let allowed = candidates(&mut *conn, user, coverage).await?;
    for member in &request.members {
        if !current.contains(&Some(*member)) && !allowed.iter().any(|p| p.id == *member) {
            return Err(ApiError::Invalid(
                "Only people who see all of this place can be on call for it.".into(),
            ));
        }
    }
    Ok(Valid {
        name,
        coverage,
        time_zone,
        rotation,
        handoff,
        weekday,
        members: request.members,
    })
}

async fn write_members(
    conn: &mut PgConnection,
    schedule: Uuid,
    members: &[Uuid],
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM on_call_members WHERE schedule_id = $1")
        .bind(schedule)
        .execute(&mut *conn)
        .await?;
    sqlx::query(
        "INSERT INTO on_call_members (schedule_id, position, user_id)
         SELECT $1, position - 1, user_id FROM unnest($2::uuid[]) WITH ORDINALITY AS m(user_id, position)",
    )
    .bind(schedule)
    .bind(members)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn record(
    db: impl PgExecutor<'_>,
    user: &CurrentUser,
    action: &str,
    target: (&str, Uuid),
    details: Value,
) -> Result<(), ApiError> {
    let actor = user.id.to_string();
    let id = target.1.to_string();
    audit::record(
        db,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action,
            target: Some((target.0, &id)),
            details,
        },
    )
    .await?;
    Ok(())
}

fn audited(valid: &Valid) -> Value {
    let (customer_id, site_id) = valid.coverage.columns();
    json!({
        "name": valid.name,
        "customer_id": customer_id,
        "site_id": site_id,
        "time_zone": valid.time_zone.name(),
        "rotation": valid.rotation.as_str(),
        "handoff_time": valid.handoff.format("%H:%M").to_string(),
        "members": valid.members,
    })
}

async fn one(
    conn: &mut PgConnection,
    user: &CurrentUser,
    id: Uuid,
) -> Result<ScheduleView, ApiError> {
    views(conn, user, Some(id))
        .await?
        .into_iter()
        .next()
        .ok_or(ApiError::NotFound)
}

/// A schedule locked for a change, which `user` must manage: 404 otherwise.
async fn lock(conn: &mut PgConnection, user: &CurrentUser, id: Uuid) -> Result<Schedule, ApiError> {
    let found: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM on_call_schedules WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
    found.ok_or(ApiError::NotFound)?;
    on_call::schedules(conn, Utc::now(), Some(id))
        .await?
        .into_iter()
        .next()
        .filter(|s| manages(user, s.coverage))
        .ok_or(ApiError::NotFound)
}

/// `GET /api/on-call/schedules`: those `user` manages or is on.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<ScheduleList>, ApiError> {
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let mut conn = state.pool.acquire().await?;
    let mut schedules = views(&mut conn, &user, None).await?;
    schedules.sort_by_key(|s| s.name.to_lowercase());
    Ok(Json(ScheduleList { schedules }))
}

/// `POST /api/on-call/schedules`: administrators of what it covers. The
/// first person is on call from the last hand-off; audited as
/// `on_call.schedule_create`.
pub async fn create(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<ScheduleRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<ScheduleView>), ApiError> {
    require_administrator(&user)?;
    let request = body(request)?;
    let mut tx = state.pool.begin().await?;
    let valid = validate(&mut tx, &user, request, &[]).await?;
    let anchor = Rota::last_handoff_date(
        valid.time_zone,
        valid.rotation,
        valid.handoff,
        valid.weekday,
        Utc::now(),
    );
    let (customer_id, site_id) = valid.coverage.columns();
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO on_call_schedules (name, customer_id, site_id, time_zone, rotation,
                                        handoff_time, anchor_date, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(&valid.name)
    .bind(customer_id)
    .bind(site_id)
    .bind(valid.time_zone.name())
    .bind(valid.rotation.as_str())
    .bind(valid.handoff)
    .bind(anchor)
    .bind(user.id)
    .fetch_one(&mut *tx)
    .await?;
    write_members(&mut tx, id, &valid.members).await?;
    record(
        &mut *tx,
        &user,
        "on_call.schedule_create",
        ("on_call_schedule", id),
        audited(&valid),
    )
    .await?;
    let view = one(&mut tx, &user, id).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(view)))
}

/// `PUT /api/on-call/schedules/{id}`: administrators of what it covers now
/// and will cover. Whoever is on call by the rotation now stays on call
/// until the next hand-off, if still a member; if not, the one after them
/// in the list takes over. Audited as `on_call.schedule_update`.
pub async fn update(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<ScheduleRequest>, JsonRejection>,
) -> Result<Json<ScheduleView>, ApiError> {
    require_administrator(&user)?;
    let id = parse_id(&id)?;
    let request = body(request)?;
    let mut tx = state.pool.begin().await?;
    let current = lock(&mut tx, &user, id).await?;
    let valid = validate(&mut tx, &user, request, &current.rota.members).await?;
    let now = Utc::now();
    let turn = current.rota.turn_at(now);
    let position = current.rota.position(turn).unwrap_or(0);
    let on_call = current.rota.members.get(position).copied().flatten();
    let anchor_position = on_call
        .and_then(|user| valid.members.iter().position(|m| *m == user))
        .unwrap_or(position % valid.members.len());
    let anchor = Rota::last_handoff_date(
        valid.time_zone,
        valid.rotation,
        valid.handoff,
        valid.weekday,
        now,
    );
    let (customer_id, site_id) = valid.coverage.columns();
    sqlx::query(
        "UPDATE on_call_schedules
         SET name = $2, customer_id = $3, site_id = $4, time_zone = $5, rotation = $6,
             handoff_time = $7, anchor_date = $8, anchor_position = $9
         WHERE id = $1",
    )
    .bind(id)
    .bind(&valid.name)
    .bind(customer_id)
    .bind(site_id)
    .bind(valid.time_zone.name())
    .bind(valid.rotation.as_str())
    .bind(valid.handoff)
    .bind(anchor)
    .bind(i32::try_from(anchor_position).unwrap_or(0))
    .execute(&mut *tx)
    .await?;
    write_members(&mut tx, id, &valid.members).await?;
    record(
        &mut *tx,
        &user,
        "on_call.schedule_update",
        ("on_call_schedule", id),
        audited(&valid),
    )
    .await?;
    let view = one(&mut tx, &user, id).await?;
    tx.commit().await?;
    Ok(Json(view))
}

/// `DELETE /api/on-call/schedules/{id}`: administrators of what it covers;
/// 204; audited as `on_call.schedule_delete`.
pub async fn delete(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_administrator(&user)?;
    let id = parse_id(&id)?;
    let mut tx = state.pool.begin().await?;
    lock(&mut tx, &user, id).await?;
    sqlx::query("DELETE FROM on_call_schedules WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    record(
        &mut *tx,
        &user,
        "on_call.schedule_delete",
        ("on_call_schedule", id),
        json!({}),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/on-call/schedules/{id}/overrides`: administrators of what
/// it covers put a member, or anyone they could add, on call for a period;
/// audited as `on_call.override_create`. Answers the schedule.
pub async fn add_override(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<OverrideRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<ScheduleView>), ApiError> {
    require_administrator(&user)?;
    let id = parse_id(&id)?;
    let request = body(request)?;
    let mut tx = state.pool.begin().await?;
    let schedule = lock(&mut tx, &user, id).await?;
    let now = Utc::now();
    let starts_at = request.starts_at.filter(|s| *s > now).unwrap_or(now);
    let ends_at = request.ends_at;
    if ends_at <= starts_at {
        return Err(ApiError::Invalid("The end comes after the start.".into()));
    }
    if ends_at - starts_at > TimeDelta::days(MAX_OVERRIDE_DAYS) {
        return Err(ApiError::Invalid(format!(
            "An override lasts at most {MAX_OVERRIDE_DAYS} days."
        )));
    }
    if starts_at > now + TimeDelta::days(MAX_OVERRIDE_START_DAYS) {
        return Err(ApiError::Invalid("Start within a year.".into()));
    }
    let member = schedule.rota.members.contains(&Some(request.user_id));
    if !member
        && !candidates(&mut *tx, &user, schedule.coverage)
            .await?
            .iter()
            .any(|p| p.id == request.user_id)
    {
        return Err(ApiError::Invalid(
            "Only people who see all of this place can be on call for it.".into(),
        ));
    }
    let created: Uuid = sqlx::query_scalar(
        "INSERT INTO on_call_overrides (schedule_id, user_id, starts_at, ends_at, created_by)
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(id)
    .bind(request.user_id)
    .bind(starts_at)
    .bind(ends_at)
    .bind(user.id)
    .fetch_one(&mut *tx)
    .await?;
    let details = json!({
        "schedule_id": id, "user_id": request.user_id,
        "starts_at": starts_at, "ends_at": ends_at,
    });
    record(
        &mut *tx,
        &user,
        "on_call.override_create",
        ("on_call_override", created),
        details,
    )
    .await?;
    let view = one(&mut tx, &user, id).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(view)))
}

/// `DELETE /api/on-call/overrides/{id}`: administrators of what its
/// schedule covers; 204; audited as `on_call.override_delete`.
pub async fn delete_override(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_administrator(&user)?;
    let id = parse_id(&id)?;
    let mut tx = state.pool.begin().await?;
    let schedule: Option<Uuid> =
        sqlx::query_scalar("SELECT schedule_id FROM on_call_overrides WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let schedule = schedule.ok_or(ApiError::NotFound)?;
    lock(&mut tx, &user, schedule).await?;
    sqlx::query("DELETE FROM on_call_overrides WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    record(
        &mut *tx,
        &user,
        "on_call.override_delete",
        ("on_call_override", id),
        json!({ "schedule_id": schedule }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/on-call/people?customer_id=&site_id=`: who `user` may put on
/// a schedule of that coverage; 403 for a coverage they do not manage.
pub async fn people(
    State(state): State<ApiState>,
    user: CurrentUser,
    params: Result<Query<PeopleParams>, QueryRejection>,
) -> Result<Json<PeopleList>, ApiError> {
    require_administrator(&user)?;
    let Query(params) = params.map_err(|_| ApiError::BadRequest)?;
    let coverage = coverage_of(&user, params.customer_id, params.site_id)?;
    let people = candidates(&state.pool, &user, coverage).await?;
    Ok(Json(PeopleList { people }))
}

/// A stored contact.
struct Stored {
    chat: Option<Destination>,
    has_secret: bool,
    public_only: bool,
}

async fn stored(db: impl PgExecutor<'_>, user: Uuid) -> Result<Option<Stored>, ApiError> {
    let row = sqlx::query(
        "SELECT kind, settings, secret IS NOT NULL AS has_secret, public_only
         FROM user_contacts WHERE user_id = $1 FOR UPDATE",
    )
    .bind(user)
    .fetch_optional(db)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let chat = match row.try_get::<Option<String>, _>("kind")? {
        Some(kind) => Destination::from_row(&kind, row.try_get("settings")?).ok(),
        None => None,
    };
    Ok(Some(Stored {
        chat,
        has_secret: row.try_get("has_secret")?,
        public_only: row.try_get("public_only")?,
    }))
}

async fn contact_view(db: impl PgExecutor<'_>, user: Uuid) -> Result<ContactView, ApiError> {
    let row = sqlx::query(
        "SELECT email, kind, settings, secret IS NOT NULL AS has_secret, last_sent_at,
                last_error, last_error_at
         FROM user_contacts WHERE user_id = $1",
    )
    .bind(user)
    .fetch_optional(db)
    .await?;
    let Some(row) = row else {
        return Ok(ContactView::default());
    };
    Ok(ContactView {
        email: row.try_get("email")?,
        kind: row.try_get("kind")?,
        settings: row.try_get("settings")?,
        has_secret: row.try_get("has_secret")?,
        last_sent_at: row.try_get("last_sent_at")?,
        last_error: row.try_get("last_error")?,
        last_error_at: row.try_get("last_error_at")?,
    })
}

/// `GET /api/on-call/contact`: the signed-in user's own contact.
pub async fn contact(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<ContactView>, ApiError> {
    Ok(Json(contact_view(&state.pool, user.id).await?))
}

/// `PUT /api/on-call/contact`: replaces the signed-in user's contact. A
/// chat or webhook follows the channels' rules (ADR-0077, ADR-0078): its
/// secret is never read back, stays only with its target, and someone
/// other than a global administrator reaches only public addresses.
/// Audited as `user.contact_update`.
pub async fn set_contact(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<ContactRequest>, JsonRejection>,
) -> Result<Json<ContactView>, ApiError> {
    let request = body(request)?;
    let email = match request.email.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(address) => {
            address
                .parse::<Mailbox>()
                .map_err(|_| ApiError::Invalid(format!("`{address}` is not an email address.")))?;
            if address.len() > 254 {
                return Err(ApiError::Invalid("That address is too long.".into()));
            }
            Some(address.to_owned())
        }
    };
    let mut tx = state.pool.begin().await?;
    let current = stored(&mut *tx, user.id).await?;
    let current_chat = current.as_ref().and_then(|c| c.chat.as_ref());
    let chat = match request.kind.as_deref() {
        None | Some("") => None,
        Some("email") => {
            return Err(ApiError::Invalid(
                "Put the email address in its own field.".into(),
            ))
        }
        Some(kind) => {
            let settings = request.settings.unwrap_or_else(|| json!({}));
            Some(checked(kind, settings, request.secret, current_chat)?)
        }
    };
    let global = is_global_admin(&user);
    let current_secret = current.as_ref().is_some_and(|c| c.has_secret);
    let current_public = current.as_ref().is_none_or(|c| c.public_only);
    // (kind, settings, secret, keep the stored secret, public only)
    let (kind, settings, secret, keep, public_only) = match &chat {
        None => (None, None, None, false, !global),
        Some((destination, secret)) => {
            let new_chat_url = destination.chat().is_some() && secret.is_some();
            let same_target =
                current_chat.is_some_and(|c| c.same_target(destination)) && !new_chat_url;
            if !same_target && current_secret && secret.is_none() {
                return Err(ApiError::Invalid(
                    "Enter the webhook URL or signing secret again: a saved one stays with the address it was saved for."
                        .into(),
                ));
            }
            (
                Some(destination.kind()),
                Some(destination.settings()),
                secret.clone(),
                same_target && secret.is_none(),
                !global && (current_public || !same_target),
            )
        }
    };
    let host = chat.as_ref().and_then(|(d, _)| match d {
        Destination::Webhook(hook) => reqwest::Url::parse(&hook.url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned)),
        other => other.chat().map(|(_, c)| c.host.clone()),
    });
    if email.is_none() && chat.is_none() {
        sqlx::query("DELETE FROM user_contacts WHERE user_id = $1")
            .bind(user.id)
            .execute(&mut *tx)
            .await?;
    } else {
        sqlx::query(
            "INSERT INTO user_contacts (user_id, email, kind, settings, secret, public_only)
             VALUES ($1, $2, $3, $4, $5, $7)
             ON CONFLICT (user_id) DO UPDATE
             SET email = EXCLUDED.email, kind = EXCLUDED.kind, settings = EXCLUDED.settings,
                 secret = CASE WHEN $6 THEN user_contacts.secret ELSE EXCLUDED.secret END,
                 public_only = EXCLUDED.public_only, updated_at = now()",
        )
        .bind(user.id)
        .bind(&email)
        .bind(kind)
        .bind(settings)
        .bind(secret.as_deref())
        .bind(keep)
        .bind(public_only)
        .execute(&mut *tx)
        .await?;
    }
    let details = json!({
        "email": email, "kind": kind, "host": host, "secret_set": secret.is_some(),
    });
    record(
        &mut *tx,
        &user,
        "user.contact_update",
        ("user", user.id),
        details,
    )
    .await?;
    let view = contact_view(&mut *tx, user.id).await?;
    tx.commit().await?;
    Ok(Json(view))
}

/// `POST /api/on-call/contact/test`: sends a test to each of the signed-in
/// user's destinations, email through the mail server their alerts would
/// use, and answers each outcome in plain words.
pub async fn test_contact(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<ContactTests>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let directory = Directory::load(&mut conn).await?;
    let person = directory
        .people
        .get(&user.id)
        .ok_or(ApiError::Unauthorized)?;
    if person.email.is_none() && person.chat.is_none() {
        return Err(ApiError::Invalid(
            "Add an email address or a chat first.".into(),
        ));
    }
    let channels = notify::channels(&mut *conn).await?;
    drop(conn);
    let mail = person
        .grants
        .iter()
        .filter_map(|(_, customer, site)| Coverage::from_columns(*customer, *site))
        .filter_map(|coverage| mail_channel(&channels, &directory, coverage))
        .max_by_key(|c| on_call::narrowness(c.coverage));
    let dispatcher = Dispatcher::new(state.pool.clone(), state.settings.web_url.clone());
    let mut results = Vec::new();
    if person.email.is_some() && mail.is_none() {
        results.push(ContactTest {
            kind: "email".into(),
            ok: false,
            error: Some(
                "No email channel can send it yet. An administrator adds one under Notifications; its mail server also writes to people on call."
                    .into(),
            ),
        });
    }
    for channel in on_call::contact_channels(person, mail) {
        let result = dispatcher.test(&channel).await?;
        results.push(ContactTest {
            kind: channel.destination.kind().into(),
            ok: result.is_ok(),
            error: result.err().map(|e| e.message),
        });
    }
    tracing::info!(user = %user.id, "tested a personal contact");
    Ok(Json(ContactTests { results }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::on_call::{Override, Person};

    fn utc(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    #[test]
    fn the_view_names_now_next_and_gaps() {
        let (alice, bob, gone) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        let customer = Uuid::from_u128(10);
        let person = |id, name: &str| Person {
            id,
            username: name.into(),
            disabled: false,
            grants: vec![(false, Some(customer), None)],
            email: None,
            chat: Some((
                Destination::Webhook(crate::notify::Webhook {
                    url: "https://h/".into(),
                }),
                None,
                true,
            )),
        };
        let directory = Directory {
            people: [(alice, person(alice, "alice")), (bob, person(bob, "bob"))].into(),
            site_customer: HashMap::new(),
        };
        let schedule = Schedule {
            id: Uuid::from_u128(9),
            coverage: Coverage::Customer(customer),
            rota: Rota {
                time_zone: chrono_tz::Europe::Paris,
                rotation: Rotation::Weekly,
                handoff: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
                anchor: "2026-11-02".parse().unwrap(),
                anchor_position: 0,
                members: vec![Some(alice), Some(bob), None],
            },
            overrides: vec![Override {
                id: Uuid::from_u128(50),
                user: gone,
                starts_at: utc("2026-11-20T00:00:00Z"),
                ends_at: utc("2026-11-21T00:00:00Z"),
                created_at: utc("2026-11-01T00:00:00Z"),
            }],
        };
        let names = Names {
            name: "Ops".into(),
            customer_id: Some(customer),
            customer_name: Some("acme".into()),
            site_name: None,
        };
        let now = utc("2026-11-03T12:00:00Z");
        let view = view(&schedule, &names, &directory, &[], now, true);
        let now_turn = view.now.unwrap();
        assert_eq!(now_turn.username.as_deref(), Some("alice"));
        assert_eq!(now_turn.starts_at, utc("2026-11-02T08:00:00Z"));
        let next = view.next.unwrap();
        assert_eq!(
            (next.username.as_deref(), next.starts_at),
            (Some("bob"), utc("2026-11-09T08:00:00Z"))
        );
        assert_eq!(view.handoff_day, Some(1));
        assert_eq!(view.handoff_time, "09:00");
        // The removed member's week and the override of someone gone are
        // gaps, shown ahead of time.
        let gaps: Vec<_> = view
            .turns
            .iter()
            .filter(|t| t.problem.is_some())
            .map(|t| (t.starts_at, t.problem.unwrap()))
            .collect();
        assert_eq!(
            gaps,
            vec![
                (utc("2026-11-16T08:00:00Z"), Problem::Removed),
                (utc("2026-11-20T00:00:00Z"), Problem::Removed),
                (utc("2026-11-21T00:00:00Z"), Problem::Removed),
            ]
        );
        assert_eq!(view.members[2].problem, Some(Problem::Removed));
        assert_eq!(
            view.turns.last().unwrap().ends_at,
            now + TimeDelta::days(HORIZON_DAYS)
        );
        // No administrator can be told: nobody but the channels.
        assert!(view.fallback.is_empty() && view.mail_host.is_none());
    }
}
