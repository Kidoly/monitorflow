//! `GET /api/relays` and `GET /api/relays/{id}` (ADR-0029): enrolled relays
//! from the database, with their live connection from the gRPC
//! [`Registry`](crate::grpc::Registry) and the agents they carry.

use std::collections::HashMap;

use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::{DateTime, Utc};
use monitorflow_common::tls::PeerRole;
use serde::Serialize;
use uuid::Uuid;

use super::access::authorize;
use super::auth::CurrentUser;
use super::sensors::parse_id;
use super::servers::{
    decode_cursor, encode_cursor, latest_certificate, page_limit, CertificateView, ListParams,
    Status, DEFAULT_LIMIT, MAX_LIMIT,
};
use super::{ApiError, ApiState};

/// Body of `GET /api/relays`, sorted by site name and peer ID.
#[derive(Debug, Serialize)]
pub struct RelayList {
    pub relays: Vec<RelayView>,
    /// Pass as `cursor` for the next page; `null` on the last one.
    pub next_cursor: Option<String>,
}

/// One relay. Add fields, never rename or remove them.
#[derive(Debug, Serialize)]
pub struct RelayView {
    pub id: Uuid,
    /// The relay certificate ID.
    pub peer_id: String,
    pub name: String,
    pub site_id: Uuid,
    pub site_name: String,
    pub customer_id: Uuid,
    pub customer_name: String,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    /// `online` while the relay's own stream is open.
    pub status: Status,
    /// Start of the relay's own stream, when online.
    pub connected_at: Option<DateTime<Utc>>,
    /// Last message on its own stream, live or as last stored.
    pub last_heartbeat: Option<DateTime<Utc>>,
    /// Latest expiry among the relay's unrevoked certificates.
    pub cert_not_after: Option<DateTime<Utc>>,
    /// Names its certificate carries for agents, from its enrollment
    /// token (ADR-0025); renewal keeps them.
    pub server_names: Vec<String>,
    /// Servers whose latest connection came through this relay.
    pub server_count: i64,
    /// Agent streams the relay forwards right now.
    pub connected_agents: usize,
}

/// Body of `GET /api/relays/{id}`.
#[derive(Debug, Serialize)]
pub struct RelayDetail {
    #[serde(flatten)]
    pub relay: RelayView,
    /// The certificate issued last to the relay, revoked or not.
    pub certificate: Option<CertificateView>,
    /// Servers attached to the relay, by hostname.
    pub servers: Vec<AttachedServer>,
}

/// A server whose latest connection came through the relay.
#[derive(Debug, Serialize)]
pub struct AttachedServer {
    pub id: Uuid,
    pub peer_id: String,
    pub hostname: String,
    /// `online` while its stream goes through this relay.
    pub status: Status,
}

const SELECT_RELAYS: &str = "SELECT r.id, r.peer_id, r.name, r.site_id, si.name AS site_name,
            si.customer_id, c.name AS customer_name, r.created_at, r.last_seen_at,
            r.revoked_at,
            (SELECT max(ce.not_after) FROM certificates ce
             WHERE ce.peer_role = 'relay' AND ce.peer_id = r.peer_id
               AND ce.revoked_at IS NULL) AS cert_not_after,
            -- The latest consumed token, on the (peer_id, used_at) index.
            coalesce((SELECT t.server_names FROM enrollment_tokens t
                      WHERE t.role = 'relay' AND t.peer_id = r.peer_id
                        AND t.used_at IS NOT NULL
                      ORDER BY t.used_at DESC LIMIT 1), '{}') AS server_names,
            (SELECT count(*) FROM servers s WHERE s.relay_id = r.id) AS server_count
     FROM relays r
     JOIN sites si ON si.id = r.site_id
     JOIN customers c ON c.id = si.customer_id";

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    peer_id: String,
    name: String,
    site_id: Uuid,
    site_name: String,
    customer_id: Uuid,
    customer_name: String,
    created_at: DateTime<Utc>,
    last_seen_at: Option<DateTime<Utc>>,
    revoked_at: Option<DateTime<Utc>>,
    cert_not_after: Option<DateTime<Utc>>,
    server_names: Vec<String>,
    server_count: i64,
}

/// `GET /api/relays`: one page of the relays of the sites the user reads.
/// Needs at least one role.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
    params: Result<Query<ListParams>, QueryRejection>,
) -> Result<Json<RelayList>, ApiError> {
    let Query(params) = params.map_err(|r| ApiError::Invalid(r.body_text()))?;
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let (all_sites, sites) = user.readable_sites().bind();
    let limit = page_limit(params.limit, DEFAULT_LIMIT, MAX_LIMIT)?;
    let after: Option<(String, String)> =
        params.cursor.as_deref().map(decode_cursor).transpose()?;
    let (after_site, after_peer) = after.unzip();
    let mut rows: Vec<Row> = sqlx::query_as(&format!(
        "{SELECT_RELAYS}
         WHERE ($1 OR r.site_id = ANY($2))
           AND ($3::text IS NULL OR (si.name, r.peer_id) > ($3, $4))
         ORDER BY si.name, r.peer_id
         LIMIT $5"
    ))
    .bind(all_sites)
    .bind(&sites)
    .bind(after_site)
    .bind(after_peer)
    .bind(i64::from(limit) + 1)
    .fetch_all(&state.pool)
    .await?;
    let next_cursor = if rows.len() > limit as usize {
        rows.truncate(limit as usize);
        rows.last()
            .map(|r| encode_cursor(&(&r.site_name, &r.peer_id)))
    } else {
        None
    };
    let forwarded = forwarded_streams(&state);
    let relays = rows
        .into_iter()
        .map(|row| RelayView::new(&state, &forwarded, row))
        .collect();
    Ok(Json(RelayList {
        relays,
        next_cursor,
    }))
}

/// `GET /api/relays/{id}`: one relay the user reads, its certificate and
/// attached servers. Outside the user's sites it is 404.
pub async fn detail(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<RelayDetail>, ApiError> {
    let id = parse_id(&id)?;
    let row: Option<Row> = sqlx::query_as(&format!("{SELECT_RELAYS} WHERE r.id = $1"))
        .bind(id)
        .fetch_optional(&state.pool)
        .await?;
    authorize(&user, row.as_ref().map(|r| r.site_id), false)?;
    let row = row.ok_or(ApiError::NotFound)?;
    let certificate = latest_certificate(&state.pool, PeerRole::Relay, &row.peer_id).await?;
    let attached: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, peer_id, hostname FROM servers WHERE relay_id = $1
         ORDER BY hostname, peer_id",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await?;
    let servers = attached
        .into_iter()
        .map(|(id, peer_id, hostname)| {
            let live = state.registry.get(&peer_id).is_some_and(|peer| {
                peer.role == PeerRole::Agent && peer.via_relay.as_deref() == Some(&row.peer_id)
            });
            AttachedServer {
                id,
                peer_id,
                hostname,
                status: if live {
                    Status::Online
                } else {
                    Status::Offline
                },
            }
        })
        .collect();
    let forwarded = forwarded_streams(&state);
    Ok(Json(RelayDetail {
        relay: RelayView::new(&state, &forwarded, row),
        certificate,
        servers,
    }))
}

/// Live agent streams per relay peer ID.
fn forwarded_streams(state: &ApiState) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for peer in state.registry.connected() {
        if let Some(relay) = peer.via_relay {
            *counts.entry(relay).or_default() += 1;
        }
    }
    counts
}

impl RelayView {
    fn new(state: &ApiState, forwarded: &HashMap<String, usize>, row: Row) -> Self {
        let live = state
            .registry
            .get(&row.peer_id)
            .filter(|peer| peer.role == PeerRole::Relay);
        Self {
            status: if live.is_some() {
                Status::Online
            } else {
                Status::Offline
            },
            connected_at: live.as_ref().map(|peer| peer.connected_at),
            last_heartbeat: live.map(|peer| peer.last_seen).or(row.last_seen_at),
            connected_agents: forwarded.get(&row.peer_id).copied().unwrap_or(0),
            id: row.id,
            peer_id: row.peer_id,
            name: row.name,
            site_id: row.site_id,
            site_name: row.site_name,
            customer_id: row.customer_id,
            customer_name: row.customer_name,
            created_at: row.created_at,
            revoked_at: row.revoked_at,
            cert_not_after: row.cert_not_after,
            server_names: row.server_names,
            server_count: row.server_count,
        }
    }
}
