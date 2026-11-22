//! `GET /api/servers` and `GET /api/servers/{id}`: managed machines from
//! the database, with their live connection status from the gRPC
//! [`Registry`](crate::grpc::Registry).
//!
//! Pages are keyset-paginated on the sort key, so a server added or
//! removed between two pages neither repeats nor skips the others.

use std::collections::BTreeMap;

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
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListParams {
    /// 1 to [`MAX_LIMIT`], default [`DEFAULT_LIMIT`].
    pub limit: Option<u32>,
    /// `next_cursor` of the previous page.
    pub cursor: Option<String>,
    /// Only the servers of this site; none outside the user's sites.
    pub site_id: Option<Uuid>,
}

/// Body of `GET /api/servers`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ServerList {
    /// By site name, hostname and name.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[schema(as = ServerStatus)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// The agent has an open stream to this server, directly or via a relay.
    Online,
    Offline,
    /// A device: it has no agent and is watched from a probe (ADR-0148).
    Device,
}

/// The one status of a server on the fleet's pages, worst first
/// (ADR-0130): a silence wins, then being unreachable behind a parent that
/// is down (ADR-0152), then being offline, then the worst open
/// incident nobody acknowledged; a server whose open incidents are all
/// acknowledged is `acknowledged`, the rest `ok`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ServerState {
    Critical,
    Warning,
    Acknowledged,
    Silenced,
    Offline,
    Ok,
    /// Made by an import, waiting for its agent (ADR-0146).
    Waiting,
    /// Behind a parent that is down (ADR-0152): what it shows is stale.
    Unreachable,
}

impl ServerState {
    /// The state of a server `online` or not, `silenced` or not, with
    /// `open` incidents of which `unacknowledged` and `critical` (among the
    /// unacknowledged) nobody acknowledged.
    pub fn of(silenced: bool, online: bool, open: i64, unacknowledged: i64, critical: i64) -> Self {
        if silenced {
            Self::Silenced
        } else if !online && (unacknowledged > 0 || open == 0) {
            Self::Offline
        } else if critical > 0 {
            Self::Critical
        } else if unacknowledged > 0 {
            Self::Warning
        } else if open > 0 {
            Self::Acknowledged
        } else {
            Self::Ok
        }
    }
}

/// One server. Field names and types are part of the API contract: add
/// fields, never rename or remove them.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ServerView {
    pub id: Uuid,
    /// The agent's name, given when it was installed; the `peer` label of
    /// its metrics.
    pub peer_id: String,
    /// The machine's host name, as its agent reports it.
    pub hostname: String,
    pub site_id: Uuid,
    pub site_name: String,
    /// Customer of the site (ADR-0026).
    pub customer_id: Uuid,
    pub customer_name: String,
    /// The relay its last connection came through; `null` when direct.
    pub relay_id: Option<Uuid>,
    /// `debian`, `rhel`, `windows`, or `linux` for any other distribution;
    /// `device` for a device.
    pub os_family: String,
    /// `agent`: the machine runs the agent; `device`: an address without
    /// one, watched by network checks from a probe (ADR-0148).
    #[schema(example = "agent")]
    pub kind: String,
    /// A device's address, which its checks reach; `null` for an agent.
    pub address: Option<String>,
    /// Like `12` or `9.4`; `null` until the agent reports it.
    pub os_version: Option<String>,
    /// Whether the host can take package updates and sources (ADR-0063).
    pub package_updates: bool,
    /// What its agent last said it does with each command kind: `accepted`,
    /// `off` or `unsupported` (ADR-0076). `null`, or a kind missing here, is
    /// unknown: its agent or a relay on the way does not report yet.
    pub command_kinds: Option<BTreeMap<String, String>>,
    /// Version reported by the live stream, else the one last stored.
    pub agent_version: Option<String>,
    /// When it joined.
    pub created_at: DateTime<Utc>,
    /// When it was removed; `null` while it is in the fleet.
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
    /// Its one status on the fleet's pages (ADR-0130).
    pub state: ServerState,
    /// Whether it opens an incident when offline (ADR-0072); muted when
    /// this or `site_offline_alerts` is `false`.
    pub offline_alerts: bool,
    /// The site's own setting.
    pub site_offline_alerts: bool,
    /// While a silence covers it, when the last one ends (ADR-0079).
    pub silenced_until: Option<DateTime<Utc>>,
    /// That silence: who made it and why (ADR-0130); `null` without one.
    pub silence: Option<SilenceNote>,
    /// The latest acknowledgement of its open incidents (ADR-0130);
    /// `null` when none is acknowledged.
    pub acknowledgement: Option<AcknowledgementNote>,
    /// Latest expiry among the agent's unrevoked certificates; `null` when
    /// it has none. Past dates mean every certificate has expired.
    pub cert_not_after: Option<DateTime<Utc>>,
    /// Made by an import and waiting for its agent, which takes it over
    /// by enrolling with `peer_id` (ADR-0146).
    pub awaiting_agent: bool,
    /// While a parent it depends on is down (ADR-0152): which, and its
    /// incident; `null` otherwise.
    pub unreachable: Option<super::dependencies::UnreachableNote>,
}

/// Body of `GET /api/servers/{id}`: the listing row plus the agent's
/// current certificate.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ServerDetail {
    #[serde(flatten)]
    pub server: ServerView,
    /// The certificate issued last to this agent, revoked or not; `null`
    /// when the CA never issued one (a server row created by hand).
    pub certificate: Option<CertificateView>,
    /// The last time AI commands were turned on or off from the web
    /// interface, `null` if never (ADR-0098).
    pub ai_commands_change: Option<super::ai_commands::AiCommandsChange>,
}

/// A certificate from the `certificates` table.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct CertificateView {
    /// Lowercase hexadecimal without leading zeros.
    pub serial: String,
    /// Start of validity.
    pub not_before: DateTime<Utc>,
    /// Expiry.
    pub not_after: DateTime<Utc>,
    pub issued_at: DateTime<Utc>,
    pub revoked: bool,
    /// `null` unless revoked.
    pub revoked_at: Option<DateTime<Utc>>,
    /// `unspecified`, `key_compromise`, `affiliation_changed`,
    /// `superseded` or `cessation_of_operation`; `null` unless revoked.
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

/// The open incidents of server `s`, as `o`: all of them, those nobody
/// acknowledged, and the critical ones among those.
macro_rules! open_counts {
    () => {
        "CROSS JOIN LATERAL (
         SELECT count(*) AS open_incidents,
                count(*) FILTER (WHERE a.incident_id IS NULL) AS unacknowledged,
                count(*) FILTER (WHERE a.incident_id IS NULL
                                   AND i.severity = 'critical') AS unacknowledged_critical
         FROM incidents i
         LEFT JOIN incident_acknowledgements a ON a.incident_id = i.id
         -- One probe of the partial unique index on open incidents.
         WHERE i.server_id = s.id AND i.state = 'open') o"
    };
}

/// Columns of [`Row`]; callers add the `WHERE` clause.
const SELECT_SERVERS: &str = concat!(
    "SELECT s.id, s.peer_id, s.hostname, s.site_id, si.name AS site_name,
            si.customer_id, c.name AS customer_name, s.relay_id,
            s.os_family, s.os_version, s.package_updates, s.command_kinds, s.agent_version,
            s.created_at, s.last_seen_at, s.awaiting_agent, s.kind, s.address,
            s.revoked_at, s.offline_alerts, si.offline_alerts AS site_offline_alerts,
            silenced_until(s.id, NULL, s.site_id, si.customer_id) AS silenced_until,
            o.open_incidents, o.unacknowledged, o.unacknowledged_critical,
            sl.reason AS silence_reason, sl.username AS silence_by,
            sl.window_id IS NOT NULL AS silence_window,
            ak.username AS acknowledged_by, ak.acknowledged_at, ak.note AS acknowledged_note,
            -- One probe of the partial index on active certificates.
            (SELECT max(c.not_after) FROM certificates c
             WHERE c.peer_role = 'agent' AND c.peer_id = s.peer_id
               AND c.revoked_at IS NULL) AS cert_not_after
     FROM servers s
     JOIN sites si ON si.id = s.site_id
     JOIN customers c ON c.id = si.customer_id
     ",
    open_counts!(),
    "
     -- The silence covering it that ends last, as silenced_until picks.
     LEFT JOIN LATERAL (
         SELECT x.reason, u.username, x.window_id FROM silences x
         LEFT JOIN users u ON u.id = x.created_by
         WHERE x.starts_at <= now() AND coalesce(x.ended_at, x.ends_at) > now()
           AND x.checks IS NULL
           AND (num_nonnulls(x.server_id, x.relay_id, x.site_id, x.customer_id) = 0
                OR x.server_id = s.id OR x.site_id = s.site_id
                OR x.customer_id = si.customer_id)
         ORDER BY x.ends_at DESC LIMIT 1) sl ON true
     -- The latest acknowledgement of its open incidents.
     LEFT JOIN LATERAL (
         SELECT a.username, a.acknowledged_at, a.note FROM incidents i
         JOIN incident_acknowledgements a ON a.incident_id = i.id
         WHERE i.server_id = s.id AND i.state = 'open'
         ORDER BY a.acknowledged_at DESC LIMIT 1) ak ON true"
);

/// Who silenced a server, until when and why (ADR-0130).
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct SilenceNote {
    /// Who made the silence; `null` once the account is removed.
    pub by: Option<String>,
    pub until: DateTime<Utc>,
    pub reason: String,
    /// An occurrence of a maintenance window (ADR-0151): "In maintenance
    /// until 04:00".
    pub window: bool,
}

/// Who acknowledged a server's latest acknowledged open incident, when,
/// and what they said they are doing (ADR-0130).
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct AcknowledgementNote {
    pub by: String,
    pub at: DateTime<Utc>,
    /// `null` from a notification link or a script that sent none.
    pub note: Option<String>,
}

/// Query string of `GET /api/servers/summary`.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SummaryParams {
    /// Only the servers of this site.
    pub site_id: Option<Uuid>,
    /// Only the servers of this customer's sites.
    pub customer_id: Option<Uuid>,
}

/// Body of `GET /api/servers/summary`: how many servers are in each
/// [`ServerState`], out of `total`.
#[derive(Debug, Default, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct ServerSummary {
    pub critical: u64,
    pub warning: u64,
    pub acknowledged: u64,
    pub silenced: u64,
    pub offline: u64,
    pub ok: u64,
    /// Made by an import and waiting for their agent (ADR-0146).
    pub waiting: u64,
    /// Behind a parent that is down (ADR-0152).
    pub unreachable: u64,
    pub total: u64,
}

impl ServerSummary {
    fn add(&mut self, state: ServerState) {
        let count = match state {
            ServerState::Critical => &mut self.critical,
            ServerState::Warning => &mut self.warning,
            ServerState::Acknowledged => &mut self.acknowledged,
            ServerState::Silenced => &mut self.silenced,
            ServerState::Offline => &mut self.offline,
            ServerState::Ok => &mut self.ok,
            ServerState::Waiting => &mut self.waiting,
            ServerState::Unreachable => &mut self.unreachable,
        };
        *count += 1;
        self.total += 1;
    }
}

#[derive(sqlx::FromRow)]
struct SummaryRow {
    id: Uuid,
    peer_id: String,
    awaiting_agent: bool,
    device: bool,
    silenced: bool,
    open_incidents: i64,
    unacknowledged: i64,
    unacknowledged_critical: i64,
}

/// `GET /api/servers/summary`: the servers the user may read, without
/// removed ones, counted by state, for the status strip of every page
/// (ADR-0130). Needs at least one role.
pub async fn summary(
    State(state): State<ApiState>,
    user: CurrentUser,
    params: Result<Query<SummaryParams>, QueryRejection>,
) -> Result<Json<ServerSummary>, ApiError> {
    let Query(params) = params.map_err(|r| ApiError::Invalid(r.body_text()))?;
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let (all_sites, sites) = user.readable_sites().bind();
    let rows: Vec<SummaryRow> = sqlx::query_as(concat!(
        "SELECT s.id, s.peer_id, s.awaiting_agent, s.kind = 'device' AS device,
                silenced_until(s.id, NULL, s.site_id, si.customer_id) IS NOT NULL AS silenced,
                o.open_incidents, o.unacknowledged, o.unacknowledged_critical
         FROM servers s
         JOIN sites si ON si.id = s.site_id
         ",
        open_counts!(),
        "
         WHERE ($1 OR s.site_id = ANY($2)) AND s.revoked_at IS NULL
           AND ($3::uuid IS NULL OR s.site_id = $3)
           AND ($4::uuid IS NULL OR si.customer_id = $4)",
    ))
    .bind(all_sites)
    .bind(&sites)
    .bind(params.site_id)
    .bind(params.customer_id)
    .fetch_all(&state.pool)
    .await?;
    let holds = {
        let mut conn = state.pool.acquire().await?;
        super::dependencies::holds(&state, &mut conn).await?
    };
    let mut summary = ServerSummary::default();
    for row in rows {
        summary.add(row_state(&state, &holds, &row));
    }
    Ok(Json(summary))
}

/// The one state of a server of the summary (ADR-0130).
fn row_state(
    state: &ApiState,
    holds: &crate::dependencies::Holds,
    row: &SummaryRow,
) -> ServerState {
    if row.awaiting_agent {
        return ServerState::Waiting;
    }
    if !row.silenced && super::dependencies::unreachable(holds, row.id) {
        return ServerState::Unreachable;
    }
    // A device is never offline: its checks say what is wrong.
    let online = row.device || state.registry.live(PeerRole::Agent, &row.peer_id).is_some();
    ServerState::of(
        row.silenced,
        online,
        row.open_incidents,
        row.unacknowledged,
        row.unacknowledged_critical,
    )
}

/// The state of each of `ids`, as the fleet's pages show it (ADR-0130),
/// for dashboard panels (ADR-0166). The caller has checked they may be
/// read.
pub(super) async fn states_of(
    state: &ApiState,
    ids: &[Uuid],
) -> Result<std::collections::HashMap<Uuid, ServerState>, ApiError> {
    if ids.is_empty() {
        return Ok(Default::default());
    }
    let rows: Vec<SummaryRow> = sqlx::query_as(concat!(
        "SELECT s.id, s.peer_id, s.awaiting_agent, s.kind = 'device' AS device,
                silenced_until(s.id, NULL, s.site_id, si.customer_id) IS NOT NULL AS silenced,
                o.open_incidents, o.unacknowledged, o.unacknowledged_critical
         FROM servers s
         JOIN sites si ON si.id = s.site_id
         ",
        open_counts!(),
        "
         WHERE s.id = ANY($1)",
    ))
    .bind(ids)
    .fetch_all(&state.pool)
    .await?;
    let holds = {
        let mut conn = state.pool.acquire().await?;
        super::dependencies::holds(state, &mut conn).await?
    };
    Ok(rows
        .iter()
        .map(|row| (row.id, row_state(state, &holds, row)))
        .collect())
}

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
    command_kinds: Option<sqlx::types::Json<BTreeMap<String, String>>>,
    agent_version: Option<String>,
    created_at: DateTime<Utc>,
    last_seen_at: Option<DateTime<Utc>>,
    revoked_at: Option<DateTime<Utc>>,
    offline_alerts: bool,
    site_offline_alerts: bool,
    silenced_until: Option<DateTime<Utc>>,
    open_incidents: i64,
    unacknowledged: i64,
    unacknowledged_critical: i64,
    silence_reason: Option<String>,
    silence_by: Option<String>,
    silence_window: bool,
    acknowledged_by: Option<String>,
    acknowledged_at: Option<DateTime<Utc>>,
    acknowledged_note: Option<String>,
    cert_not_after: Option<DateTime<Utc>>,
    awaiting_agent: bool,
    kind: String,
    address: Option<String>,
}

/// The statement of [`list`]: `$1` all sites or `$2` the readable ones,
/// the cursor's site name, hostname and peer ID in `$3` to `$5`, the
/// limit in `$6`, a site in `$7`. The page's servers are picked first,
/// then their incidents, silences and certificates are read: read for
/// the whole site and sorted after, they cost every page as much as the
/// site's every server, so listing a site of 2,240 servers took six times
/// that (docs/load-test.md).
pub fn list_query() -> String {
    // SQL-safe: interpolates only the constant SELECT_SERVERS.
    format!(
        "{SELECT_SERVERS}
         WHERE s.id = ANY(ARRAY(
             SELECT p.id FROM servers p JOIN sites ps ON ps.id = p.site_id
             WHERE ($1 OR p.site_id = ANY($2)) AND p.revoked_at IS NULL
               AND ($3::text IS NULL OR (ps.name, p.hostname, p.peer_id) > ($3, $4, $5))
               AND ($7::uuid IS NULL OR p.site_id = $7)
             ORDER BY ps.name, p.hostname, p.peer_id
             LIMIT $6))
         ORDER BY si.name, s.hostname, s.peer_id"
    )
}

/// `GET /api/servers`: one page of the servers of the sites the user may
/// read, sorted by site name, hostname and peer ID, without removed ones
/// (ADR-0062). Needs at least one role.
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
    let mut rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(list_query()))
        .bind(all_sites)
        .bind(&sites)
        .bind(after_site)
        .bind(after_host)
        .bind(after_peer)
        .bind(i64::from(limit) + 1)
        .bind(params.site_id)
        .fetch_all(&state.pool)
        .await?;
    let next_cursor = if rows.len() > limit as usize {
        rows.truncate(limit as usize);
        rows.last()
            .map(|r| encode_cursor(&(&r.site_name, &r.hostname, &r.peer_id)))
    } else {
        None
    };

    let mut servers: Vec<ServerView> = rows
        .into_iter()
        .map(|row| ServerView::new(&state, row))
        .collect();
    super::dependencies::mark_unreachable(&state, &mut servers).await?;
    Ok(Json(ServerList {
        servers,
        next_cursor,
    }))
}

/// `GET /api/servers/{id}`: one server the user may read, with its
/// current certificate. Removed servers stay readable, for their history.
/// Outside the user's sites it is 404.
pub async fn detail(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<ServerDetail>, ApiError> {
    let id = parse_id(&id)?;
    // SQL-safe: interpolates only the constant SELECT_SERVERS.
    let row: Option<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SELECT_SERVERS} WHERE s.id = $1"
    )))
    .bind(id)
    .fetch_optional(&state.pool)
    .await?;
    authorize(&user, row.as_ref().map(|r| r.site_id), false)?;
    let row = row.ok_or(ApiError::NotFound)?;
    let certificate = latest_certificate(&state.pool, PeerRole::Agent, &row.peer_id).await?;
    let ai_commands_change = super::ai_commands::last_change(&state.pool, row.id).await?;
    let mut server = [ServerView::new(&state, row)];
    super::dependencies::mark_unreachable(&state, &mut server).await?;
    let [server] = server;
    Ok(Json(ServerDetail {
        server,
        certificate,
        ai_commands_change,
    }))
}

/// The listing row of the server `id`, removed or not, without access
/// checks.
pub(super) async fn view(state: &ApiState, id: Uuid) -> Result<Option<ServerView>, ApiError> {
    // SQL-safe: interpolates only the constant SELECT_SERVERS.
    let row: Option<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SELECT_SERVERS} WHERE s.id = $1"
    )))
    .bind(id)
    .fetch_optional(&state.pool)
    .await?;
    Ok(row.map(|row| ServerView::new(state, row)))
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
        // On any server instance (ADR-0089).
        let live = state.registry.live(PeerRole::Agent, &row.peer_id);
        if row.kind == "device" {
            return Self::device(row);
        }
        match live {
            Some(peer) => Self {
                status: Status::Online,
                state: ServerState::of(
                    row.silenced_until.is_some(),
                    true,
                    row.open_incidents,
                    row.unacknowledged,
                    row.unacknowledged_critical,
                ),
                connected_at: Some(peer.connected_at),
                last_heartbeat: Some(peer.last_seen),
                via_relay: peer.via_relay,
                agent_version: Some(peer.agent_version).filter(|v| !v.is_empty()),
                ..Self::offline(row)
            },
            None => Self::offline(row),
        }
    }

    /// A device: its state from its incidents alone, as if online.
    fn device(row: Row) -> Self {
        Self {
            status: Status::Device,
            state: ServerState::of(
                row.silenced_until.is_some(),
                true,
                row.open_incidents,
                row.unacknowledged,
                row.unacknowledged_critical,
            ),
            ..Self::offline(row)
        }
    }

    fn offline(row: Row) -> Self {
        Self {
            state: if row.awaiting_agent {
                ServerState::Waiting
            } else {
                ServerState::of(
                    row.silenced_until.is_some(),
                    false,
                    row.open_incidents,
                    row.unacknowledged,
                    row.unacknowledged_critical,
                )
            },
            awaiting_agent: row.awaiting_agent,
            id: row.id,
            peer_id: row.peer_id,
            hostname: row.hostname,
            site_id: row.site_id,
            site_name: row.site_name,
            customer_id: row.customer_id,
            customer_name: row.customer_name,
            relay_id: row.relay_id,
            os_family: row.os_family,
            kind: row.kind,
            address: row.address,
            os_version: row.os_version,
            package_updates: row.package_updates,
            command_kinds: row.command_kinds.map(|kinds| kinds.0),
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
            silence: row
                .silenced_until
                .zip(row.silence_reason)
                .map(|(until, reason)| SilenceNote {
                    by: row.silence_by,
                    until,
                    reason,
                    window: row.silence_window,
                }),
            acknowledgement: row
                .acknowledged_by
                .zip(row.acknowledged_at)
                .map(|(by, at)| AcknowledgementNote {
                    by,
                    at,
                    note: row.acknowledged_note,
                }),
            silenced_until: row.silenced_until,
            cert_not_after: row.cert_not_after,
            unreachable: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_state_per_server_worst_first() {
        use ServerState::*;
        // (silenced, online, open, unacknowledged, critical)
        let cases = [
            ((true, false, 3, 3, 1), Silenced),
            ((false, false, 0, 0, 0), Offline),
            ((false, false, 1, 1, 1), Offline),
            ((false, false, 1, 0, 0), Acknowledged),
            ((false, true, 2, 2, 1), Critical),
            ((false, true, 2, 1, 0), Warning),
            ((false, true, 2, 0, 0), Acknowledged),
            ((false, true, 0, 0, 0), Ok),
        ];
        for ((silenced, online, open, unacknowledged, critical), want) in cases {
            assert_eq!(
                ServerState::of(silenced, online, open, unacknowledged, critical),
                want,
                "{silenced} {online} {open} {unacknowledged} {critical}"
            );
        }
        let mut summary = ServerSummary::default();
        for state in [Critical, Ok, Ok, Offline] {
            summary.add(state);
        }
        assert_eq!(
            summary,
            ServerSummary {
                critical: 1,
                offline: 1,
                ok: 2,
                total: 4,
                ..Default::default()
            }
        );
    }

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
