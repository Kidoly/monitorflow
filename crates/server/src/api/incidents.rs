//! Incident endpoints (ADR-0017, ADR-0019): views of what the threshold
//! engine and the offline rule (ADR-0072) opened and resolved, and their
//! acknowledgement (ADR-0082).
//!
//! The engine owns `incidents`; nothing here changes them. Acknowledging
//! adds to `incident_events` and stops the follow-up, nothing else. Every
//! role reads the incidents of the servers of its sites, and an incident
//! outside them is not found; operators and above acknowledge them.

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use super::access::{authorize, Scope};
use super::audit;
use super::auth::CurrentUser;
use super::follow_up::{follow_up_view, FollowUpView};
use super::sensors::{body, parse_id};
use super::servers::{decode_cursor, encode_cursor, page_limit};
use super::{ApiError, ApiState};
use crate::alerting::reason;
use crate::alerting::rules::FOREVER_S;
use crate::notify::follow_up;

/// Page size without `limit`.
pub const DEFAULT_LIMIT: u32 = 100;
/// Largest accepted `limit`.
pub const MAX_LIMIT: u32 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum StateFilter {
    Open,
    Resolved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SeverityFilter {
    Warning,
    Critical,
}

/// Query string of `GET /api/incidents`. Every filter is optional.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ListParams {
    /// `open` or `resolved`.
    pub state: Option<StateFilter>,
    /// The incident's current severity.
    pub severity: Option<SeverityFilter>,
    /// Incidents of this server.
    pub server_id: Option<Uuid>,
    /// Incidents of the servers and relays of this site.
    pub site_id: Option<Uuid>,
    /// Incidents still open at or resolved after this time.
    pub from: Option<DateTime<Utc>>,
    /// Incidents opened at or before this time.
    pub to: Option<DateTime<Utc>>,
    /// The incidents one notification told together (ADR-0080).
    pub group: Option<Uuid>,
    /// 1 to [`MAX_LIMIT`], default [`DEFAULT_LIMIT`].
    pub limit: Option<u32>,
    /// `next_cursor` of the previous page.
    pub cursor: Option<String>,
}

/// One incident. Field names and types are part of the API contract: add
/// fields, never rename or remove them.
#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct IncidentView {
    pub id: Uuid,
    /// `null` for a relay's offline incident.
    pub server_id: Option<Uuid>,
    /// Set for a relay's offline incident only (ADR-0072).
    pub relay_id: Option<Uuid>,
    /// The server's hostname, or the relay's name.
    pub hostname: String,
    pub site_id: Uuid,
    pub site_name: String,
    pub customer_id: Uuid,
    /// `offline` when the server or relay stopped reporting (ADR-0072).
    pub metric: String,
    /// `""` for a series without an instance.
    pub instance: String,
    /// `warning` or `critical`; the last one while resolved.
    pub severity: String,
    /// `open` or `resolved`.
    pub state: String,
    /// Sample times, not the server clock.
    pub opened_at: DateTime<Utc>,
    /// `null` while open.
    pub resolved_at: Option<DateTime<Utc>>,
    /// The latest value of the metric, in its unit (a ratio from 0 to 1
    /// for `_ratio` metrics); 0 for `offline`.
    pub last_value: f64,
    pub last_value_at: DateTime<Utc>,
    /// The worst value seen while open.
    pub peak_value: f64,
    /// The threshold as it was when the incident opened: `{"metric",
    /// "instance", "comparison", "warning", "critical", "duration_s",
    /// "hysteresis"}`, or `{"metric": "offline", "after_s"}`.
    pub threshold: Value,
    /// While open and covered by a silence, when the last one ends
    /// (ADR-0079): one of its server, relay, site or customer, of
    /// everything, or of its check (ADR-0151).
    pub silenced_until: Option<DateTime<Utc>>,
    /// While open, whether a maintenance window covers it now (ADR-0151):
    /// "In maintenance until 04:00". Silences of chosen checks count, for
    /// the incidents of those checks.
    pub in_maintenance: bool,
    /// Whether a silence ever held back a notification of it.
    pub silenced: bool,
    /// The kind of rule that opened it (ADR-0153): `value`, `window`,
    /// `compare`, `forecast` or `nodata`; `value` for `offline`.
    pub kind: String,
    /// While a `forecast` incident is open, when the value is expected to
    /// reach its target: the time of the last value plus the time left.
    #[sqlx(skip)]
    pub expected_at: Option<DateTime<Utc>>,
    /// The parent's incident that speaks for it (ADR-0152): it opened a
    /// moment before its parent went down and was never told; `null`
    /// otherwise.
    pub hidden_by: Option<Uuid>,
    /// Who said they are on it, and when (ADR-0082); `null` until someone
    /// does. The name stays when the account is removed.
    pub acknowledged_by: Option<String>,
    pub acknowledged_at: Option<DateTime<Utc>>,
    /// What they said they are doing (ADR-0130); `null` from a
    /// notification link, or from an API key that sent none.
    pub acknowledged_note: Option<String>,
    /// Whether the reader may acknowledge it: open, not acknowledged, and
    /// the reader an operator or above of its site.
    #[sqlx(skip)]
    pub can_acknowledge: bool,
    /// Why it opened, in one sentence: "9% available is below the
    /// warning limit of 12%"; for a resolved incident, its peak.
    #[sqlx(skip)]
    pub reason: String,
}

impl IncidentView {
    /// Fills the fields computed from the row.
    fn complete(&mut self, modifiable: &Scope) {
        self.can_acknowledge = acknowledgeable(self, modifiable);
        let open = self.state == "open";
        self.expected_at = expected_at(&self.kind, open, self.last_value, self.last_value_at);
        self.reason = reason::incident(
            &self.metric,
            &self.severity,
            open,
            if open {
                self.last_value
            } else {
                self.peak_value
            },
            &self.threshold,
        );
    }
}

/// When an open forecast expects its target; `None` for other kinds and
/// for a forecast that never gets there.
fn expected_at(kind: &str, open: bool, left_s: f64, at: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if kind != "forecast" || !open || !(0.0..FOREVER_S).contains(&left_s) {
        return None;
    }
    Some(at + chrono::Duration::milliseconds((left_s * 1000.0) as i64))
}

/// Body of `GET /api/incidents`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct IncidentList {
    pub incidents: Vec<IncidentView>,
    /// Pass as `cursor` for the next page; `null` on the last one.
    pub next_cursor: Option<String>,
}

/// One change of an incident.
#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct EventView {
    /// Increasing in the order events happened.
    pub id: i64,
    /// `opened`, `escalated`, `deescalated`, `resolved`, what a silence
    /// did: `silenced` and `silence_ended` (ADR-0079), `acknowledged`, and
    /// `followed_up` when a follow-up step told someone (ADR-0082).
    pub kind: String,
    /// Severity after the change; `null` once resolved.
    pub severity: Option<String>,
    /// The metric's value then; `null` when the event has none.
    pub value: Option<f64>,
    /// Sample time of the change, or the server clock without a sample.
    pub occurred_at: DateTime<Utc>,
    /// When MonitorFlow wrote it.
    pub recorded_at: DateTime<Utc>,
    /// For instance `{"reason": "threshold_removed"}`, `{"notified":
    /// true}` when a silence ended, `{"username", "via"}` when
    /// acknowledged, or `{"step", "steps", "minutes", "told"}` for a
    /// follow-up step.
    pub details: Value,
}

/// A notification that told this incident with others (ADR-0080).
#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct GroupView {
    /// `group` of `GET /api/incidents` lists them.
    pub id: Uuid,
    /// How many incidents it told that the user may read, this one
    /// included. Members on sites outside the user's scope are not counted.
    pub incidents: i32,
    /// When the notification went out.
    pub created_at: DateTime<Utc>,
}

/// Body of `GET /api/incidents/{id}`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct IncidentDetail {
    #[serde(flatten)]
    pub incident: IncidentView,
    /// In the order they happened.
    pub events: Vec<EventView>,
    /// Oldest first.
    pub notification_groups: Vec<GroupView>,
    /// `page` (the web interface or the API) or `link` (the link in a
    /// notification): where it was acknowledged; `null` until then.
    pub acknowledged_via: Option<String>,
    /// Who is told next if nobody acknowledges (ADR-0082); `null` when no
    /// follow-up started: a warning, or not sent yet.
    pub follow_up: Option<FollowUpView>,
    /// While it says a parent is down: everything behind that parent,
    /// unreachable meanwhile (ADR-0152).
    pub hides: Vec<super::dependencies::EndView>,
    /// The incidents it hid: opened by children a moment before it, never
    /// told.
    pub hidden: Vec<Uuid>,
}

/// The sort key `(open, opened_at, id)` of a page's last incident.
type Cursor = (bool, DateTime<Utc>, Uuid);

const SELECT: &str = "SELECT i.id, i.server_id, i.relay_id,
                             COALESCE(s.hostname, r.name) AS hostname, si.id AS site_id,
                             si.name AS site_name, si.customer_id, i.metric, i.instance, i.severity, i.state, i.opened_at,
                             i.resolved_at, i.last_value, i.last_value_at, i.peak_value,
                             i.threshold,
                             CASE WHEN i.state = 'open' THEN
                                 (SELECT max(coalesce(x.ended_at, x.ends_at))
                                  FROM covering_silences(i.server_id, i.relay_id, si.id,
                                                         si.customer_id, i.metric, i.instance) x)
                             END AS silenced_until,
                             i.state = 'open' AND EXISTS (
                                 SELECT 1 FROM covering_silences(i.server_id, i.relay_id, si.id,
                                                                 si.customer_id, i.metric, i.instance) x
                                 WHERE x.window_id IS NOT NULL) AS in_maintenance,
                             EXISTS (SELECT 1 FROM incident_events e
                                     WHERE e.incident_id = i.id AND e.kind = 'silenced') AS silenced,
                             i.kind,
                             i.hidden_by,
                             a.username AS acknowledged_by, a.acknowledged_at,
                             a.note AS acknowledged_note
                      FROM incidents i
                      LEFT JOIN incident_acknowledgements a ON a.incident_id = i.id
                      LEFT JOIN servers s ON s.id = i.server_id
                      LEFT JOIN relays r ON r.id = i.relay_id
                      JOIN sites si ON si.id = COALESCE(s.site_id, r.site_id)";

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
    let limit = page_limit(params.limit, DEFAULT_LIMIT, MAX_LIMIT)?;
    if let (Some(from), Some(to)) = (params.from, params.to) {
        if from > to {
            return Err(ApiError::Invalid("from must not be after to".into()));
        }
    }
    let after: Option<Cursor> = params.cursor.as_deref().map(decode_cursor).transpose()?;
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
    // SQL-safe: interpolates only the constant SELECT.
    let mut rows: Vec<IncidentView> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SELECT}
         WHERE ($1 OR si.id = ANY($2))
           AND ($3::text IS NULL OR i.state = $3)
           AND ($4::text IS NULL OR i.severity = $4)
           AND ($5::uuid IS NULL OR i.server_id = $5)
           AND ($6::uuid IS NULL OR si.id = $6)
           AND ($7::timestamptz IS NULL OR i.resolved_at IS NULL OR i.resolved_at >= $7)
           AND ($8::timestamptz IS NULL OR i.opened_at <= $8)
           AND ($9::boolean IS NULL OR (i.state = 'open', i.opened_at, i.id) < ($9, $10, $11))
           AND ($13::uuid IS NULL OR i.id = ANY(
                    SELECT unnest(incident_ids) FROM notification_groups WHERE id = $13))
         ORDER BY i.state = 'open' DESC, i.opened_at DESC, i.id DESC
         LIMIT $12"
    )))
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
    .bind(params.group)
    .fetch_all(&state.pool)
    .await?;
    let modifiable = user.modifiable_sites();
    for row in &mut rows {
        row.complete(&modifiable);
    }
    let next_cursor = if rows.len() > limit as usize {
        rows.truncate(limit as usize);
        rows.last()
            .map(|r| encode_cursor(&(r.state == "open", r.opened_at, r.id)))
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
    // SQL-safe: interpolates only the constant SELECT.
    let incident: Option<IncidentView> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} WHERE i.id = $1")))
            .bind(id)
            .fetch_optional(&state.pool)
            .await?;
    authorize(&user, incident.as_ref().map(|i| i.site_id), false)?;
    let mut incident = incident.ok_or(ApiError::NotFound)?;
    incident.complete(&user.modifiable_sites());
    // Events are appended as they happen, so their ID is their order.
    let events = sqlx::query_as(
        "SELECT id, kind, severity, value, occurred_at, recorded_at, details
         FROM incident_events WHERE incident_id = $1 ORDER BY id",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await?;
    // A group mixes the sites of one customer: count only the members the
    // user may read, as `GET /api/incidents?group=` lists them.
    let (all_sites, sites) = user.readable_sites().bind();
    let notification_groups = sqlx::query_as(
        "SELECT g.id, g.created_at,
                (SELECT count(*)::int4
                 FROM unnest(g.incident_ids) AS m(id)
                 JOIN incidents i ON i.id = m.id
                 LEFT JOIN servers s ON s.id = i.server_id
                 LEFT JOIN relays r ON r.id = i.relay_id
                 WHERE $2 OR COALESCE(s.site_id, r.site_id) = ANY($3)) AS incidents
         FROM notification_groups g WHERE g.incident_ids @> ARRAY[$1]
         ORDER BY g.created_at, g.id",
    )
    .bind(id)
    .bind(all_sites)
    .bind(&sites)
    .fetch_all(&state.pool)
    .await?;
    let acknowledged_via =
        sqlx::query_scalar("SELECT via FROM incident_acknowledgements WHERE incident_id = $1")
            .bind(id)
            .fetch_optional(&state.pool)
            .await?;
    let follow_up = follow_up_view(&state.pool, &incident).await?;
    let hides = if incident.state == "open" {
        super::dependencies::hides(&state, &user, id).await?
    } else {
        Vec::new()
    };
    let hidden = sqlx::query_scalar(
        "SELECT i.id FROM incidents i
         LEFT JOIN servers s ON s.id = i.server_id
         WHERE i.hidden_by = $1 AND ($2 OR s.site_id = ANY($3))
         ORDER BY i.opened_at, i.id",
    )
    .bind(id)
    .bind(all_sites)
    .bind(&sites)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(IncidentDetail {
        incident,
        events,
        notification_groups,
        acknowledged_via,
        follow_up,
        hides,
        hidden,
    }))
}

fn acknowledgeable(incident: &IncidentView, modifiable: &Scope) -> bool {
    incident.state == "open"
        && incident.acknowledged_at.is_none()
        && modifiable.contains(incident.site_id)
}

/// Body of `POST /api/incidents/acknowledge`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AcknowledgeRequest {
    /// 1 to [`MAX_LIMIT`] incidents, like a group of a notification.
    pub incident_ids: Vec<Uuid>,
    /// What the user is doing about them, up to 500 characters, shown with
    /// the acknowledgement (ADR-0130). Optional, for people and API keys
    /// alike (ADR-0166).
    #[serde(default)]
    pub note: Option<String>,
}

/// Longest acknowledgement note, in characters.
pub const MAX_NOTE: usize = 500;

/// The note trimmed, `None` when blank or missing (ADR-0166), or why it
/// is refused: too long.
pub fn acknowledgement_note(note: Option<&str>) -> Result<Option<String>, String> {
    let note = note.map(str::trim).filter(|n| !n.is_empty());
    match note {
        Some(n) if n.chars().count() > MAX_NOTE => {
            Err(format!("Keep the note under {MAX_NOTE} characters."))
        }
        _ => Ok(note.map(str::to_owned)),
    }
}

/// Body of the answer: the incidents this request acknowledged. The
/// others were resolved, or someone acknowledged them first.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct Acknowledged {
    pub acknowledged: Vec<Uuid>,
}

/// `POST /api/incidents/acknowledge`: says the user is on it, so nobody
/// else is told next (ADR-0082). Needs an operator role or above on each
/// incident's site, as silencing its server does: 404 when one is not
/// readable, 403 when one is read-only, before anything is written.
/// Audited as `incident.acknowledge` per incident.
pub async fn acknowledge(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<AcknowledgeRequest>, JsonRejection>,
) -> Result<Json<Acknowledged>, ApiError> {
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let request = body(request)?;
    let mut ids = request.incident_ids;
    ids.sort();
    ids.dedup();
    if ids.is_empty() || ids.len() > MAX_LIMIT as usize {
        return Err(ApiError::Invalid(format!(
            "acknowledge 1 to {MAX_LIMIT} incidents at once"
        )));
    }
    let sites: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT i.id, si.id FROM incidents i
         LEFT JOIN servers s ON s.id = i.server_id
         LEFT JOIN relays r ON r.id = i.relay_id
         JOIN sites si ON si.id = COALESCE(s.site_id, r.site_id)
         WHERE i.id = ANY($1)",
    )
    .bind(&ids)
    .fetch_all(&state.pool)
    .await?;
    if sites.len() != ids.len() {
        return Err(ApiError::NotFound);
    }
    for (_, site) in &sites {
        authorize(&user, Some(*site), false)?;
    }
    for (_, site) in &sites {
        authorize(&user, Some(*site), true)?;
    }
    // After the permissions, so a note never tells what an ID is.
    let note = acknowledgement_note(request.note.as_deref()).map_err(ApiError::Invalid)?;
    let mut tx = state.pool.begin().await?;
    let acknowledged = follow_up::acknowledge(
        &mut tx,
        &ids,
        (user.id, &user.username),
        "page",
        note.as_deref(),
        Utc::now(),
    )
    .await?;
    let actor = user.id.to_string();
    for incident in &acknowledged {
        let target = incident.to_string();
        audit::record(
            &mut *tx,
            audit::Event {
                actor_kind: "user",
                actor_id: &actor,
                action: "incident.acknowledge",
                target: Some(("incident", &target)),
                details: match &note {
                    Some(note) => json!({ "via": "page", "note": note }),
                    None => json!({ "via": "page" }),
                },
            },
        )
        .await?;
    }
    tx.commit().await?;
    Ok(Json(Acknowledged { acknowledged }))
}

#[cfg(test)]
mod tests {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;

    use super::*;

    #[test]
    fn cursors_round_trip_and_refuse_garbage() {
        let at = DateTime::from_timestamp_micros(1_800_000_000_123_456).unwrap();
        let key = (true, at, Uuid::from_u128(7));
        let cursor = encode_cursor(&key);
        assert!(cursor
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        // Microseconds survive, as PostgreSQL stores them.
        assert_eq!(decode_cursor::<Cursor>(&cursor).unwrap(), key);
        for bad in ["", "!!", &URL_SAFE_NO_PAD.encode(b"[true,1,2]")] {
            assert!(
                matches!(decode_cursor::<Cursor>(bad), Err(ApiError::Invalid(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_note_is_trimmed_optional_and_short() {
        assert_eq!(
            acknowledgement_note(Some("  swapping the disk ")),
            Ok(Some("swapping the disk".into()))
        );
        assert_eq!(acknowledgement_note(None), Ok(None));
        assert_eq!(acknowledgement_note(Some("   ")), Ok(None));
        let long = "x".repeat(MAX_NOTE + 1);
        assert!(acknowledgement_note(Some(&long)).is_err());
        assert!(acknowledgement_note(Some(&long[1..])).is_ok());
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

    #[test]
    fn open_forecasts_say_when_they_expect_their_target() {
        let at = DateTime::parse_from_rfc3339("2026-11-20T10:00:00Z")
            .unwrap()
            .to_utc();
        let in_two_days = DateTime::parse_from_rfc3339("2026-11-22T10:00:00Z")
            .unwrap()
            .to_utc();
        assert_eq!(
            expected_at("forecast", true, 172_800.0, at),
            Some(in_two_days)
        );
        assert_eq!(expected_at("forecast", true, 0.0, at), Some(at));
        assert_eq!(expected_at("forecast", false, 172_800.0, at), None);
        assert_eq!(expected_at("forecast", true, FOREVER_S, at), None);
        assert_eq!(expected_at("value", true, 0.9, at), None);
    }
}
