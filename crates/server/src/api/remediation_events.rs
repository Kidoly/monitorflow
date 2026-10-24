//! `GET /api/remediation/events`: the remediation audit log (ADR-0045),
//! the events of every incident the user reads, newest first.
//!
//! Each incident's chain is verified by its detail route
//! (`/api/remediation/incidents/{id}`); this listing reads across chains
//! and leaves the large texts (rendered context, command output) to it.

use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::auth::CurrentUser;
use super::servers::{decode_cursor, encode_cursor, page_limit};
use super::{ApiError, ApiState};
use crate::remediation::audit::EventKind;

pub const DEFAULT_LIMIT: u32 = 100;
pub const MAX_LIMIT: u32 = 500;

/// `actor_kind` values, as in `audit_log`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    User,
    System,
    Ai,
    Peer,
}

impl ActorKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::System => "system",
            Self::Ai => "ai",
            Self::Peer => "peer",
        }
    }
}

/// Query string; every filter is optional.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListParams {
    pub customer_id: Option<Uuid>,
    pub site_id: Option<Uuid>,
    pub server_id: Option<Uuid>,
    pub incident_id: Option<Uuid>,
    pub kind: Option<EventKind>,
    pub actor_kind: Option<ActorKind>,
    /// A user ID, `server`, a model or a peer ID.
    pub actor_id: Option<String>,
    /// Events at or after `from` and before `to`.
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    /// 1 to [`MAX_LIMIT`], default [`DEFAULT_LIMIT`].
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

/// One event with its incident's server. Field names and types are part
/// of the API contract: add fields, never rename or remove them.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct EventView {
    pub incident_id: Uuid,
    pub seq: i64,
    pub kind: EventKind,
    pub actor_kind: String,
    pub actor_id: String,
    /// The user's name when `actor_kind` is `user`.
    pub actor_name: Option<String>,
    /// As stored, without `context.rendered` (`context_sent`) and
    /// `stdout`/`stderr` (`command_result`).
    pub payload: serde_json::Value,
    pub created_at: DateTime<Utc>,
    /// Hex, as in the incident detail.
    pub hash: String,
    pub customer_id: Uuid,
    pub customer_name: String,
    pub site_id: Uuid,
    pub site_name: String,
    pub server_id: Uuid,
    pub hostname: String,
    pub metric: String,
    pub instance: String,
}

#[derive(Debug, Serialize)]
pub struct EventList {
    pub events: Vec<EventView>,
    pub next_cursor: Option<String>,
}

/// `GET /api/remediation/events`: one page, newest first.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
    params: Result<Query<ListParams>, QueryRejection>,
) -> Result<Json<EventList>, ApiError> {
    let Query(params) = params.map_err(|r| ApiError::Invalid(r.body_text()))?;
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let (all_sites, sites) = user.readable_sites().bind();
    let limit = page_limit(params.limit, DEFAULT_LIMIT, MAX_LIMIT)?;
    let before: Option<(DateTime<Utc>, Uuid, i64)> =
        params.cursor.as_deref().map(decode_cursor).transpose()?;
    let (before_at, before_incident, before_seq) = match before {
        Some((at, incident, seq)) => (Some(at), Some(incident), Some(seq)),
        None => (None, None, None),
    };
    let mut rows: Vec<EventView> = sqlx::query_as(
        "SELECT e.incident_id, e.seq, e.kind, e.actor_kind, e.actor_id, u.username AS actor_name,
                CASE e.kind
                    WHEN 'context_sent' THEN e.payload #- '{context,rendered}'
                    WHEN 'command_result' THEN e.payload - 'stdout' - 'stderr'
                    ELSE e.payload
                END AS payload,
                e.created_at, encode(e.hash, 'hex') AS hash, i.customer_id,
                c.name AS customer_name, s.site_id, si.name AS site_name, i.server_id,
                s.hostname, i.metric, i.instance
         FROM remediation_incident_events e
         JOIN remediation_incidents i ON i.id = e.incident_id
         JOIN servers s ON s.id = i.server_id
         JOIN sites si ON si.id = s.site_id
         JOIN customers c ON c.id = i.customer_id
         LEFT JOIN users u ON e.actor_kind = 'user' AND u.id::text = e.actor_id
         WHERE ($1 OR s.site_id = ANY($2))
           AND ($3::uuid IS NULL OR i.customer_id = $3)
           AND ($4::uuid IS NULL OR s.site_id = $4)
           AND ($5::uuid IS NULL OR i.server_id = $5)
           AND ($6::uuid IS NULL OR e.incident_id = $6)
           AND ($7::text IS NULL OR e.kind = $7)
           AND ($8::text IS NULL OR e.actor_kind = $8)
           AND ($9::text IS NULL OR e.actor_id = $9)
           AND ($10::timestamptz IS NULL OR e.created_at >= $10)
           AND ($11::timestamptz IS NULL OR e.created_at < $11)
           AND ($12::timestamptz IS NULL
                OR (e.created_at, e.incident_id, e.seq) < ($12, $13, $14))
         ORDER BY e.created_at DESC, e.incident_id DESC, e.seq DESC
         LIMIT $15",
    )
    .bind(all_sites)
    .bind(&sites)
    .bind(params.customer_id)
    .bind(params.site_id)
    .bind(params.server_id)
    .bind(params.incident_id)
    .bind(params.kind)
    .bind(params.actor_kind.map(ActorKind::as_str))
    .bind(params.actor_id.as_deref())
    .bind(params.from)
    .bind(params.to)
    .bind(before_at)
    .bind(before_incident)
    .bind(before_seq)
    .bind(i64::from(limit) + 1)
    .fetch_all(&state.pool)
    .await?;
    let next_cursor = if rows.len() > limit as usize {
        rows.truncate(limit as usize);
        rows.last()
            .map(|r| encode_cursor(&(r.created_at, r.incident_id, r.seq)))
    } else {
        None
    };
    Ok(Json(EventList {
        events: rows,
        next_cursor,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_parse_known_kinds_and_actors_only() {
        let parse = |q: &str| {
            let uri: axum::http::Uri = format!("/api/remediation/events?{q}").parse().unwrap();
            Query::<ListParams>::try_from_uri(&uri).map(|Query(p)| p)
        };
        let params = parse("kind=approval&actor_kind=user&from=2026-10-24T00:00:00Z").unwrap();
        assert_eq!(params.kind, Some(EventKind::Approval));
        assert_eq!(params.actor_kind, Some(ActorKind::User));
        assert!(params.from.is_some());
        assert!(parse("kind=deletion").is_err());
        assert!(parse("actor_kind=root").is_err());
        assert!(parse("from=yesterday").is_err());
        assert!(parse("action=x").is_err());
    }
}
