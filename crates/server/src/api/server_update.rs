//! Updating the server from the web interface (ADR-0056): the status the
//! updater container reports, a check of the latest release, and the
//! request that starts an update. Every route needs a global administrator
//! role: the server is shared by every customer.

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use monitorflow_common::config::ServerConfig;
use monitorflow_common::release::{self, PublicKey, Version};
use monitorflow_common::server_update::{
    self as update, read_small, write_atomic, Images, Request, Running, Status, UpdateError,
    UpdateRun, HEARTBEAT, MAX_STATE_FILE, REQUEST_FILE, STATUS_FILE,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::access::Scope;
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::sensors::body;
use super::{ApiError, ApiState};

/// The subdirectory of the update volume the server writes requests in.
pub(super) const INBOX: &str = "inbox";
/// Older heartbeats mean the updater is gone.
const ALIVE: Duration = Duration::from_secs(4 * HEARTBEAT.as_secs());

/// Where updates come from and how they are asked for, from
/// [`ServerConfig`].
#[derive(Debug, Clone)]
pub struct ServerUpdateSettings {
    /// The volume shared with the updater; `None` turns updates off.
    pub dir: Option<PathBuf>,
    /// The backups volume, read-only, for downloads (ADR-0058).
    pub backup_dir: Option<PathBuf>,
    /// `release_url`, else `agent_download_url`.
    pub source: String,
    /// The keys built in; tests use their own.
    pub keys: Vec<PublicKey>,
    pub client: reqwest::Client,
}

impl ServerUpdateSettings {
    pub fn from_config(config: &ServerConfig) -> Self {
        Self {
            dir: config.server_update_dir.clone(),
            backup_dir: config.backup_dir.clone(),
            source: update::source(config.release_url.as_deref(), &config.agent_download_url)
                .to_owned(),
            keys: release::trusted_keys()
                .map(<[PublicKey]>::to_vec)
                .unwrap_or_default(),
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(30))
                .build()
                .expect("a reqwest client with rustls builds"),
        }
    }
}

/// Body of `GET /api/server-update`.
#[derive(Debug, Serialize)]
pub struct UpdateView {
    /// The version of this server process.
    pub running_version: &'static str,
    /// Whether `server_update_dir` is set.
    pub enabled: bool,
    pub source: String,
    /// What the updater last reported; `None` when it never did.
    pub updater: Option<UpdaterView>,
    /// The last update, running or finished.
    pub update: Option<UpdateRun>,
    /// A request waits for the updater to take it.
    pub requested: bool,
}

#[derive(Debug, Serialize)]
pub struct UpdaterView {
    pub version: String,
    pub heartbeat_at: DateTime<Utc>,
    /// The heartbeat is recent.
    pub alive: bool,
    pub running: Option<Running>,
}

/// Body of `POST /api/server-update/check`.
#[derive(Debug, Serialize)]
pub struct CheckResult {
    pub source: String,
    pub running_version: &'static str,
    /// Whether the source answered; `false` on an offline server.
    pub reachable: bool,
    /// Why there is no `latest`.
    pub error: Option<String>,
    pub latest: Option<Latest>,
}

/// A verified release and what installing it changes.
#[derive(Debug, Serialize)]
pub struct Latest {
    pub version: String,
    /// Newer than the running server: it can be installed.
    pub newer: bool,
    pub release_notes_url: Option<String>,
    pub images: Images,
    /// The images differ from the running ones, as the updater reported
    /// them; a local build always differs.
    pub web_changes: bool,
    pub updater_changes: bool,
    /// The release's agents are already cached for agents.
    pub agent_release_cached: bool,
}

/// Body of `POST /api/server-update`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartRequest {
    pub version: String,
}

/// Body of the 202 answer.
#[derive(Debug, Serialize)]
pub struct Started {
    pub id: String,
}

pub(super) fn require_global_admin(user: &CurrentUser) -> Result<(), ApiError> {
    if user.administered_customers() != Scope::All {
        return Err(ApiError::Forbidden);
    }
    Ok(())
}

fn running_version() -> Version {
    monitorflow_common::VERSION
        .parse()
        .expect("the build version is a version")
}

/// `status.json` and whether a request waits, read off the runtime.
pub(super) async fn read_state(dir: Option<PathBuf>) -> Result<(Option<Status>, bool), ApiError> {
    let Some(dir) = dir else {
        return Ok((None, false));
    };
    tokio::task::spawn_blocking(move || {
        let status = read_small(&dir.join(STATUS_FILE), MAX_STATE_FILE)
            .map_err(|e| ApiError::Internal(format!("reading the update status: {e}")))?
            .and_then(|bytes| serde_json::from_slice::<Status>(&bytes).ok());
        let requested = dir.join(INBOX).join(REQUEST_FILE).exists();
        Ok((status, requested))
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
}

pub(super) fn alive(status: &Status) -> bool {
    Utc::now()
        .signed_duration_since(status.heartbeat_at)
        .to_std()
        .is_ok_and(|age| age < ALIVE)
        || status.heartbeat_at > Utc::now()
}

/// `GET /api/server-update`.
pub async fn get(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<UpdateView>, ApiError> {
    require_global_admin(&user)?;
    let settings = &state.settings.server_update;
    let (status, requested) = read_state(settings.dir.clone()).await?;
    let (updater, update) = match status {
        None => (None, None),
        Some(status) => (
            Some(UpdaterView {
                alive: alive(&status),
                version: status.updater_version,
                heartbeat_at: status.heartbeat_at,
                running: status.running,
            }),
            status.update,
        ),
    };
    Ok(Json(UpdateView {
        running_version: monitorflow_common::VERSION,
        enabled: settings.dir.is_some(),
        source: settings.source.clone(),
        updater,
        update,
        requested,
    }))
}

/// `POST /api/server-update/check`: fetches and verifies the latest
/// release, as the updater will.
pub async fn check(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<CheckResult>, ApiError> {
    require_global_admin(&user)?;
    let settings = &state.settings.server_update;
    let base = update::release_base(&settings.source, None);
    let mut result = CheckResult {
        source: settings.source.clone(),
        running_version: monitorflow_common::VERSION,
        reachable: true,
        error: None,
        latest: None,
    };
    let candidate = match update::fetch(&settings.client, &base, &settings.keys, None).await {
        Ok(candidate) => candidate,
        Err(e) => {
            result.reachable = !matches!(e, UpdateError::Download { .. });
            result.error = Some(e.to_string());
            return Ok(Json(result));
        }
    };
    let (status, _) = read_state(settings.dir.clone()).await?;
    let running = status.and_then(|s| s.running);
    let differs = |pinned: Option<&String>, image: &update::ImageRef| {
        pinned.is_none_or(|pinned| pinned != image.as_str())
    };
    let version = candidate.release.version;
    let cache = state.settings.agent_releases.cache.clone();
    let cached_version = version.clone();
    let agent_release_cached = tokio::task::spawn_blocking(move || cache.has(&cached_version))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    result.latest = Some(Latest {
        newer: version > running_version(),
        release_notes_url: update::release_notes_url(&settings.source, &version),
        web_changes: differs(
            running.as_ref().and_then(|r| r.web_image.as_ref()),
            &candidate.images.web,
        ),
        updater_changes: differs(
            running.as_ref().and_then(|r| r.updater_image.as_ref()),
            &candidate.images.updater,
        ),
        agent_release_cached,
        images: candidate.images,
        version: version.to_string(),
    });
    Ok(Json(result))
}

/// `POST /api/server-update`: asks the updater to install `version`.
pub async fn start(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<StartRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Started>), ApiError> {
    require_global_admin(&user)?;
    let request = body(request)?;
    let target: Version = request
        .version
        .parse()
        .map_err(|_| ApiError::Invalid("version must be a release version like 0.2.0".into()))?;
    let current = running_version();
    if target <= current {
        return Err(ApiError::Invalid(format!(
            "{target} is not newer than the running {current}"
        )));
    }
    let Some(dir) = state.settings.server_update.dir.clone() else {
        return Err(ApiError::Conflict(
            "this server is not set up for updates from the web interface".into(),
        ));
    };
    let (status, requested) = read_state(Some(dir.clone())).await?;
    let Some(status) = status.filter(alive) else {
        return Err(ApiError::Conflict("the updater is not running".into()));
    };
    let busy = status.update.is_some_and(|u| !u.state.is_final());
    if busy || requested {
        return Err(ApiError::Conflict(
            "an update is already in progress".into(),
        ));
    }

    let id = Uuid::new_v4().to_string();
    let file = Request {
        id: id.clone(),
        version: target.to_string(),
        requested_by: user.username.clone(),
        requested_at: Utc::now(),
    };
    let bytes = serde_json::to_vec(&file).expect("a request serializes");
    let inbox = dir.join(INBOX);
    tokio::task::spawn_blocking(move || write_request(&inbox, &bytes))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .map_err(|e| ApiError::Internal(format!("writing the update request: {e}")))?;
    audit::record(
        &state.pool,
        Event {
            actor_kind: "user",
            actor_id: &user.id.to_string(),
            action: "server.update",
            target: None,
            details: json!({ "request_id": id, "from": current.to_string(), "to": target.to_string() }),
        },
    )
    .await?;
    tracing::info!(%id, from = %current, to = %target, by = %user.username, "server update requested");
    Ok((StatusCode::ACCEPTED, Json(Started { id })))
}

fn write_request(inbox: &Path, bytes: &[u8]) -> std::io::Result<()> {
    write_atomic(inbox, REQUEST_FILE, bytes, 0o600)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(age: i64) -> Status {
        Status {
            updater_version: "0.2.0".into(),
            heartbeat_at: Utc::now() - chrono::Duration::seconds(age),
            running: None,
            update: None,
            backups: Default::default(),
        }
    }

    #[test]
    fn the_updater_is_alive_while_its_heartbeat_is_recent() {
        assert!(alive(&status(10)));
        assert!(alive(&status(-5)), "a clock slightly ahead");
        assert!(!alive(&status(ALIVE.as_secs() as i64 + 1)));
    }
}
