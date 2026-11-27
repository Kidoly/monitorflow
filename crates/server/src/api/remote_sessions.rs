//! Remote terminals and desktops from a server's page (ADR-0190): who
//! may open them, starting one, its WebSocket, the list of past sessions
//! and their recordings: terminals' transcripts, desktops' pictures
//! (ADR-0192).
//!
//! Starting a session answers a one-time token, valid one minute, which
//! the browser (or any API client) presents as the first message of the
//! session's WebSocket. Only then is the signed command queued, by the
//! instance holding that WebSocket, which runs the session
//! ([`crate::remote`]). A desktop's WebSocket starts with IronRDP's
//! RDCleanPath request instead, the token in it
//! ([`crate::remote::desktop`]).

use std::time::Duration;

use axum::extract::rejection::JsonRejection;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::Engine;
use chrono::{DateTime, Utc};
use futures_util::{SinkExt, StreamExt};
use monitorflow_common::command::{REMOTE_DESKTOP, REMOTE_SESSION};
use monitorflow_common::tls::PeerRole;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::access::authorize;
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::one_time::{hash_of, random_hex};
use super::sensors::{body, parse_id};
use super::server_update::require_global_admin;
use super::{ApiError, ApiState, Role};
use crate::remote::desktop::{self, DesktopError};
use crate::remote::{FromBrowser, ToBrowser, TOKEN_TTL};

/// Time the browser has to send its token once the WebSocket is open.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// Largest WebSocket message: a paste, at most.
const MAX_MESSAGE: usize = 64 * 1024;
/// Sessions listed per server.
const LIST_LIMIT: i64 = 100;

/// Who may open remote terminals and desktops, and how sessions run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoteAccessSettings {
    /// The lowest role, on a server's site, that may open a terminal or a
    /// desktop there; `null` lets nobody. Viewers never may.
    pub min_role: Option<Role>,
    /// Whether sessions are recorded, for replay by the site's
    /// administrators: what terminals print, and the screen of desktops.
    pub record: bool,
    /// A session closes after this many minutes without input, 1 to 1440.
    pub idle_timeout_minutes: i32,
    /// Recordings older than this many days are deleted, 1 to 3650.
    pub retention_days: i32,
}

#[derive(sqlx::FromRow)]
struct SettingsRow {
    min_role: Option<String>,
    record: bool,
    idle_timeout_minutes: i32,
    retention_days: i32,
}

pub(super) async fn settings(pool: &sqlx::PgPool) -> Result<RemoteAccessSettings, ApiError> {
    let row: Option<SettingsRow> = sqlx::query_as(
        "SELECT min_role, record, idle_timeout_minutes, retention_days FROM remote_access_settings",
    )
    .fetch_optional(pool)
    .await?;
    Ok(match row {
        Some(row) => RemoteAccessSettings {
            min_role: row.min_role.and_then(|r| r.parse().ok()),
            record: row.record,
            idle_timeout_minutes: row.idle_timeout_minutes,
            retention_days: row.retention_days,
        },
        None => RemoteAccessSettings {
            min_role: None,
            record: true,
            idle_timeout_minutes: 15,
            retention_days: 90,
        },
    })
}

/// Whether `user` may open a terminal on a server of `site`.
fn may_open(user: &CurrentUser, site: Uuid, settings: &RemoteAccessSettings) -> bool {
    settings
        .min_role
        .is_some_and(|role| role > Role::Viewer && user.sites_with(role).contains(site))
}

/// Whether `user` may see the sessions of a server of `site`: those who
/// may open them, and the site's administrators.
fn may_review(user: &CurrentUser, site: Uuid, settings: &RemoteAccessSettings) -> bool {
    may_open(user, site, settings) || user.sites_with(Role::Administrator).contains(site)
}

/// `GET /api/settings/remote-access`: global administrators.
pub async fn get_settings(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<RemoteAccessSettings>, ApiError> {
    require_global_admin(&user)?;
    Ok(Json(settings(&state.pool).await?))
}

/// `PUT /api/settings/remote-access`: replaces the settings.
pub async fn set_settings(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<RemoteAccessSettings>, JsonRejection>,
) -> Result<Json<RemoteAccessSettings>, ApiError> {
    require_global_admin(&user)?;
    let request = body(request)?;
    if request.min_role == Some(Role::Viewer) {
        return Err(ApiError::Invalid(
            "viewers only read; choose operator, approver, administrator or null".into(),
        ));
    }
    if !(1..=1440).contains(&request.idle_timeout_minutes) {
        return Err(ApiError::Invalid(
            "idle_timeout_minutes must be between 1 and 1440".into(),
        ));
    }
    if !(1..=3650).contains(&request.retention_days) {
        return Err(ApiError::Invalid(
            "retention_days must be between 1 and 3650".into(),
        ));
    }
    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "INSERT INTO remote_access_settings (id, min_role, record, idle_timeout_minutes, retention_days)
         VALUES (true, $1, $2, $3, $4)
         ON CONFLICT (id) DO UPDATE SET min_role = $1, record = $2, idle_timeout_minutes = $3,
             retention_days = $4, updated_at = now()",
    )
    .bind(request.min_role.map(Role::as_str))
    .bind(request.record)
    .bind(request.idle_timeout_minutes)
    .bind(request.retention_days)
    .execute(&mut *tx)
    .await?;
    let actor = user.id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: "settings.remote_access",
            target: None,
            details: json!({
                "min_role": request.min_role.map(Role::as_str),
                "record": request.record,
                "idle_timeout_minutes": request.idle_timeout_minutes,
                "retention_days": request.retention_days,
            }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(Json(request))
}

/// What `GET /api/servers/{id}` says about remote terminals for the
/// person asking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct RemoteTerminalAccess {
    /// Whether this person's role lets them open a terminal here. The
    /// agent must also allow it: `command_kinds.remote_session` is
    /// `accepted`.
    pub can_open: bool,
    /// Whether this person may see the past sessions of this server.
    pub can_review: bool,
    /// Whether new terminals and desktops are recorded.
    pub recorded: bool,
    pub idle_timeout_minutes: i32,
}

pub(super) async fn access(
    state: &ApiState,
    user: &CurrentUser,
    site: Uuid,
) -> Result<RemoteTerminalAccess, ApiError> {
    let settings = settings(&state.pool).await?;
    Ok(RemoteTerminalAccess {
        can_open: may_open(user, site, &settings),
        can_review: may_review(user, site, &settings),
        recorded: settings.record,
        idle_timeout_minutes: settings.idle_timeout_minutes,
    })
}

/// A remote session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct RemoteSession {
    pub id: Uuid,
    pub server_id: Uuid,
    pub hostname: String,
    /// `terminal` or `desktop`.
    pub kind: String,
    /// The username of who opened it, kept when the account goes.
    pub requested_by: String,
    pub created_at: DateTime<Utc>,
    /// When the agent opened the terminal or reached the host's RDP
    /// service; `null` if it never did.
    pub opened_at: Option<DateTime<Utc>>,
    /// `null` while it is open.
    pub closed_at: Option<DateTime<Utc>>,
    /// Why it ended, in plain words; `null` while it is open.
    pub close_reason: Option<String>,
    /// From opening to closing, or to now while open; `null` if it never
    /// opened.
    pub duration_seconds: Option<i64>,
    pub recorded: bool,
    /// Bytes typed, or sent to the RDP service.
    pub bytes_in: i64,
    /// Bytes the terminal printed, or the RDP service sent.
    pub bytes_out: i64,
}

const SESSION_COLUMNS: &str = "r.id, r.server_id, s.hostname, r.kind, r.username AS requested_by,
     r.created_at, r.opened_at, r.closed_at, r.close_reason,
     CASE WHEN r.opened_at IS NULL THEN NULL
          ELSE extract(epoch FROM coalesce(r.closed_at, now()) - r.opened_at)::bigint END
         AS duration_seconds,
     r.recorded, r.bytes_in, r.bytes_out";

/// Body of `POST /api/servers/{id}/remote-sessions`.
#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StartRequest {
    /// `terminal`, the default, or `desktop`: the host's own RDP service,
    /// Windows only.
    #[serde(default)]
    pub kind: Option<String>,
}

/// Answer of `POST /api/servers/{id}/remote-sessions`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct RemoteSessionStart {
    pub session: RemoteSession,
    /// A terminal's: send it as the first WebSocket message, with the
    /// terminal's size, `{"token": "…", "cols": 120, "rows": 40}`. A
    /// desktop's: the `proxy_auth` of the RDCleanPath request that starts
    /// the WebSocket. It works once.
    pub token: String,
    pub token_expires_at: DateTime<Utc>,
    /// The WebSocket's path, `/api/v1/remote-sessions/{id}/socket` through
    /// the public API.
    pub socket: String,
}

/// A server as these routes need it.
#[derive(sqlx::FromRow)]
struct Host {
    id: Uuid,
    site_id: Uuid,
    peer_id: String,
    hostname: String,
    os_family: String,
    revoked: bool,
    acceptance: Option<String>,
    desktop_acceptance: Option<String>,
}

/// Why the agent would refuse, by what it reported (ADR-0076).
fn refusal(host: &Host, desktop: bool) -> Option<String> {
    if host.revoked {
        return Some("the server is removed".into());
    }
    if desktop {
        return desktop_refusal(host);
    }
    match host.acceptance.as_deref() {
        Some("accepted") => None,
        None => Some(
            "this server's agent cannot open remote terminals yet; it updates by itself".into(),
        ),
        Some("unsupported") => Some("this server's system cannot open remote terminals".into()),
        Some(_) => Some(not_allowed(&host.os_family).into()),
    }
}

fn desktop_refusal(host: &Host) -> Option<String> {
    if host.os_family != "windows" {
        return Some("remote desktops are for Windows servers; open a terminal instead".into());
    }
    match host.desktop_acceptance.as_deref() {
        Some("accepted") => None,
        None => {
            Some("this server's agent cannot open remote desktops yet; it updates by itself".into())
        }
        Some("unsupported") => Some("this server's system cannot open remote desktops".into()),
        Some(_) => Some(DESKTOP_NOT_ALLOWED.into()),
    }
}

/// Why a desktop is refused where the host did not allow it, with the
/// lines that allow it there.
pub const DESKTOP_NOT_ALLOWED: &str =
    "this server does not allow remote desktops; to allow them, run \
     `monitorflow-agent.exe remote-access desktop on` there as an administrator, then restart \
     the MonitorFlowAgent service";

/// Why a terminal is refused where the host did not allow it, with the
/// line that allows it there.
pub fn not_allowed(os_family: &str) -> &'static str {
    if os_family == "windows" {
        "this server does not allow remote terminals; to allow them, run \
         `monitorflow-agent.exe remote-access terminal on` there as an administrator, then \
         restart the MonitorFlowAgent service"
    } else {
        "this server does not allow remote terminals; to allow them, run the installer there \
         with MF_REMOTE_TERMINAL=1"
    }
}

/// `POST /api/servers/{id}/remote-sessions`: 201 with the session and its
/// one-time token.
pub async fn start(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<StartRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<RemoteSessionStart>), ApiError> {
    let id = parse_id(&id)?;
    let host: Option<Host> = sqlx::query_as(
        "SELECT id, site_id, peer_id, hostname, os_family, revoked_at IS NOT NULL AS revoked,
                command_kinds->>$2 AS acceptance, command_kinds->>$3 AS desktop_acceptance
         FROM servers WHERE id = $1 AND kind = 'agent'",
    )
    .bind(id)
    .bind(REMOTE_SESSION)
    .bind(REMOTE_DESKTOP)
    .fetch_optional(&state.pool)
    .await?;
    let site = authorize(&user, host.as_ref().map(|h| h.site_id), false)?;
    let host = host.ok_or(ApiError::NotFound)?;
    let settings = settings(&state.pool).await?;
    if !may_open(&user, site, &settings) {
        return Err(ApiError::Forbidden);
    }
    let kind = body(request)?.kind.unwrap_or_else(|| "terminal".into());
    let desktop = match kind.as_str() {
        "terminal" => false,
        "desktop" => true,
        _ => return Err(ApiError::Invalid("kind must be terminal or desktop".into())),
    };
    // A terminal's transcript, or a desktop's screen (ADR-0192).
    let recorded = settings.record;
    if let Some(refusal) = refusal(&host, desktop) {
        return Err(ApiError::Conflict(refusal));
    }
    if state.remote.is_none() {
        return Err(ApiError::Conflict(
            "this central server does not run remote sessions".into(),
        ));
    }
    if state
        .registry
        .live(PeerRole::Agent, &host.peer_id)
        .is_none()
    {
        return Err(ApiError::Conflict(
            "the server's agent is not connected now; try again once it is".into(),
        ));
    }
    let token = random_hex();
    let session = Uuid::new_v4();
    let expires = Utc::now() + chrono::Duration::from_std(TOKEN_TTL).expect("a minute");
    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "INSERT INTO remote_sessions (id, server_id, kind, requested_by, username, token_sha256,
             token_expires_at, recorded, idle_timeout_seconds)
         VALUES ($1, $2, $9, $3, $4, $5, $6, $7, $8)",
    )
    .bind(session)
    .bind(host.id)
    .bind(user.id)
    .bind(&user.username)
    .bind(hash_of(&token))
    .bind(expires)
    .bind(recorded)
    .bind(settings.idle_timeout_minutes * 60)
    .bind(&kind)
    .execute(&mut *tx)
    .await?;
    let actor = user.id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: "remote_session.start",
            target: Some(("server", &host.id.to_string())),
            details: json!({
                "session_id": session,
                "hostname": host.hostname,
                "peer_id": host.peer_id,
                "kind": kind,
                "recorded": recorded,
            }),
        },
    )
    .await?;
    tx.commit().await?;
    tracing::info!(user = %user.id, %session, peer_id = %host.peer_id, %kind, "remote session requested");
    let session = one(&state, session).await?.ok_or(ApiError::NotFound)?;
    let socket = format!("/api/v1/remote-sessions/{}/socket", session.id);
    Ok((
        StatusCode::CREATED,
        Json(RemoteSessionStart {
            session,
            token,
            token_expires_at: expires,
            socket,
        }),
    ))
}

async fn one(state: &ApiState, id: Uuid) -> Result<Option<RemoteSession>, ApiError> {
    // SQL-safe: interpolates only the constant SESSION_COLUMNS.
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {SESSION_COLUMNS} FROM remote_sessions r JOIN servers s ON s.id = r.server_id
         WHERE r.id = $1"
    )))
    .bind(id)
    .fetch_optional(&state.pool)
    .await?)
}

/// The session `id` with its server's site, once `user` may see it.
async fn reviewable(
    state: &ApiState,
    user: &CurrentUser,
    id: &str,
) -> Result<(RemoteSession, Uuid, Option<Uuid>), ApiError> {
    let id = parse_id(id)?;
    let place: Option<(Uuid, Option<Uuid>)> = sqlx::query_as(
        "SELECT s.site_id, r.requested_by FROM remote_sessions r JOIN servers s ON s.id = r.server_id
         WHERE r.id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?;
    let site = authorize(user, place.map(|p| p.0), false)?;
    let settings = settings(&state.pool).await?;
    if !may_review(user, site, &settings) {
        return Err(ApiError::Forbidden);
    }
    let session = one(state, id).await?.ok_or(ApiError::NotFound)?;
    Ok((session, site, place.and_then(|p| p.1)))
}

/// `GET /api/servers/{id}/remote-sessions`: the server's last sessions,
/// newest first.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<Vec<RemoteSession>>, ApiError> {
    let id = parse_id(&id)?;
    let site: Option<Uuid> = sqlx::query_scalar("SELECT site_id FROM servers WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?;
    let site = authorize(&user, site, false)?;
    let settings = settings(&state.pool).await?;
    if !may_review(&user, site, &settings) {
        return Err(ApiError::Forbidden);
    }
    // SQL-safe: interpolates only the constant SESSION_COLUMNS.
    let sessions = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {SESSION_COLUMNS} FROM remote_sessions r JOIN servers s ON s.id = r.server_id
         WHERE r.server_id = $1 ORDER BY r.created_at DESC, r.id LIMIT $2"
    )))
    .bind(id)
    .bind(LIST_LIMIT)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(sessions))
}

/// `GET /api/remote-sessions/{id}`.
pub async fn detail(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<RemoteSession>, ApiError> {
    Ok(Json(reviewable(&state, &user, &id).await?.0))
}

/// `POST /api/remote-sessions/{id}/close`: 204. Who opened it, or an
/// administrator of the server's site.
pub async fn close(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let (session, site, requested_by) = reviewable(&state, &user, &id).await?;
    let own = requested_by == Some(user.id);
    if !own && !user.sites_with(Role::Administrator).contains(site) {
        return Err(ApiError::Forbidden);
    }
    if session.closed_at.is_some() {
        return Err(ApiError::Conflict("the session is already closed".into()));
    }
    let reason = format!("closed by {}", user.username);
    // Not attached yet: nothing runs it, so the token stops working here.
    let unattached = sqlx::query(
        "UPDATE remote_sessions SET closed_at = now(), close_reason = $2
         WHERE id = $1 AND connected_at IS NULL AND closed_at IS NULL",
    )
    .bind(session.id)
    .bind(format!("{reason} before it opened"))
    .execute(&state.pool)
    .await?
    .rows_affected();
    if unattached == 0 {
        if let Some(remote) = &state.remote {
            remote.close(session.id, &user.username).await?;
        }
    }
    let actor = user.id.to_string();
    audit::record(
        &state.pool,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: "remote_session.close_request",
            target: Some(("server", &session.server_id.to_string())),
            details: json!({"session_id": session.id, "hostname": session.hostname}),
        },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// One write of a recording.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct RecordingChunk {
    /// Milliseconds after the session opened.
    pub at_ms: i64,
    /// What the terminal printed, base64: bytes as the shell wrote them,
    /// escape sequences included.
    pub data: String,
}

/// One picture of a desktop's recording (ADR-0192).
#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct DesktopFrame {
    /// Its number, for `GET /api/v1/remote-sessions/{id}/recording/frames/{seq}`.
    pub seq: i32,
    /// Milliseconds after the session opened.
    pub at_ms: i64,
    /// The part of the screen the picture covers, in pixels.
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// The size of the whole screen when it was taken. A picture as large
    /// as it starts the screen again.
    pub screen_width: i32,
    pub screen_height: i32,
    /// The size of its PNG, in bytes.
    pub bytes: i32,
}

/// Answer of `GET /api/remote-sessions/{id}/recording`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct RemoteRecording {
    pub session: RemoteSession,
    /// A terminal's, in order; replaying them into a terminal of the
    /// session's size shows what it showed. Empty for a desktop.
    pub chunks: Vec<RecordingChunk>,
    /// A desktop's pictures, in order; drawing each at its place, from a
    /// picture of the whole screen on, shows what the screen showed.
    /// Empty for a terminal.
    pub frames: Vec<DesktopFrame>,
}

/// The recorded session `id`, once `user` may read its recording: an
/// administrator of its server's site, as it may show anything the
/// session showed.
async fn recorded(
    state: &ApiState,
    user: &CurrentUser,
    id: &str,
) -> Result<RemoteSession, ApiError> {
    let (session, site, _) = reviewable(state, user, id).await?;
    if !user.sites_with(Role::Administrator).contains(site) {
        return Err(ApiError::Forbidden);
    }
    if !session.recorded {
        return Err(ApiError::NotFound);
    }
    Ok(session)
}

/// `GET /api/remote-sessions/{id}/recording`: what a recorded terminal
/// printed, or the pictures of a recorded desktop's screen.
pub async fn recording(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<RemoteRecording>, ApiError> {
    let session = recorded(&state, &user, &id).await?;
    let frames: Vec<DesktopFrame> = sqlx::query_as(
        "SELECT seq, at_ms, x, y, width, height, screen_width, screen_height,
                octet_length(image) AS bytes
         FROM remote_desktop_frames WHERE session_id = $1 ORDER BY seq",
    )
    .bind(session.id)
    .fetch_all(&state.pool)
    .await?;
    let rows: Vec<(i64, Vec<u8>)> = sqlx::query_as(
        "SELECT at_ms, output FROM remote_session_recordings WHERE session_id = $1 ORDER BY seq",
    )
    .bind(session.id)
    .fetch_all(&state.pool)
    .await?;
    let chunks = rows
        .into_iter()
        .map(|(at_ms, output)| RecordingChunk {
            at_ms,
            data: base64::engine::general_purpose::STANDARD.encode(output),
        })
        .collect();
    let actor = user.id.to_string();
    audit::record(
        &state.pool,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: "remote_session.recording_read",
            target: Some(("server", &session.server_id.to_string())),
            details: json!({"session_id": session.id, "hostname": session.hostname}),
        },
    )
    .await?;
    Ok(Json(RemoteRecording {
        session,
        chunks,
        frames,
    }))
}

/// `GET /api/remote-sessions/{id}/recording/frames/{seq}`: one picture of
/// a desktop's recording, a PNG. The same people as the recording; its
/// reading is audited there, once, not for each picture.
pub async fn frame(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path((id, seq)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let session = recorded(&state, &user, &id).await?;
    let seq: i32 = seq.parse().map_err(|_| ApiError::NotFound)?;
    let image: Option<Vec<u8>> = sqlx::query_scalar(
        "SELECT image FROM remote_desktop_frames WHERE session_id = $1 AND seq = $2",
    )
    .bind(session.id)
    .bind(seq)
    .fetch_optional(&state.pool)
    .await?;
    let mut response = image.ok_or(ApiError::NotFound)?.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    // A recording never changes.
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=86400, immutable"),
    );
    Ok(response)
}

/// The first WebSocket message.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello {
    token: String,
    cols: u32,
    rows: u32,
}

/// Text messages from the browser after the first.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
enum Control {
    Resize { cols: u32, rows: u32 },
    Close,
}

/// `GET /api/remote-sessions/{id}/socket`: the session's WebSocket. The
/// one-time token of the session is the first message; see
/// [`RemoteSessionStart::token`].
pub async fn socket(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let id = parse_id(&id)?;
    let Some(hub) = state.remote.clone() else {
        return Err(ApiError::NotFound);
    };
    Ok(upgrade
        .max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |socket| serve(hub, id, socket)))
}

fn event(kind: &str, reason: Option<&str>) -> Message {
    let mut value = json!({ "type": kind });
    if let Some(reason) = reason {
        value["reason"] = json!(reason);
    }
    Message::Text(value.to_string().into())
}

async fn refuse(mut socket: WebSocket, reason: &str) {
    let _ = socket.send(event("closed", Some(reason))).await;
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code: 4001,
            reason: "refused".into(),
        })))
        .await;
}

async fn serve(hub: crate::remote::RemoteHub, id: Uuid, mut socket: WebSocket) {
    match tokio::time::timeout(HELLO_TIMEOUT, socket.recv()).await {
        Ok(Some(Ok(Message::Text(text)))) => match serde_json::from_str::<Hello>(&text) {
            Ok(hello) => terminal(hub, id, socket, hello).await,
            Err(_) => refuse(socket, "send the session's token first").await,
        },
        Ok(Some(Ok(Message::Binary(first)))) => {
            serve_desktop(hub, id, socket, first.to_vec()).await
        }
        _ => refuse(socket, "send the session's token first").await,
    }
}

/// The longest close reason a WebSocket frame takes, in bytes.
const MAX_CLOSE_REASON: usize = 120;

/// A desktop's WebSocket: RDCleanPath, then RDP, in binary messages both
/// ways; it closes with why the session ended.
async fn serve_desktop(hub: crate::remote::RemoteHub, id: Uuid, socket: WebSocket, first: Vec<u8>) {
    let (mut sink, mut stream) = socket.split();
    let (from_tx, from_rx) = mpsc::channel::<Vec<u8>>(64);
    let (to_tx, mut to_rx) = mpsc::channel::<Vec<u8>>(64);
    let session = tokio::spawn(async move { desktop::run(hub, id, &first, from_rx, to_tx).await });
    // Read and write apart, so neither waits on the other.
    let reader = tokio::spawn(async move {
        while let Some(Ok(message)) = stream.next().await {
            match message {
                Message::Binary(bytes) => {
                    if from_tx.send(bytes.to_vec()).await.is_err() {
                        break;
                    }
                }
                Message::Close(_) => break,
                Message::Text(_) | Message::Ping(_) | Message::Pong(_) => {}
            }
        }
    });
    while let Some(bytes) = to_rx.recv().await {
        if sink.send(Message::Binary(bytes.into())).await.is_err() {
            break;
        }
    }
    let (code, reason) = match session.await {
        Ok(Ok(reason)) => (1000, reason),
        Ok(Err(DesktopError::BadRequest)) => (4001, "send an RDCleanPath request first".into()),
        Ok(Err(DesktopError::Refused(reason))) => (4001, reason),
        Err(_) => (1011, "the session stopped".into()),
    };
    reader.abort();
    let mut end = reason.len().min(MAX_CLOSE_REASON);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    let _ = sink
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: reason[..end].into(),
        })))
        .await;
}

/// A terminal's WebSocket, once its first message said hello.
async fn terminal(hub: crate::remote::RemoteHub, id: Uuid, mut socket: WebSocket, hello: Hello) {
    let attached = match hub.attach(id, &hello.token, hello.cols, hello.rows).await {
        Ok(attached) => attached,
        Err(error) => return refuse(socket, &error.to_string()).await,
    };
    let (from_tx, from_rx) = mpsc::channel(64);
    let (to_tx, mut to_rx) = mpsc::channel(64);
    let runner = hub.clone();
    let session = tokio::spawn(async move { runner.run(attached, from_rx, to_tx).await });
    loop {
        tokio::select! {
            message = socket.recv() => {
                let message = match message {
                    Some(Ok(Message::Binary(bytes))) => Some(FromBrowser::Input(bytes.to_vec())),
                    Some(Ok(Message::Text(text))) => match serde_json::from_str::<Control>(&text) {
                        Ok(Control::Resize { cols, rows }) => Some(FromBrowser::Resize { cols, rows }),
                        Ok(Control::Close) => Some(FromBrowser::Close),
                        Err(_) => None,
                    },
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => None,
                    Some(Ok(Message::Close(_)) | Err(_)) | None => Some(FromBrowser::Close),
                };
                if let Some(message) = message {
                    let closing = message == FromBrowser::Close;
                    if from_tx.send(message).await.is_err() || closing {
                        // The session answers with Closed, sent below.
                        drop(from_tx);
                        break;
                    }
                }
            }
            out = to_rx.recv() => match out {
                Some(ToBrowser::Output(bytes)) => {
                    if socket.send(Message::Binary(bytes.into())).await.is_err() {
                        drop(from_tx);
                        break;
                    }
                }
                Some(ToBrowser::Waiting) => { let _ = socket.send(event("waiting", None)).await; }
                Some(ToBrowser::Opened) => { let _ = socket.send(event("opened", None)).await; }
                Some(ToBrowser::Closed(reason)) => {
                    let _ = socket.send(event("closed", Some(&reason))).await;
                    break;
                }
                None => break,
            },
        }
    }
    // What the session still says, then the end.
    while let Some(out) = to_rx.recv().await {
        if let ToBrowser::Closed(reason) = out {
            let _ = socket.send(event("closed", Some(&reason))).await;
        }
    }
    let _ = session.await;
    let _ = socket.send(Message::Close(None)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(acceptance: Option<&str>, os: &str) -> Host {
        Host {
            id: Uuid::nil(),
            site_id: Uuid::nil(),
            peer_id: "web-1".into(),
            hostname: "web-1".into(),
            os_family: os.into(),
            revoked: false,
            acceptance: acceptance.map(Into::into),
            desktop_acceptance: acceptance.map(Into::into),
        }
    }

    #[test]
    fn desktops_have_their_own_refusals() {
        assert_eq!(refusal(&host(Some("accepted"), "windows"), true), None);
        let terminal_only = Host {
            desktop_acceptance: Some("off".into()),
            ..host(Some("accepted"), "windows")
        };
        assert_eq!(refusal(&terminal_only, false), None);
        assert!(refusal(&terminal_only, true)
            .unwrap()
            .contains("remote-access desktop on"));
        assert!(refusal(&host(Some("accepted"), "debian"), true)
            .unwrap()
            .contains("Windows servers"));
        assert!(refusal(&host(None, "windows"), true)
            .unwrap()
            .contains("updates by itself"));
    }

    fn refusal_t(host: &Host) -> Option<String> {
        refusal(host, false)
    }

    #[test]
    fn refusals_name_the_line_that_allows_terminals() {
        assert_eq!(refusal_t(&host(Some("accepted"), "debian")), None);
        assert!(refusal_t(&host(Some("off"), "debian"))
            .unwrap()
            .contains("MF_REMOTE_TERMINAL=1"));
        assert!(refusal_t(&host(Some("off"), "windows"))
            .unwrap()
            .contains("remote-access terminal on"));
        assert!(refusal_t(&host(None, "debian"))
            .unwrap()
            .contains("updates by itself"));
        assert!(refusal_t(&host(Some("unsupported"), "linux")).is_some());
        let removed = Host {
            revoked: true,
            ..host(Some("accepted"), "debian")
        };
        assert!(refusal_t(&removed).unwrap().contains("removed"));
    }

    #[test]
    fn websocket_messages_parse_strictly() {
        let hello: Hello = serde_json::from_str(r#"{"token":"abc","cols":80,"rows":24}"#).unwrap();
        assert_eq!((hello.cols, hello.rows), (80, 24));
        assert!(serde_json::from_str::<Hello>(r#"{"token":"abc"}"#).is_err());
        assert!(matches!(
            serde_json::from_str::<Control>(r#"{"type":"resize","cols":90,"rows":30}"#),
            Ok(Control::Resize { cols: 90, rows: 30 })
        ));
        assert!(matches!(
            serde_json::from_str::<Control>(r#"{"type":"close"}"#),
            Ok(Control::Close)
        ));
        assert!(serde_json::from_str::<Control>(r#"{"type":"input"}"#).is_err());
    }
}
