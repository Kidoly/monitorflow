//! `GET /api/relays` and `GET /api/relays/{id}` (ADR-0029): enrolled relays
//! from the database, with their live connection from the gRPC
//! [`Registry`](crate::grpc::Registry) and the agents they carry, their
//! version and self-updates; `POST /api/relays/{id}/update` updates one
//! (ADR-0055).

use std::collections::HashMap;

use std::time::Duration;

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use monitorflow_common::release::Version;
use monitorflow_common::tls::PeerRole;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::access::{authorize, authorize_admin};
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::commands::{MAX_HEALTH_TIMEOUT_S, MIN_HEALTH_TIMEOUT_S};
use super::sensors::{body, parse_id};
use super::servers::{
    decode_cursor, encode_cursor, latest_certificate, page_limit, CertificateView, ListParams,
    Status, DEFAULT_LIMIT, MAX_LIMIT,
};
use super::{ApiError, ApiState};
use crate::relay_commands::{self, RelayCommand};

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
    /// Base URL of the relay's mirror listener as it last reported it;
    /// `null` when it serves no mirrors (ADR-0037).
    pub mirror_url: Option<String>,
    /// When the relay last reported its mirrors.
    pub mirror_reported_at: Option<DateTime<Utc>>,
    /// The version of its last Hello; `null` before it connected with a
    /// server that records it (ADR-0055).
    pub relay_version: Option<String>,
    /// Its latest self-update.
    pub last_update: Option<RelayCommand>,
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
    /// Its latest self-updates, newest first.
    pub updates: Vec<RelayCommand>,
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
            r.revoked_at, r.mirror_url, r.mirror_reported_at, r.relay_version,
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
    mirror_url: Option<String>,
    mirror_reported_at: Option<DateTime<Utc>>,
    relay_version: Option<String>,
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
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let mut latest: HashMap<Uuid, RelayCommand> = relay_commands::latest(&state.pool, &ids)
        .await?
        .into_iter()
        .map(|c| (c.relay_id, c))
        .collect();
    let relays = rows
        .into_iter()
        .map(|row| {
            let last = latest.remove(&row.id);
            RelayView::new(&state, &forwarded, row, last)
        })
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
    let updates = relay_commands::recent(&state.pool, id, UPDATES).await?;
    Ok(Json(RelayDetail {
        relay: RelayView::new(&state, &forwarded, row, updates.first().cloned()),
        certificate,
        servers,
        updates,
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
    fn new(
        state: &ApiState,
        forwarded: &HashMap<String, usize>,
        row: Row,
        last_update: Option<RelayCommand>,
    ) -> Self {
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
            mirror_url: row.mirror_url,
            mirror_reported_at: row.mirror_reported_at,
            relay_version: row.relay_version,
            last_update,
        }
    }
}

/// Self-updates listed on a relay's page.
const UPDATES: i64 = 10;
/// How long a relay update may wait for its relay.
const UPDATE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Body of `POST /api/relays/{id}/update`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateRequest {
    /// A version the server holds.
    pub version: String,
    #[serde(default)]
    pub allow_downgrade: bool,
    /// Seconds the new relay has to connect; 0 or absent for 300.
    #[serde(default)]
    pub health_timeout_s: u32,
}

/// `POST /api/relays/{id}/update`: administrators of the relay's customer
/// queue a self-update (ADR-0055); 409 while one is in flight. Audited as
/// `relay.update`.
pub async fn update(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<UpdateRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<RelayCommand>), ApiError> {
    let id = parse_id(&id)?;
    let relay: Option<(Uuid, String, bool)> =
        sqlx::query_as("SELECT site_id, peer_id, revoked_at IS NOT NULL FROM relays WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.pool)
            .await?;
    authorize_admin(&user, relay.as_ref().map(|r| r.0))?;
    let (_, peer_id, revoked) = relay.ok_or(ApiError::NotFound)?;
    if revoked {
        return Err(ApiError::Conflict("the relay is revoked".into()));
    }
    let request = body(request)?;
    let version: Version = request
        .version
        .parse()
        .map_err(|_| ApiError::Invalid("version must be like 0.2.0".into()))?;
    let health = request.health_timeout_s;
    if health != 0 && !(MIN_HEALTH_TIMEOUT_S..=MAX_HEALTH_TIMEOUT_S).contains(&health) {
        return Err(ApiError::Invalid(format!(
            "health_timeout_s must be between {MIN_HEALTH_TIMEOUT_S} and {MAX_HEALTH_TIMEOUT_S}"
        )));
    }
    let payload =
        state
            .settings
            .agent_releases
            .payload(&version, None, request.allow_downgrade, health);
    let mut tx = state.pool.begin().await?;
    let Some(command) = relay_commands::enqueue_in(
        &mut tx,
        id,
        &version.to_string(),
        &payload,
        UPDATE_TTL,
        Some(user.id),
    )
    .await?
    else {
        return Err(ApiError::Conflict(
            "the relay has an update in flight".into(),
        ));
    };
    let actor = user.id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: "relay.update",
            target: Some(("relay", &id.to_string())),
            details: json!({
                "command_id": command.id,
                "peer_id": peer_id,
                "version": version.to_string(),
                "allow_downgrade": request.allow_downgrade,
            }),
        },
    )
    .await?;
    tx.commit().await?;
    if let Some(commands) = &state.relay_commands {
        commands.dispatch(&peer_id);
    }
    Ok((StatusCode::ACCEPTED, Json(command)))
}
