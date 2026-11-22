//! Availability reports (ADR-0155): the availability of the services,
//! servers, devices and checks a user reads, over a period, as JSON, CSV
//! or PDF.
//!
//! A report reads only the user's sites, and with `customer_id` only that
//! customer's; business services only of customers granted as a whole.

use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::{header, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Datelike, TimeDelta, TimeZone, Utc};
use chrono_tz::Tz;
use serde::Deserialize;
use uuid::Uuid;

use super::auth::CurrentUser;
use super::{ApiError, ApiState};
use crate::availability::data::{load, Reach};
use crate::availability::report::{build, Report};
use crate::availability::sender::file_name;
use crate::availability::uptime::{self, Uptime};
use crate::availability::words::Language;
use crate::availability::{csv, pdf, Span};

/// The shortest period a report covers.
pub const SHORTEST: TimeDelta = TimeDelta::hours(1);
/// The longest: 13 months and a little.
pub const LONGEST: TimeDelta = TimeDelta::days(400);

/// Query string of `GET /api/availability`.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct Params {
    /// Start of the period, RFC 3339; the start of this month (UTC) when
    /// absent.
    pub from: Option<DateTime<Utc>>,
    /// End of the period, excluded; now when absent. A period still going
    /// on counts until now.
    pub to: Option<DateTime<Utc>>,
    /// Only this customer's sites and services.
    pub customer_id: Option<Uuid>,
}

/// Query string of `GET /api/availability/report`.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct FileParams {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub customer_id: Option<Uuid>,
    /// `csv` or `pdf`.
    pub format: String,
    /// `en` (the default) or `fr`.
    pub language: Option<String>,
    /// IANA time zone the times are written in, like `Europe/Paris`; UTC
    /// when absent.
    pub time_zone: Option<String>,
}

/// Query string of `GET /api/availability/uptime`.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct UptimeParams {
    /// Only this customer's sites and services.
    pub customer_id: Option<Uuid>,
    /// The instant the windows end at, RFC 3339; now when absent or in
    /// the future.
    pub at: Option<DateTime<Utc>>,
    /// IANA time zone days are cut in, like `Europe/Paris`; UTC when
    /// absent.
    pub time_zone: Option<String>,
}

/// The period of `from` and `to` at `now`, or why not.
pub fn period(
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<Span, String> {
    let from = from.unwrap_or_else(|| {
        Utc.with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
            .single()
            .unwrap_or(now)
    });
    let to = to.unwrap_or(now);
    if to - from < SHORTEST {
        return Err("The period ends at least an hour after it starts.".into());
    }
    if to - from > LONGEST {
        return Err("A report covers at most 13 months.".into());
    }
    if from >= now {
        return Err("The period starts in the past.".into());
    }
    Ok((from, to))
}

/// What `user` may read for `customer` or everything, and the customer's
/// name: 404 for a customer outside their reach.
async fn reach(
    conn: &mut sqlx::PgConnection,
    user: &CurrentUser,
    customer: Option<Uuid>,
) -> Result<(Reach, Option<String>), ApiError> {
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let readable = user.readable_sites();
    let whole = user.visible_customers();
    let (reach, name) = match customer {
        None => {
            let sites = match &readable {
                super::access::Scope::All => None,
                super::access::Scope::Only(ids) => Some(ids.clone()),
            };
            let services_of = match &whole {
                super::access::Scope::All => None,
                super::access::Scope::Only(ids) => Some(ids.clone()),
            };
            (Reach { sites, services_of }, None)
        }
        Some(customer) => {
            let row: Option<String> =
                sqlx::query_scalar("SELECT name FROM customers WHERE id = $1")
                    .bind(customer)
                    .fetch_optional(&mut *conn)
                    .await?;
            let sites: Vec<Uuid> =
                sqlx::query_scalar("SELECT id FROM sites WHERE customer_id = $1")
                    .bind(customer)
                    .fetch_all(&mut *conn)
                    .await?;
            let sites: Vec<Uuid> = sites
                .into_iter()
                .filter(|s| readable.contains(*s))
                .collect();
            let name = match row {
                Some(name) if whole.contains(customer) || !sites.is_empty() => name,
                _ => return Err(ApiError::NotFound),
            };
            let services_of = if whole.contains(customer) {
                vec![customer]
            } else {
                vec![]
            };
            (
                Reach {
                    sites: Some(sites),
                    services_of: Some(services_of),
                },
                Some(name),
            )
        }
    };
    Ok((reach, name))
}

/// The report `user` may read over `period`, for `customer` or everything.
async fn report(
    state: &ApiState,
    user: &CurrentUser,
    period: Span,
    customer: Option<Uuid>,
) -> Result<Report, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (reach, name) = reach(&mut conn, user, customer).await?;
    let now = Utc::now();
    let data = load(&mut conn, &reach, period, now).await?;
    let mut report = build(&data, period, now);
    report.customer_id = customer;
    report.customer_name = name;
    Ok(report)
}

/// `GET /api/availability`.
pub async fn get(
    State(state): State<ApiState>,
    user: CurrentUser,
    params: Result<Query<Params>, QueryRejection>,
) -> Result<Json<Report>, ApiError> {
    let Query(params) = params.map_err(|r| ApiError::Invalid(r.body_text()))?;
    let period = period(params.from, params.to, Utc::now()).map_err(ApiError::Invalid)?;
    Ok(Json(
        report(&state, &user, period, params.customer_id).await?,
    ))
}

/// `GET /api/availability/uptime`: the last 24 hours, 7, 30 and 90 days
/// and 90 daily bars of what `user` reads (ADR-0172).
pub async fn uptime(
    State(state): State<ApiState>,
    user: CurrentUser,
    params: Result<Query<UptimeParams>, QueryRejection>,
) -> Result<Json<Uptime>, ApiError> {
    let Query(params) = params.map_err(|r| ApiError::Invalid(r.body_text()))?;
    let (_, zone) = language_and_zone(None, params.time_zone.as_deref())?;
    let now = Utc::now();
    let at = params.at.map_or(now, |at| at.min(now));
    let mut conn = state.pool.acquire().await?;
    let (reach, _) = reach(&mut conn, &user, params.customer_id).await?;
    let data = load(&mut conn, &reach, uptime::period(at), now).await?;
    Ok(Json(uptime::build(&data, at, zone, now)))
}

/// `report` as a `csv` or `pdf` file; the PDF is made off the runtime.
pub async fn render(
    report: &Report,
    extension: &str,
    zone: Tz,
    language: Language,
) -> Result<Vec<u8>, ApiError> {
    if extension == "csv" {
        return Ok(csv::render(report, zone, language).into_bytes());
    }
    let report = report.clone();
    tokio::task::spawn_blocking(move || pdf::render(&report, zone, language))
        .await
        .map_err(|e| ApiError::Internal(format!("making the PDF: {e}")))
}

/// The language and zone of a file request, or why not.
pub fn language_and_zone(
    language: Option<&str>,
    zone: Option<&str>,
) -> Result<(Language, Tz), ApiError> {
    let language = Language::parse(language.unwrap_or("en"))
        .ok_or_else(|| ApiError::Invalid("The language is `en` or `fr`.".into()))?;
    let zone = match zone {
        None => Tz::UTC,
        Some(name) => name.parse().map_err(|_| {
            ApiError::Invalid(format!("{name} is not a time zone, like Europe/Paris."))
        })?,
    };
    Ok((language, zone))
}

/// `GET /api/availability/report`: the report as a file to download.
pub async fn file(
    State(state): State<ApiState>,
    user: CurrentUser,
    params: Result<Query<FileParams>, QueryRejection>,
) -> Result<Response, ApiError> {
    let Query(params) = params.map_err(|r| ApiError::Invalid(r.body_text()))?;
    let (language, zone) =
        language_and_zone(params.language.as_deref(), params.time_zone.as_deref())?;
    let period = period(params.from, params.to, Utc::now()).map_err(ApiError::Invalid)?;
    let (content_type, extension) = match params.format.as_str() {
        "csv" => ("text/csv; charset=utf-8", "csv"),
        "pdf" => ("application/pdf", "pdf"),
        _ => return Err(ApiError::Invalid("The format is `csv` or `pdf`.".into())),
    };
    let report = report(&state, &user, period, params.customer_id).await?;
    let body = render(&report, extension, zone, language).await?;
    let name = file_name(&report, zone, extension);
    let mut response = body.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{name}\""))
            .map_err(|e| ApiError::Internal(format!("file name: {e}")))?,
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text).unwrap().to_utc()
    }

    #[test]
    fn periods() {
        let now = at("2026-11-18T10:00:00Z");
        assert_eq!(
            period(None, None, now),
            Ok((at("2026-11-01T00:00:00Z"), now))
        );
        let october = (
            Some(at("2026-10-01T00:00:00Z")),
            Some(at("2026-11-01T00:00:00Z")),
        );
        assert!(period(october.0, october.1, now).is_ok());
        assert!(period(Some(now), Some(now + TimeDelta::minutes(30)), now).is_err());
        assert!(period(Some(now - TimeDelta::days(500)), Some(now), now).is_err());
        assert!(period(
            Some(now + TimeDelta::hours(1)),
            Some(now + TimeDelta::hours(3)),
            now
        )
        .is_err());
    }

    #[test]
    fn languages_and_zones() {
        assert!(language_and_zone(None, None).is_ok());
        assert!(language_and_zone(Some("de"), None).is_err());
        assert!(language_and_zone(Some("fr"), Some("Paris")).is_err());
        let (language, zone) = language_and_zone(Some("fr"), Some("Europe/Paris")).unwrap();
        assert_eq!((language, zone), (Language::Fr, chrono_tz::Europe::Paris));
    }
}
