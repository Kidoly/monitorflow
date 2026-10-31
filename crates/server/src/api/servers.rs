//! `GET /api/servers` and `GET /api/servers/{id}`: managed machines from
//! the database, with their live connection status from the gRPC
//! [`Registry`](crate::grpc::Registry).
//!
//! Pages are keyset-paginated on the sort key, so a server added or
//! removed between two pages neither repeats nor skips the others.

use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::Json;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use monitorflow_common::tls::PeerRole;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::access::authorize;
use super::auth::CurrentUser;
use super::sensors::parse_id;
use super::{ApiError, ApiState};
use crate::ca::Serial;

/// Page size without `limit`.
pub const DEFAULT_LIMIT: u32 = 500;
/// Largest accepted `limit`.
pub const MAX_LIMIT: u32 = 1000;

/// Query string of `GET /api/servers`.
#[derive(Debug, Default, Deserialize)]
pub struct ListParams {
    /// 1 to [`MAX_LIMIT`], default [`DEFAULT_LIMIT`].
    pub limit: Option<u32>,
    /// `next_cursor` of the previous page.
    pub cursor: Option<String>,
}

/// Body of `GET /api/servers`.
#[derive(Debug, Serialize)]
pub struct ServerList {
    pub servers: Vec<ServerView>,
    /// Pass as `cursor` for the next page; `null` on the last one.
    pub next_cursor: Option<String>,
}

/// The sort key of a page's last row, such as `(site name, hostname, peer
/// ID)` for servers, as opaque text.
pub(super) fn encode_cursor(key: &impl Serialize) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(key).unwrap_or_default())
}

pub(super) fn decode_cursor<T: DeserializeOwned>(cursor: &str) -> Result<T, ApiError> {
    URL_SAFE_NO_PAD
        .decode(cursor)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or_else(|| ApiError::Invalid("invalid cursor".into()))
}

/// `limit` checked against 1 to `max`, or `default`.
pub(super) fn page_limit(limit: Option<u32>, default: u32, max: u32) -> Result<u32, ApiError> {
    let limit = limit.unwrap_or(default);
    if !(1..=max).contains(&limit) {
        return Err(ApiError::Invalid(format!(
            "limit must be between 1 and {max}"
        )));
    }
    Ok(limit)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// The agent has an open stream to this server, directly or via a relay.
    Online,
    Offline,
}

/// One server. Field names and types are part of the API contract: add
/// fields, never rename or remove them.
#[derive(Debug, Serialize)]
pub struct ServerView {
    pub id: Uuid,
    /// The agent certificate ID.
    pub peer_id: String,
    pub hostname: String,
    pub site_id: Uuid,
    pub site_name: String,
    /// Customer of the site (ADR-0026).
    pub customer_id: Uuid,
    pub customer_name: String,
    pub relay_id: Option<Uuid>,
    /// `debian`, `rhel`, `windows`, or `linux` for any other distribution.
    pub os_family: String,
    pub os_version: Option<String>,
    /// Whether the host can take package updates and sources (ADR-0063).
    pub package_updates: bool,
    /// Version reported by the live stream, else the one last stored.
    pub agent_version: Option<String>,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub status: Status,
    /// Start of the current stream, when online.
    pub connected_at: Option<DateTime<Utc>>,
    /// Last message received: from the live stream when online, else the
    /// last time recorded in the database.
    pub last_heartbeat: Option<DateTime<Utc>>,
    /// Peer ID of the relay carrying the live stream, if any.
    pub via_relay: Option<String>,
    /// Threshold and offline incidents currently open on this server.
    pub open_incidents: i64,
    /// Whether it opens an incident when offline (ADR-0057); muted when
    /// this or `site_offline_alerts` is `false`.
    pub offline_alerts: bool,
    pub site_offline_alerts: bool,
    /// Latest expiry among the agent's unrevoked certificates; `null` when
    /// it has none. Past dates mean every certificate has expired.
    pub cert_not_after: Option<DateTime<Utc>>,
}

/// Body of `GET /api/servers/{id}`: the listing row plus the agent's
/// current certificate.
#[derive(Debug, Serialize)]
pub struct ServerDetail {
    #[serde(flatten)]
    pub server: ServerView,
    /// The certificate issued last to this agent, revoked or not; `null`
    /// when the CA never issued one (a server row created by hand).
    pub certificate: Option<CertificateView>,
}

/// A certificate from the `certificates` table.
#[derive(Debug, Serialize)]
pub struct CertificateView {
    /// Lowercase hexadecimal without leading zeros.
    pub serial: String,
    pub not_before: DateTime<Utc>,
    /// Expiry.
    pub not_after: DateTime<Utc>,
    pub issued_at: DateTime<Utc>,
    pub revoked: bool,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revocation_reason: Option<String>,
}

#[derive(sqlx::FromRow)]
struct CertificateRow {
    serial: Vec<u8>,
    not_before: DateTime<Utc>,
    not_after: DateTime<Utc>,
    issued_at: DateTime<Utc>,
    revoked_at: Option<DateTime<Utc>>,
    revocation_reason: Option<String>,
}

impl From<CertificateRow> for CertificateView {
    fn from(row: CertificateRow) -> Self {
        Self {
            serial: Serial::from_bytes(&row.serial).to_string(),
            not_before: row.not_before,
            not_after: row.not_after,
            issued_at: row.issued_at,
            revoked: row.revoked_at.is_some(),
            revoked_at: row.revoked_at,
            revocation_reason: row.revocation_reason,
        }
    }
}

/// Columns of [`Row`]; callers add the `WHERE` clause.
const SELECT_SERVERS: &str = "SELECT s.id, s.peer_id, s.hostname, s.site_id, si.name AS site_name,
            si.customer_id, c.name AS customer_name, s.relay_id,
            s.os_family, s.os_version, s.package_updates, s.agent_version, s.created_at,
            s.last_seen_at,
            s.revoked_at, s.offline_alerts, si.offline_alerts AS site_offline_alerts,
            -- One probe of the partial unique index on open incidents.
            (SELECT count(*) FROM incidents i
             WHERE i.server_id = s.id AND i.state = 'open') AS open_incidents,
            -- One probe of the partial index on active certificates.
            (SELECT max(c.not_after) FROM certificates c
             WHERE c.peer_role = 'agent' AND c.peer_id = s.peer_id
               AND c.revoked_at IS NULL) AS cert_not_after
     FROM servers s
     JOIN sites si ON si.id = s.site_id
     JOIN customers c ON c.id = si.customer_id";

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    peer_id: String,
    hostname: String,
    site_id: Uuid,
    site_name: String,
    customer_id: Uuid,
    customer_name: String,
    relay_id: Option<Uuid>,
    os_family: String,
    os_version: Option<String>,
    package_updates: bool,
    agent_version: Option<String>,
    created_at: DateTime<Utc>,
    last_seen_at: Option<DateTime<Utc>>,
    revoked_at: Option<DateTime<Utc>>,
    offline_alerts: bool,
    site_offline_alerts: bool,
    open_incidents: i64,
    cert_not_after: Option<DateTime<Utc>>,
}

/// `GET /api/servers`: one page of the servers of the sites the user may
/// read, sorted by site name, hostname and peer ID. Needs at least one role.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
    params: Result<Query<ListParams>, QueryRejection>,
) -> Result<Json<ServerList>, ApiError> {
    let Query(params) = params.map_err(|r| ApiError::Invalid(r.body_text()))?;
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let (all_sites, sites) = user.readable_sites().bind();
    let limit = page_limit(params.limit, DEFAULT_LIMIT, MAX_LIMIT)?;
    let (after_site, after_host, after_peer) = match params.cursor.as_deref() {
        Some(cursor) => {
            let (site, host, peer): (String, String, String) = decode_cursor(cursor)?;
            (Some(site), Some(host), Some(peer))
        }
        None => (None, None, None),
    };
    let mut rows: Vec<Row> = sqlx::query_as(&format!(
        "{SELECT_SERVERS}
         WHERE ($1 OR s.site_id = ANY($2))
           AND ($3::text IS NULL OR (si.name, s.hostname, s.peer_id) > ($3, $4, $5))
         ORDER BY si.name, s.hostname, s.peer_id
         LIMIT $6"
    ))
    .bind(all_sites)
    .bind(&sites)
    .bind(after_site)
    .bind(after_host)
    .bind(after_peer)
    .bind(i64::from(limit) + 1)
    .fetch_all(&state.pool)
    .await?;
    let next_cursor = if rows.len() > limit as usize {
        rows.truncate(limit as usize);
        rows.last()
            .map(|r| encode_cursor(&(&r.site_name, &r.hostname, &r.peer_id)))
    } else {
        None
    };

    let servers = rows
        .into_iter()
        .map(|row| ServerView::new(&state, row))
        .collect();
    Ok(Json(ServerList {
        servers,
        next_cursor,
    }))
}

/// `GET /api/servers/{id}`: one server the user may read, with its
/// current certificate. Outside the user's sites it is 404.
pub async fn detail(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<ServerDetail>, ApiError> {
    let id = parse_id(&id)?;
    let row: Option<Row> = sqlx::query_as(&format!("{SELECT_SERVERS} WHERE s.id = $1"))
        .bind(id)
        .fetch_optional(&state.pool)
        .await?;
    authorize(&user, row.as_ref().map(|r| r.site_id), false)?;
    let row = row.ok_or(ApiError::NotFound)?;
    let certificate = latest_certificate(&state.pool, PeerRole::Agent, &row.peer_id).await?;
    Ok(Json(ServerDetail {
        server: ServerView::new(&state, row),
        certificate,
    }))
}

/// The certificate the CA issued last to `role`/`peer_id`, revoked or not.
pub(super) async fn latest_certificate(
    pool: &sqlx::PgPool,
    role: PeerRole,
    peer_id: &str,
) -> Result<Option<CertificateView>, sqlx::Error> {
    // Newest first on the per-peer issued_at index.
    let certificate: Option<CertificateRow> = sqlx::query_as(
        "SELECT serial, not_before, not_after, issued_at, revoked_at, revocation_reason
         FROM certificates
         WHERE peer_role = $1 AND peer_id = $2
         ORDER BY issued_at DESC, serial DESC
         LIMIT 1",
    )
    .bind(role.as_str())
    .bind(peer_id)
    .fetch_optional(pool)
    .await?;
    Ok(certificate.map(CertificateView::from))
}

impl ServerView {
    /// `row` with the live status of its agent, if connected.
    fn new(state: &ApiState, row: Row) -> Self {
        let live = state
            .registry
            .get(&row.peer_id)
            .filter(|peer| peer.role == PeerRole::Agent);
        match live {
            Some(peer) => Self {
                status: Status::Online,
                connected_at: Some(peer.connected_at),
                last_heartbeat: Some(peer.last_seen),
                via_relay: peer.via_relay,
                agent_version: Some(peer.agent_version).filter(|v| !v.is_empty()),
                ..Self::offline(row)
            },
            None => Self::offline(row),
        }
    }

    fn offline(row: Row) -> Self {
        Self {
            id: row.id,
            peer_id: row.peer_id,
            hostname: row.hostname,
            site_id: row.site_id,
            site_name: row.site_name,
            customer_id: row.customer_id,
            customer_name: row.customer_name,
            relay_id: row.relay_id,
            os_family: row.os_family,
            os_version: row.os_version,
            package_updates: row.package_updates,
            agent_version: row.agent_version,
            created_at: row.created_at,
            revoked_at: row.revoked_at,
            status: Status::Offline,
            connected_at: None,
            last_heartbeat: row.last_seen_at,
            via_relay: None,
            open_incidents: row.open_incidents,
            offline_alerts: row.offline_alerts,
            site_offline_alerts: row.site_offline_alerts,
            cert_not_after: row.cert_not_after,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_round_trip_and_refuse_garbage() {
        let cursor = encode_cursor(&("paris", "web-1", "agent-1"));
        assert!(cursor
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_eq!(
            decode_cursor::<(String, String, String)>(&cursor).unwrap(),
            ("paris".into(), "web-1".into(), "agent-1".into())
        );
        for bad in ["", "!!", &URL_SAFE_NO_PAD.encode(b"[1,2]")] {
            assert!(
                matches!(
                    decode_cursor::<(String, String, String)>(bad),
                    Err(ApiError::Invalid(_))
                ),
                "{bad}"
            );
        }
    }
}
