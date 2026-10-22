//! Remediation incident endpoints (ADR-0042): read-only views of what the
//! AI remediation loop did, with each incident's chained history.
//!
//! The loop owns `remediation_incidents` and their events; nothing here
//! writes them. Every role reads the incidents of the servers of its
//! sites, and an incident outside them is not found.

use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::access::authorize;
use super::auth::CurrentUser;
use super::sensors::parse_id;
use super::servers::{decode_cursor, encode_cursor, page_limit};
use super::{ApiError, ApiState};
use crate::remediation::audit::{self, AuditError, ChainReport, IncidentState, StoredEvent};

pub const DEFAULT_LIMIT: u32 = 100;
pub const MAX_LIMIT: u32 = 500;

/// Query string of `GET /api/remediation/incidents`. Every filter is
/// optional.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListParams {
    pub state: Option<IncidentState>,
    pub customer_id: Option<Uuid>,
    pub site_id: Option<Uuid>,
    pub server_id: Option<Uuid>,
    pub alert_incident_id: Option<Uuid>,
    /// 1 to [`MAX_LIMIT`], default [`DEFAULT_LIMIT`].
    pub limit: Option<u32>,
    /// `next_cursor` of the previous page.
    pub cursor: Option<String>,
}

/// One remediation incident. Field names and types are part of the API
/// contract: add fields, never rename or remove them.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct IncidentView {
    pub id: Uuid,
    pub customer_id: Uuid,
    pub customer_name: String,
    pub site_id: Uuid,
    pub site_name: String,
    pub server_id: Uuid,
    pub hostname: String,
    /// The threshold incident that opened it, see `/api/incidents`.
    pub alert_incident_id: Option<Uuid>,
    pub metric: String,
    /// `""` for a series without an instance.
    pub instance: String,
    pub state: IncidentState,
    pub opened_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    /// Limits as they were when it opened.
    pub max_commands: i32,
    pub max_duration_s: i32,
    /// Number of events.
    pub event_count: i64,
}

/// Body of `GET /api/remediation/incidents`.
#[derive(Debug, Serialize)]
pub struct IncidentList {
    pub incidents: Vec<IncidentView>,
    /// Pass as `cursor` for the next page; `null` on the last one.
    pub next_cursor: Option<String>,
}

/// Body of `GET /api/remediation/incidents/{id}`.
#[derive(Debug, Serialize)]
pub struct IncidentDetail {
    #[serde(flatten)]
    pub incident: IncidentView,
    /// By sequence number, from 1.
    pub events: Vec<StoredEvent>,
    /// The hash chain recomputed at this request.
    pub chain: ChainReport,
}

const SELECT: &str = "SELECT i.id, i.customer_id, c.name AS customer_name, s.site_id,
                             si.name AS site_name, i.server_id, s.hostname,
                             i.alert_incident_id, i.metric, i.instance, i.state,
                             i.opened_at, i.closed_at, i.max_commands, i.max_duration_s,
                             i.head_seq AS event_count
                      FROM remediation_incidents i
                      JOIN servers s ON s.id = i.server_id
                      JOIN sites si ON si.id = s.site_id
                      JOIN customers c ON c.id = i.customer_id";

/// `GET /api/remediation/incidents`: one page of the incidents of the
/// user's sites, newest first.
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
    let limit = page_limit(params.limit, DEFAULT_LIMIT, MAX_LIMIT)?;
    let before: Option<(DateTime<Utc>, Uuid)> =
        params.cursor.as_deref().map(decode_cursor).transpose()?;
    let (before_at, before_id) = before.unzip();
    // Filters outside the user's scope match nothing, as if they had no
    // incidents.
    let mut rows: Vec<IncidentView> = sqlx::query_as(&format!(
        "{SELECT}
         WHERE ($1 OR s.site_id = ANY($2))
           AND ($3::text IS NULL OR i.state = $3)
           AND ($4::uuid IS NULL OR i.customer_id = $4)
           AND ($5::uuid IS NULL OR s.site_id = $5)
           AND ($6::uuid IS NULL OR i.server_id = $6)
           AND ($7::uuid IS NULL OR i.alert_incident_id = $7)
           AND ($8::timestamptz IS NULL OR (i.opened_at, i.id) < ($8, $9))
         ORDER BY i.opened_at DESC, i.id DESC
         LIMIT $10"
    ))
    .bind(all_sites)
    .bind(&sites)
    .bind(params.state.map(IncidentState::as_str))
    .bind(params.customer_id)
    .bind(params.site_id)
    .bind(params.server_id)
    .bind(params.alert_incident_id)
    .bind(before_at)
    .bind(before_id)
    .bind(i64::from(limit) + 1)
    .fetch_all(&state.pool)
    .await?;
    let next_cursor = if rows.len() > limit as usize {
        rows.truncate(limit as usize);
        rows.last().map(|r| encode_cursor(&(r.opened_at, r.id)))
    } else {
        None
    };
    Ok(Json(IncidentList {
        incidents: rows,
        next_cursor,
    }))
}

/// `GET /api/remediation/incidents/{id}`: the incident, its events and
/// whether their chain is intact.
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
    let events = audit::events(&state.pool, id).await.map_err(from_audit)?;
    let chain = audit::verify(&state.pool, id).await.map_err(from_audit)?;
    if !chain.valid {
        tracing::error!(incident = %id, ?chain, "remediation incident history fails verification");
    }
    Ok(Json(IncidentDetail {
        incident,
        events,
        chain,
    }))
}

fn from_audit(error: AuditError) -> ApiError {
    match error {
        AuditError::NotFound => ApiError::NotFound,
        AuditError::Database(e) => ApiError::Database(e),
        other => ApiError::Internal(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_parse_snake_case_states_only() {
        let parse = |q: &str| {
            let uri: axum::http::Uri = format!("/api/remediation/incidents?{q}").parse().unwrap();
            Query::<ListParams>::try_from_uri(&uri).map(|Query(p)| p)
        };
        let params = parse("state=awaiting_approval&limit=10").unwrap();
        assert_eq!(params.state, Some(IncidentState::AwaitingApproval));
        assert_eq!(params.limit, Some(10));
        assert!(parse("state=closed").is_err());
        assert!(parse("state=Open").is_err());
        assert!(parse("severity=warning").is_err());
    }
}
