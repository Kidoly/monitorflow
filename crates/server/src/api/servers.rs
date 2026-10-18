//! `GET /api/servers`: managed machines from the database, with their live
//! connection status from the gRPC [`Registry`](crate::grpc::Registry).

use axum::extract::State;
use axum::Json;
use chrono::{DateTime, Utc};
use monitorflow_common::tls::PeerRole;
use serde::Serialize;
use uuid::Uuid;

use super::auth::{CurrentUser, SiteScope};
use super::{ApiError, ApiState};

/// Body of `GET /api/servers`.
#[derive(Debug, Serialize)]
pub struct ServerList {
    pub servers: Vec<ServerView>,
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
}

/// `GET /api/servers`: every server of the sites the user may read, sorted
/// by site name, hostname and peer ID. Needs at least one role.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<ServerList>, ApiError> {
    let (all_sites, sites) = match user.readable_sites() {
        SiteScope::None => return Err(ApiError::Forbidden),
        SiteScope::All => (true, Vec::new()),
        SiteScope::Sites(sites) => (false, sites),
    };
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT s.id, s.peer_id, s.hostname, s.site_id, si.name AS site_name, s.relay_id,
                s.os_family, s.os_version, s.agent_version, s.created_at, s.last_seen_at,
                s.revoked_at
         FROM servers s
         JOIN sites si ON si.id = s.site_id
         WHERE $1 OR s.site_id = ANY($2)
         ORDER BY si.name, s.hostname, s.peer_id",
    )
    .bind(all_sites)
    .bind(&sites)
    .fetch_all(&state.pool)
    .await?;

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
    Ok(Json(ServerList { servers }))
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
        }
    }
}
