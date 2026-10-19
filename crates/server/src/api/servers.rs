//! `GET /api/servers`: managed machines from the database, with their live
//! connection status from the gRPC [`Registry`](crate::grpc::Registry).
//!
//! Pages are keyset-paginated on the sort key, so a server added or
//! removed between two pages neither repeats nor skips the others.

use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::Json;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use monitorflow_common::tls::PeerRole;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::auth::{CurrentUser, SiteScope};
use super::{ApiError, ApiState};

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

/// The sort key `(site name, hostname, peer ID)` of a page's last server,
/// as opaque text.
fn encode_cursor(key: (&str, &str, &str)) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(&key).unwrap_or_default())
}

fn decode_cursor(cursor: &str) -> Result<(String, String, String), ApiError> {
    URL_SAFE_NO_PAD
        .decode(cursor)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or_else(|| ApiError::Invalid("invalid cursor".into()))
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
    pub relay_id: Option<Uuid>,
    /// `debian` or `rhel`.
    pub os_family: String,
    pub os_version: Option<String>,
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
    /// Threshold incidents currently open on this server.
    pub open_incidents: i64,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    peer_id: String,
    hostname: String,
    site_id: Uuid,
    site_name: String,
    relay_id: Option<Uuid>,
    os_family: String,
    os_version: Option<String>,
    agent_version: Option<String>,
    created_at: DateTime<Utc>,
    last_seen_at: Option<DateTime<Utc>>,
    revoked_at: Option<DateTime<Utc>>,
    open_incidents: i64,
}

/// `GET /api/servers`: one page of the servers of the sites the user may
/// read, sorted by site name, hostname and peer ID. Needs at least one role.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
    params: Result<Query<ListParams>, QueryRejection>,
) -> Result<Json<ServerList>, ApiError> {
    let Query(params) = params.map_err(|r| ApiError::Invalid(r.body_text()))?;
    let (all_sites, sites) = match user.readable_sites() {
        SiteScope::None => return Err(ApiError::Forbidden),
        SiteScope::All => (true, Vec::new()),
        SiteScope::Sites(sites) => (false, sites),
    };
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(ApiError::Invalid(format!(
            "limit must be between 1 and {MAX_LIMIT}"
        )));
    }
    let (after_site, after_host, after_peer) = match params.cursor.as_deref() {
        Some(cursor) => {
            let (site, host, peer) = decode_cursor(cursor)?;
            (Some(site), Some(host), Some(peer))
        }
        None => (None, None, None),
    };
    let mut rows: Vec<Row> = sqlx::query_as(
        "SELECT s.id, s.peer_id, s.hostname, s.site_id, si.name AS site_name, s.relay_id,
                s.os_family, s.os_version, s.agent_version, s.created_at, s.last_seen_at,
                s.revoked_at,
                -- One probe of the partial unique index on open incidents.
                (SELECT count(*) FROM incidents i
                 WHERE i.server_id = s.id AND i.state = 'open') AS open_incidents
         FROM servers s
         JOIN sites si ON si.id = s.site_id
         WHERE ($1 OR s.site_id = ANY($2))
           AND ($3::text IS NULL OR (si.name, s.hostname, s.peer_id) > ($3, $4, $5))
         ORDER BY si.name, s.hostname, s.peer_id
         LIMIT $6",
    )
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
            .map(|r| encode_cursor((&r.site_name, &r.hostname, &r.peer_id)))
    } else {
        None
    };

    let servers = rows
        .into_iter()
        .map(|row| {
            let live = state
                .registry
                .get(&row.peer_id)
                .filter(|peer| peer.role == PeerRole::Agent);
            match live {
                Some(peer) => ServerView {
                    status: Status::Online,
                    connected_at: Some(peer.connected_at),
                    last_heartbeat: Some(peer.last_seen),
                    via_relay: peer.via_relay,
                    agent_version: Some(peer.agent_version).filter(|v| !v.is_empty()),
                    ..ServerView::offline(row)
                },
                None => ServerView::offline(row),
            }
        })
        .collect();
    Ok(Json(ServerList {
        servers,
        next_cursor,
    }))
}

impl ServerView {
    fn offline(row: Row) -> Self {
        Self {
            id: row.id,
            peer_id: row.peer_id,
            hostname: row.hostname,
            site_id: row.site_id,
            site_name: row.site_name,
            relay_id: row.relay_id,
            os_family: row.os_family,
            os_version: row.os_version,
            agent_version: row.agent_version,
            created_at: row.created_at,
            revoked_at: row.revoked_at,
            status: Status::Offline,
            connected_at: None,
            last_heartbeat: row.last_seen_at,
            via_relay: None,
            open_incidents: row.open_incidents,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_round_trip_and_refuse_garbage() {
        let cursor = encode_cursor(("paris", "web-1", "agent-1"));
        assert!(cursor
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_eq!(
            decode_cursor(&cursor).unwrap(),
            ("paris".into(), "web-1".into(), "agent-1".into())
        );
        for bad in ["", "!!", &URL_SAFE_NO_PAD.encode(b"[1,2]")] {
            assert!(
                matches!(decode_cursor(bad), Err(ApiError::Invalid(_))),
                "{bad}"
            );
        }
    }
}
