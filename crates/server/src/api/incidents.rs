//! Incident endpoints (ADR-0017, ADR-0019): read-only views of what the
//! threshold engine opened and resolved.
//!
//! The engine owns `incidents` and `incident_events`; nothing here writes
//! them. Every role reads the incidents of the servers of its sites, and
//! an incident outside them is not found.

use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::Json;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::access::authorize;
use super::auth::CurrentUser;
use super::sensors::parse_id;
use super::{ApiError, ApiState};

/// Page size without `limit`.
pub const DEFAULT_LIMIT: u32 = 100;
/// Largest accepted `limit`.
pub const MAX_LIMIT: u32 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StateFilter {
    Open,
    Resolved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SeverityFilter {
    Warning,
    Critical,
}

/// Query string of `GET /api/incidents`. Every filter is optional.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListParams {
    pub state: Option<StateFilter>,
    /// The incident's current severity.
    pub severity: Option<SeverityFilter>,
    pub server_id: Option<Uuid>,
    pub site_id: Option<Uuid>,
    /// Incidents still open at or resolved after this time.
    pub from: Option<DateTime<Utc>>,
    /// Incidents opened at or before this time.
    pub to: Option<DateTime<Utc>>,
    /// 1 to [`MAX_LIMIT`], default [`DEFAULT_LIMIT`].
    pub limit: Option<u32>,
    /// `next_cursor` of the previous page.
    pub cursor: Option<String>,
}

/// One incident. Field names and types are part of the API contract: add
/// fields, never rename or remove them.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct IncidentView {
    pub id: Uuid,
    pub server_id: Uuid,
    pub hostname: String,
    pub site_id: Uuid,
    pub site_name: String,
    pub metric: String,
    /// `""` for a series without an instance.
    pub instance: String,
    /// `warning` or `critical`; the last one while resolved.
    pub severity: String,
    /// `open` or `resolved`.
    pub state: String,
    /// Sample times, not the server clock.
    pub opened_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub last_value: f64,
    pub last_value_at: DateTime<Utc>,
    /// The worst value seen while open.
    pub peak_value: f64,
    /// The threshold as it was when the incident opened.
    pub threshold: Value,
}

/// Body of `GET /api/incidents`.
#[derive(Debug, Serialize)]
pub struct IncidentList {
    pub incidents: Vec<IncidentView>,
    /// Pass as `cursor` for the next page; `null` on the last one.
    pub next_cursor: Option<String>,
}

/// One change of an incident.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct EventView {
    pub id: i64,
    /// `opened`, `escalated`, `deescalated` or `resolved`.
    pub kind: String,
    /// Severity after the change; `null` once resolved.
    pub severity: Option<String>,
    pub value: Option<f64>,
    /// Sample time of the change, or the server clock without a sample.
    pub occurred_at: DateTime<Utc>,
    pub recorded_at: DateTime<Utc>,
    /// For instance `{"reason": "threshold_removed"}`.
    pub details: Value,
}

/// Body of `GET /api/incidents/{id}`.
#[derive(Debug, Serialize)]
pub struct IncidentDetail {
    #[serde(flatten)]
    pub incident: IncidentView,
    /// In the order they happened.
    pub events: Vec<EventView>,
}

/// The sort key `(open, opened_at, id)` of a page's last incident.
type Cursor = (bool, DateTime<Utc>, Uuid);

fn encode_cursor(key: Cursor) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(&key).unwrap_or_default())
}

fn decode_cursor(cursor: &str) -> Result<Cursor, ApiError> {
    URL_SAFE_NO_PAD
        .decode(cursor)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or_else(|| ApiError::Invalid("invalid cursor".into()))
}

const SELECT: &str = "SELECT i.id, i.server_id, s.hostname, s.site_id, si.name AS site_name,
                             i.metric, i.instance, i.severity, i.state, i.opened_at,
                             i.resolved_at, i.last_value, i.last_value_at, i.peak_value,
                             i.threshold
                      FROM incidents i
                      JOIN servers s ON s.id = i.server_id
                      JOIN sites si ON si.id = s.site_id";

/// `GET /api/incidents`: one page of the incidents of the user's sites,
/// open ones first, then newest first.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
    params: Result<Query<ListParams>, QueryRejection>,
) -> Result<Json<IncidentList>, ApiError> {
    let Query(params) = params.map_err(|r| ApiError::Invalid(r.body_text()))?;
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let (all_sites, sites) = user.readable_sites().bind();
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(ApiError::Invalid(format!(
            "limit must be between 1 and {MAX_LIMIT}"
        )));
    }
    if let (Some(from), Some(to)) = (params.from, params.to) {
        if from > to {
            return Err(ApiError::Invalid("from must not be after to".into()));
        }
    }
    let after = params.cursor.as_deref().map(decode_cursor).transpose()?;
    let state_filter = params.state.map(|s| match s {
        StateFilter::Open => "open",
        StateFilter::Resolved => "resolved",
    });
    let severity = params.severity.map(|s| match s {
        SeverityFilter::Warning => "warning",
        SeverityFilter::Critical => "critical",
    });
    // A server or site outside the user's scope matches nothing, as if it
    // had no incidents.
    let mut rows: Vec<IncidentView> = sqlx::query_as(&format!(
        "{SELECT}
         WHERE ($1 OR s.site_id = ANY($2))
           AND ($3::text IS NULL OR i.state = $3)
           AND ($4::text IS NULL OR i.severity = $4)
           AND ($5::uuid IS NULL OR i.server_id = $5)
           AND ($6::uuid IS NULL OR s.site_id = $6)
           AND ($7::timestamptz IS NULL OR i.resolved_at IS NULL OR i.resolved_at >= $7)
           AND ($8::timestamptz IS NULL OR i.opened_at <= $8)
           AND ($9::boolean IS NULL OR (i.state = 'open', i.opened_at, i.id) < ($9, $10, $11))
         ORDER BY i.state = 'open' DESC, i.opened_at DESC, i.id DESC
         LIMIT $12"
    ))
    .bind(all_sites)
    .bind(&sites)
    .bind(state_filter)
    .bind(severity)
    .bind(params.server_id)
    .bind(params.site_id)
    .bind(params.from)
    .bind(params.to)
    .bind(after.map(|c| c.0))
    .bind(after.map(|c| c.1))
    .bind(after.map(|c| c.2))
    .bind(i64::from(limit) + 1)
    .fetch_all(&state.pool)
    .await?;
    let next_cursor = if rows.len() > limit as usize {
        rows.truncate(limit as usize);
        rows.last()
            .map(|r| encode_cursor((r.state == "open", r.opened_at, r.id)))
    } else {
        None
    };
    Ok(Json(IncidentList {
        incidents: rows,
        next_cursor,
    }))
}

/// `GET /api/incidents/{id}`: the incident and its events.
pub async fn detail(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<IncidentDetail>, ApiError> {
    let id = parse_id(&id)?;
    let incident: Option<IncidentView> = sqlx::query_as(&format!("{SELECT} WHERE i.id = $1"))
        .bind(id)
        .fetch_optional(&state.pool)
        .await?;
    authorize(&user, incident.as_ref().map(|i| i.site_id), false)?;
    let incident = incident.ok_or(ApiError::NotFound)?;
    // Events are appended as they happen, so their ID is their order.
    let events = sqlx::query_as(
        "SELECT id, kind, severity, value, occurred_at, recorded_at, details
         FROM incident_events WHERE incident_id = $1 ORDER BY id",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(IncidentDetail { incident, events }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_round_trip_and_refuse_garbage() {
        let at = DateTime::from_timestamp_micros(1_800_000_000_123_456).unwrap();
        let key = (true, at, Uuid::from_u128(7));
        let cursor = encode_cursor(key);
        assert!(cursor
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        // Microseconds survive, as PostgreSQL stores them.
        assert_eq!(decode_cursor(&cursor).unwrap(), key);
        for bad in ["", "!!", &URL_SAFE_NO_PAD.encode(b"[true,1,2]")] {
            assert!(
                matches!(decode_cursor(bad), Err(ApiError::Invalid(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn filters_parse_lowercase_values_only() {
        let parse = |q: &str| {
            let uri: axum::http::Uri = format!("/api/incidents?{q}").parse().unwrap();
            Query::<ListParams>::try_from_uri(&uri).map(|Query(p)| p)
        };
        let params = parse("state=open&severity=critical&limit=10").unwrap();
        assert_eq!(params.state, Some(StateFilter::Open));
        assert_eq!(params.severity, Some(SeverityFilter::Critical));
        assert!(parse("state=closed").is_err());
        assert!(parse("severity=Warning").is_err());
        assert!(parse("sort=asc").is_err());
    }
}
