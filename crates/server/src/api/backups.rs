//! Backups in the web interface (ADR-0058): what the updater reports,
//! "Back up now", and downloads. Global administrators only: a backup
//! holds every customer's data and the server's keys.

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::Utc;
use monitorflow_common::server_update::backup::{
    parse_file_name, BackupRequest, BackupState, Backups, BACKUP_REQUEST_FILE, KEEP,
};
use monitorflow_common::server_update::write_atomic;
use serde::Serialize;
use serde_json::json;
use tower_http::services::ServeFile;
use uuid::Uuid;

use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::server_update::{alive, read_state, require_global_admin, INBOX};
use super::{ApiError, ApiState};

/// Body of `GET /api/backups`.
#[derive(Debug, Serialize)]
pub struct BackupsView {
    /// The server shares a volume with the updater, which takes backups.
    pub enabled: bool,
    /// The backups volume is mounted: files can be downloaded.
    pub downloads: bool,
    pub updater_alive: bool,
    /// "Back up now" waits for the updater to take it.
    pub requested: bool,
    /// How many backups are kept.
    pub keep: usize,
    #[serde(flatten)]
    pub backups: Backups,
}

/// Body of the 202 answer.
#[derive(Debug, Serialize)]
pub struct Started {
    pub id: String,
}

/// `GET /api/backups`.
pub async fn get(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<BackupsView>, ApiError> {
    require_global_admin(&user)?;
    let settings = &state.settings.server_update;
    let (status, requested) = read_backup_state(&state).await?;
    let updater_alive = status.as_ref().is_some_and(alive);
    Ok(Json(BackupsView {
        enabled: settings.dir.is_some(),
        downloads: settings.backup_dir.is_some(),
        updater_alive,
        requested,
        keep: KEEP,
        backups: status.map(|s| s.backups).unwrap_or_default(),
    }))
}

async fn read_backup_state(
    state: &ApiState,
) -> Result<(Option<monitorflow_common::server_update::Status>, bool), ApiError> {
    let dir = state.settings.server_update.dir.clone();
    let (status, _) = read_state(dir.clone()).await?;
    let requested = dir.is_some_and(|d| d.join(INBOX).join(BACKUP_REQUEST_FILE).exists());
    Ok((status, requested))
}

/// `POST /api/backups`: "Back up now".
pub async fn start(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<(StatusCode, Json<Started>), ApiError> {
    require_global_admin(&user)?;
    let Some(dir) = state.settings.server_update.dir.clone() else {
        return Err(ApiError::Conflict(
            "this server has no updater service, which takes backups".into(),
        ));
    };
    let (status, requested) = read_backup_state(&state).await?;
    let Some(status) = status.filter(alive) else {
        return Err(ApiError::Conflict("the updater is not running".into()));
    };
    let running = status
        .backups
        .last
        .is_some_and(|l| l.state == BackupState::Running);
    if running || requested {
        return Err(ApiError::Conflict("a backup is already in progress".into()));
    }
    let id = Uuid::new_v4().to_string();
    let file = BackupRequest {
        id: id.clone(),
        requested_by: user.username.clone(),
        requested_at: Utc::now(),
    };
    let bytes = serde_json::to_vec(&file).expect("a request serializes");
    let inbox = dir.join(INBOX);
    tokio::task::spawn_blocking(move || write_atomic(&inbox, BACKUP_REQUEST_FILE, &bytes, 0o600))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .map_err(|e| ApiError::Internal(format!("writing the backup request: {e}")))?;
    audit::record(
        &state.pool,
        Event {
            actor_kind: "user",
            actor_id: &user.id.to_string(),
            action: "server.backup",
            target: None,
            details: json!({ "request_id": id }),
        },
    )
    .await?;
    tracing::info!(%id, by = %user.username, "backup requested");
    Ok((StatusCode::ACCEPTED, Json(Started { id })))
}

/// `GET /api/backups/{name}`: the file, audited.
pub async fn download(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(name): Path<String>,
    request: Request,
) -> Result<Response, ApiError> {
    require_global_admin(&user)?;
    let Some(dir) = state.settings.server_update.backup_dir.clone() else {
        return Err(ApiError::Conflict(
            "the backups volume is not mounted in the server".into(),
        ));
    };
    // Only names the updater writes reach the path.
    if parse_file_name(&name).is_none() {
        return Err(ApiError::NotFound);
    }
    let path = dir.join(&name);
    let check = path.clone();
    let is_file = tokio::task::spawn_blocking(move || {
        std::fs::symlink_metadata(check).is_ok_and(|m| m.is_file())
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    if !is_file {
        return Err(ApiError::NotFound);
    }
    audit::record(
        &state.pool,
        Event {
            actor_kind: "user",
            actor_id: &user.id.to_string(),
            action: "server.backup.download",
            target: None,
            details: json!({ "file": name }),
        },
    )
    .await?;
    tracing::info!(file = %name, by = %user.username, "backup downloaded");
    let mut response = ServeFile::new(path)
        .try_call(request)
        .await
        .map_err(|e| ApiError::Internal(format!("serving the backup: {e}")))?
        .map(Body::new);
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/gzip"),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{name}\""))
            .expect("checked names are header-safe"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response.into_response())
}
