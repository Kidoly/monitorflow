//! Report schedules (ADR-0155): an availability report emailed each month
//! or week, for one customer or for everything, through an email channel.
//!
//! Schedules are managed by those who manage the channels they send
//! through (ADR-0078): administrators of the whole customer, and global
//! administrators for everything. Others do not see them.

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgConnection;
use uuid::Uuid;

use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::notifications::{is_global_admin, manages};
use super::sensors::{body, parse_id};
use super::{ApiError, ApiState};
use crate::availability::schedule::{latest, next, send_time, Frequency};
use crate::availability::sender::{self, Stored};
use crate::availability::words::Language;
use crate::notify::{self, Destination};

/// Most recipients of one schedule.
pub const MAX_RECIPIENTS: usize = 20;

/// Body of `POST /api/report-schedules` and `PUT
/// /api/report-schedules/{id}`; a change replaces every field.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[schema(as = ReportScheduleRequest)]
#[serde(deny_unknown_fields)]
pub struct ScheduleRequest {
    /// One customer's report; absent or `null` for everything.
    #[serde(default)]
    pub customer_id: Option<Uuid>,
    /// `monthly` (the default) or `weekly`.
    #[serde(default = "monthly")]
    pub frequency: String,
    /// IANA time zone of the periods and of the times in the report, like
    /// `Europe/Paris`.
    pub time_zone: String,
    /// `en` (the default) or `fr`.
    #[serde(default = "english")]
    pub language: String,
    /// Attach the PDF; true by default.
    #[serde(default = "yes")]
    pub pdf: bool,
    /// Attach the CSV; true by default.
    #[serde(default = "yes")]
    pub csv: bool,
    /// An email channel covering the customer, or everything.
    pub channel_id: Uuid,
    /// Who gets it, at most 20 addresses; empty or absent for the channel's
    /// own recipients.
    #[serde(default)]
    pub recipients: Vec<String>,
    /// True by default; false keeps it without sending.
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn monthly() -> String {
    "monthly".into()
}

fn english() -> String {
    "en".into()
}

fn yes() -> bool {
    true
}

/// One schedule.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = ReportSchedule)]
pub struct ScheduleView {
    pub id: Uuid,
    pub customer_id: Option<Uuid>,
    /// `null` for everything.
    pub customer_name: Option<String>,
    pub frequency: String,
    pub time_zone: String,
    pub language: String,
    pub pdf: bool,
    pub csv: bool,
    pub channel_id: Uuid,
    /// Where the channel sends, like `smtp.example.com`.
    pub channel_host: String,
    pub recipients: Vec<String>,
    /// The channel's own, used when `recipients` is empty.
    pub channel_recipients: Vec<String>,
    pub enabled: bool,
    /// The last period sent, and when.
    pub last_period_end: Option<DateTime<Utc>>,
    pub last_sent_at: Option<DateTime<Utc>>,
    /// Why the last sending failed, in plain words; `null` once one works.
    pub last_error: Option<String>,
    pub last_error_at: Option<DateTime<Utc>>,
    /// The next report's period and when it goes out.
    pub next_from: DateTime<Utc>,
    pub next_to: DateTime<Utc>,
    pub next_send_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

/// Body of `GET /api/report-schedules`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = ReportScheduleList)]
pub struct ScheduleList {
    pub schedules: Vec<ScheduleView>,
}

/// Body of `POST /api/report-schedules/{id}/send`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = ReportSent)]
pub struct SentView {
    /// Whether the email went through.
    pub ok: bool,
    /// Why not, in plain words; `null` when it went through.
    pub error: Option<String>,
    /// The period of the report.
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    /// Who it was sent to; empty when it failed.
    pub recipients: Vec<String>,
}

/// Whether `user` manages what a schedule of `customer` covers.
fn administers(user: &CurrentUser, customer: Option<Uuid>) -> bool {
    match customer {
        None => is_global_admin(user),
        Some(id) => user.administered_customers().contains(id),
    }
}

struct Valid {
    frequency: Frequency,
    zone: Tz,
    language: Language,
    recipients: Vec<String>,
}

/// What `request` says, before looking at the database.
fn shape(request: &ScheduleRequest) -> Result<Valid, String> {
    let frequency = Frequency::parse(&request.frequency).ok_or("Send it `monthly` or `weekly`.")?;
    let zone: Tz = request.time_zone.parse().map_err(|_| {
        format!(
            "{} is not a time zone, like Europe/Paris.",
            request.time_zone
        )
    })?;
    let language = Language::parse(&request.language).ok_or("The language is `en` or `fr`.")?;
    if !request.pdf && !request.csv {
        return Err("Attach the PDF, the CSV or both.".into());
    }
    let mut recipients: Vec<String> = Vec::new();
    for address in &request.recipients {
        let address = address.trim();
        if address.parse::<lettre::message::Mailbox>().is_err() || address.contains(['<', '>']) {
            return Err(format!("`{address}` is not an email address."));
        }
        if !recipients.iter().any(|r| r.eq_ignore_ascii_case(address)) {
            recipients.push(address.to_owned());
        }
    }
    if recipients.len() > MAX_RECIPIENTS {
        return Err(format!("Send it to at most {MAX_RECIPIENTS} addresses."));
    }
    Ok(Valid {
        frequency,
        zone,
        language,
        recipients,
    })
}

async fn validate(
    db: &mut PgConnection,
    user: &CurrentUser,
    request: &ScheduleRequest,
) -> Result<Valid, ApiError> {
    if !user.is_administrator() {
        return Err(ApiError::Forbidden);
    }
    if let Some(customer) = request.customer_id {
        let found: Option<Uuid> = sqlx::query_scalar("SELECT id FROM customers WHERE id = $1")
            .bind(customer)
            .fetch_optional(&mut *db)
            .await?;
        if found.is_none() || !user.visible_customers().contains(customer) {
            return Err(ApiError::NotFound);
        }
    }
    if !administers(user, request.customer_id) {
        return Err(ApiError::Forbidden);
    }
    let valid = shape(request).map_err(ApiError::Invalid)?;
    let channel = notify::load(&mut *db, request.channel_id)
        .await?
        .filter(|c| manages(user, c.coverage))
        .ok_or_else(|| ApiError::Invalid("Choose an email channel you manage.".into()))?;
    if !matches!(channel.destination, Destination::Email(_)) {
        return Err(ApiError::Invalid("Choose an email channel.".into()));
    }
    if !sender::channel_fits(channel.coverage, request.customer_id) {
        return Err(ApiError::Invalid(
            "Choose an email channel covering this customer as a whole, or everything.".into(),
        ));
    }
    Ok(valid)
}

type Row = (
    Uuid,
    Option<Uuid>,
    Option<String>,
    String,
    String,
    String,
    bool,
    bool,
    Uuid,
    Vec<String>,
    bool,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    Option<String>,
    Option<DateTime<Utc>>,
    DateTime<Utc>,
);

const SELECT: &str = "SELECT r.id, r.customer_id, c.name, r.frequency, r.time_zone, r.language,
        r.pdf, r.csv, r.channel_id, r.recipients, r.enabled, r.last_period_end, r.last_sent_at,
        r.last_error, r.last_error_at, r.created_at
    FROM report_schedules r LEFT JOIN customers c ON c.id = r.customer_id";

async fn view(db: &mut PgConnection, row: Row) -> Result<ScheduleView, ApiError> {
    let channel = notify::load(&mut *db, row.8).await?;
    let (host, to) = match channel.map(|c| c.destination) {
        Some(Destination::Email(email)) => (email.host, email.to),
        _ => (String::new(), Vec::new()),
    };
    let zone: Tz = row.4.parse().unwrap_or(Tz::UTC);
    let frequency = Frequency::parse(&row.3).unwrap_or(Frequency::Monthly);
    let next = next(frequency, zone, row.11, Utc::now());
    Ok(ScheduleView {
        id: row.0,
        customer_id: row.1,
        customer_name: row.2,
        frequency: row.3,
        time_zone: row.4,
        language: row.5,
        pdf: row.6,
        csv: row.7,
        channel_id: row.8,
        channel_host: host,
        recipients: row.9,
        channel_recipients: to,
        enabled: row.10,
        last_period_end: row.11,
        last_sent_at: row.12,
        last_error: row.13,
        last_error_at: row.14,
        next_from: next.0,
        next_to: next.1,
        next_send_at: send_time(next.1, zone),
        created_at: row.15,
    })
}

async fn one(db: &mut PgConnection, id: Uuid) -> Result<ScheduleView, ApiError> {
    // SQL-safe: interpolates only the constant SELECT.
    let row: Row = sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} WHERE r.id = $1")))
        .bind(id)
        .fetch_optional(&mut *db)
        .await?
        .ok_or(ApiError::NotFound)?;
    view(db, row).await
}

/// `GET /api/report-schedules`: the schedules the user manages.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<ScheduleList>, ApiError> {
    if !user.is_administrator() {
        return Err(ApiError::Forbidden);
    }
    let (all, customers) = user.administered_customers().bind();
    let mut conn = state.pool.acquire().await?;
    // SQL-safe: interpolates only the constant SELECT.
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SELECT} WHERE $1 OR r.customer_id = ANY($2) ORDER BY c.name NULLS FIRST, r.created_at"
    )))
    .bind(all)
    .bind(&customers)
    .fetch_all(&mut *conn)
    .await?;
    let mut schedules = Vec::with_capacity(rows.len());
    for row in rows {
        schedules.push(view(&mut conn, row).await?);
    }
    Ok(Json(ScheduleList { schedules }))
}

async fn store(
    db: &mut PgConnection,
    id: Uuid,
    request: &ScheduleRequest,
    valid: &Valid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE report_schedules SET customer_id = $2, frequency = $3, time_zone = $4,
                language = $5, pdf = $6, csv = $7, channel_id = $8, recipients = $9,
                enabled = $10, retry_at = NULL, updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(request.customer_id)
    .bind(valid.frequency.as_str())
    .bind(valid.zone.name())
    .bind(valid.language.code())
    .bind(request.pdf)
    .bind(request.csv)
    .bind(request.channel_id)
    .bind(&valid.recipients)
    .bind(request.enabled)
    .execute(&mut *db)
    .await
    .map(drop)
}

fn audited(request: &ScheduleRequest, valid: &Valid) -> serde_json::Value {
    json!({
        "customer_id": request.customer_id,
        "frequency": valid.frequency.as_str(),
        "time_zone": valid.zone.name(),
        "language": valid.language.code(),
        "pdf": request.pdf,
        "csv": request.csv,
        "channel_id": request.channel_id,
        "recipients": valid.recipients,
        "enabled": request.enabled,
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
            target: Some(("report_schedule", &id.to_string())),
            details,
        },
    )
    .await
}

/// `POST /api/report-schedules`. A new schedule sends from the next
/// period: the one that ended already is not sent unless asked.
pub async fn create(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<ScheduleRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<ScheduleView>), ApiError> {
    let request = body(request)?;
    let mut tx = state.pool.begin().await?;
    let valid = validate(&mut tx, &user, &request).await?;
    let ended = latest(valid.frequency, valid.zone, Utc::now()).1;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO report_schedules (time_zone, channel_id, created_by, last_period_end)
         VALUES ('UTC', $1, $2, $3) RETURNING id",
    )
    .bind(request.channel_id)
    .bind(user.id)
    .bind(ended)
    .fetch_one(&mut *tx)
    .await?;
    store(&mut tx, id, &request, &valid).await?;
    record(
        &mut tx,
        &user,
        "report_schedule.create",
        id,
        audited(&request, &valid),
    )
    .await?;
    let view = one(&mut tx, id).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(view)))
}

/// The schedule `id` if the user manages it, else 404.
async fn managed(db: &mut PgConnection, user: &CurrentUser, id: &str) -> Result<Stored, ApiError> {
    if !user.is_administrator() {
        return Err(ApiError::Forbidden);
    }
    let id = parse_id(id)?;
    // SQL-safe: interpolates only the constant SELECT.
    let stored: Option<Stored> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{} WHERE id = $1",
        sender::SELECT
    )))
    .bind(id)
    .fetch_optional(&mut *db)
    .await?;
    stored
        .filter(|s| administers(user, s.customer_id))
        .ok_or(ApiError::NotFound)
}

/// `PUT /api/report-schedules/{id}`.
pub async fn update(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<ScheduleRequest>, JsonRejection>,
) -> Result<Json<ScheduleView>, ApiError> {
    let request = body(request)?;
    let mut tx = state.pool.begin().await?;
    let before = managed(&mut tx, &user, &id).await?;
    let valid = validate(&mut tx, &user, &request).await?;
    store(&mut tx, before.id, &request, &valid).await?;
    record(
        &mut tx,
        &user,
        "report_schedule.update",
        before.id,
        audited(&request, &valid),
    )
    .await?;
    let view = one(&mut tx, before.id).await?;
    tx.commit().await?;
    Ok(Json(view))
}

/// `DELETE /api/report-schedules/{id}`.
pub async fn remove(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let mut tx = state.pool.begin().await?;
    let schedule = managed(&mut tx, &user, &id).await?;
    sqlx::query("DELETE FROM report_schedules WHERE id = $1")
        .bind(schedule.id)
        .execute(&mut *tx)
        .await?;
    record(
        &mut tx,
        &user,
        "report_schedule.remove",
        schedule.id,
        json!({"customer_id": schedule.customer_id}),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/report-schedules/{id}/send`: the latest period now, to check
/// the setup. The schedule still sends its next one as planned.
pub async fn send_now(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<SentView>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let schedule = managed(&mut conn, &user, &id).await?;
    drop(conn);
    let period = latest(schedule.frequency(), schedule.zone(), Utc::now());
    let actor = user.id.to_string();
    let sent = sender::send(&state.pool, &schedule, period, true, ("user", &actor)).await;
    let (error, recipients) = match sent {
        Ok(sent) => (None, sent.recipients),
        Err(error) => (Some(error), Vec::new()),
    };
    Ok(Json(SentView {
        ok: error.is_none(),
        error,
        from: period.0,
        to: period.1,
        recipients,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> ScheduleRequest {
        ScheduleRequest {
            customer_id: None,
            frequency: "monthly".into(),
            time_zone: "Europe/Paris".into(),
            language: "fr".into(),
            pdf: true,
            csv: true,
            channel_id: Uuid::nil(),
            recipients: vec![" boss@example.com".into(), "Boss@example.com".into()],
            enabled: true,
        }
    }

    #[test]
    fn a_sensible_request() {
        let valid = shape(&request()).unwrap();
        assert_eq!(valid.recipients, ["boss@example.com"]);
        assert_eq!(valid.language, Language::Fr);
    }

    #[test]
    fn requests_that_say_nothing_sensible() {
        for bad in [
            ScheduleRequest {
                frequency: "daily".into(),
                ..request()
            },
            ScheduleRequest {
                time_zone: "Paris".into(),
                ..request()
            },
            ScheduleRequest {
                language: "de".into(),
                ..request()
            },
            ScheduleRequest {
                pdf: false,
                csv: false,
                ..request()
            },
            ScheduleRequest {
                recipients: vec!["not an address".into()],
                ..request()
            },
            ScheduleRequest {
                recipients: (0..21).map(|i| format!("p{i}@example.com")).collect(),
                ..request()
            },
        ] {
            assert!(shape(&bad).is_err(), "{:?}", bad.frequency);
        }
    }
}
