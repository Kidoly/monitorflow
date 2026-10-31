//! Importing a release from the web interface (ADR-0061), for servers
//! without Internet: `agent-release import` from files uploaded by the
//! browser or downloaded from a URL of the site. Either way the files land
//! in one staging directory, hidden from the listeners by its dot name, and
//! [`ReleaseCache::import`] checks them as it does on the command line.
//! Global administrators only: the release cache is shared by every
//! customer.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::multipart::{Multipart, MultipartError, MultipartRejection};
use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use monitorflow_common::release::{self, ReleaseError};
use monitorflow_common::server_update::error_chain;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, OwnedMutexGuard};

use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::sensors::body;
use super::server_update::require_global_admin;
use super::{ApiError, ApiState};
use crate::agent_releases::{self, CacheError, Imported};

/// Largest release accepted, comfortably above every file together.
pub const MAX_RELEASE: usize = 2 * 1024 * 1024 * 1024;
/// Under the release cache, so the import is a copy on the same disk.
const STAGING: &str = ".upload";
/// Binaries take a while on a slow site link: only a stall ends a download.
const STALLED: Duration = Duration::from_secs(60);

/// One import at a time: they share the staging directory.
pub type ImportLock = Arc<Mutex<()>>;

/// Body of `POST /api/agent-releases/import-url`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UrlRequest {
    /// The directory holding the release files.
    pub url: String,
}

/// Body of a successful import.
#[derive(Debug, Serialize)]
pub struct ImportResult {
    pub version: String,
    /// Files of a whole release that were not given.
    pub missing: Vec<&'static str>,
}

/// A staging directory emptied for this import, and the right to use it.
struct Staging {
    dir: PathBuf,
    _lock: OwnedMutexGuard<()>,
}

impl Staging {
    async fn begin(state: &ApiState) -> Result<Self, ApiError> {
        let lock = state.release_import.clone().try_lock_owned().map_err(|_| {
            ApiError::Conflict("Another import is running; try again when it ends.".into())
        })?;
        let dir = state.settings.agent_releases.cache.dir().join(STAGING);
        // An import cut short by a restart leaves its files here.
        let _ = fs::remove_dir_all(&dir).await;
        fs::create_dir_all(&dir).await.map_err(staging_error)?;
        Ok(Self { dir, _lock: lock })
    }

    /// The file to write `name` in; `None` for a file a release does not
    /// keep, like the images list or the server installer.
    async fn create(&self, name: &str) -> Result<Option<fs::File>, ApiError> {
        let Some(name) = agent_releases::kept_files()
            .into_iter()
            .find(|n| *n == name)
        else {
            return Ok(None);
        };
        let file = fs::File::create(self.dir.join(name))
            .await
            .map_err(staging_error)?;
        Ok(Some(file))
    }
}

fn staging_error(error: std::io::Error) -> ApiError {
    ApiError::Internal(format!("staging a release: {error}"))
}

/// Sentences for the page: the import failed on what the administrator
/// gave, not on the server.
fn refusal(error: CacheError) -> ApiError {
    let sentence = match error {
        CacheError::Release(ReleaseError::BadSignature) => {
            "These files are not a genuine MonitorFlow release: the signature does not verify."
                .to_owned()
        }
        CacheError::Release(ReleaseError::NoKeys) => {
            "This build of the server trusts no release key, so it cannot import releases."
                .to_owned()
        }
        CacheError::Release(ReleaseError::Mismatch(name)) => {
            format!("{name} is damaged or from another release: its checksum does not match.")
        }
        CacheError::Release(error) => format!("These files are not a usable release: {error}."),
        CacheError::NoBinary => {
            "The release needs at least one agent binary; choose all of its files.".to_owned()
        }
        CacheError::Io(path, error) => {
            return ApiError::Internal(format!("importing {path}: {error}"))
        }
    };
    ApiError::Invalid(sentence)
}

/// Imports what `staging` holds, if `filled` went well, then empties it.
async fn finish(
    state: &ApiState,
    user: &CurrentUser,
    staging: Staging,
    filled: Result<(), ApiError>,
    from: &str,
) -> Result<Json<ImportResult>, ApiError> {
    let imported = match filled {
        Ok(()) => import(state, staging.dir.clone()).await,
        Err(error) => Err(error),
    };
    let _ = fs::remove_dir_all(&staging.dir).await;
    let Imported { version, missing } = imported?;
    let version = version.to_string();
    audit::record(
        &state.pool,
        Event {
            actor_kind: "user",
            actor_id: &user.id.to_string(),
            action: "agent_release.import",
            target: Some(("agent_release", &version)),
            details: json!({ "from": from, "missing": missing }),
        },
    )
    .await?;
    tracing::info!(%version, from, by = %user.username, "release imported");
    Ok(Json(ImportResult { version, missing }))
}

async fn import(state: &ApiState, dir: PathBuf) -> Result<Imported, ApiError> {
    let cache = state.settings.agent_releases.cache.clone();
    let keys = state.settings.agent_releases.keys.clone();
    tokio::task::spawn_blocking(move || {
        let absent = absent_signed_files(&dir);
        if !absent.is_empty() {
            return Err(ApiError::Invalid(format!(
                "A release needs {}: choose all of its files.",
                absent.join(", ")
            )));
        }
        cache.import(&dir, &keys).map_err(refusal)
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
}

fn absent_signed_files(dir: &Path) -> Vec<&'static str> {
    [release::SUMS, release::SIGNATURE, release::VERSION_FILE]
        .into_iter()
        .filter(|name| !dir.join(name).is_file())
        .collect()
}

/// `POST /api/agent-releases/import`: the release files as a multipart
/// form, one part per file, named by their file names.
pub async fn upload(
    State(state): State<ApiState>,
    user: CurrentUser,
    multipart: Result<Multipart, MultipartRejection>,
) -> Result<Json<ImportResult>, ApiError> {
    require_global_admin(&user)?;
    let multipart = multipart.map_err(|_| ApiError::BadRequest)?;
    let staging = Staging::begin(&state).await?;
    let filled = receive(&staging, multipart).await;
    finish(&state, &user, staging, filled, "upload").await
}

async fn receive(staging: &Staging, mut multipart: Multipart) -> Result<(), ApiError> {
    let cut_short =
        |e: MultipartError| ApiError::Invalid(format!("The upload stopped: {}.", e.body_text()));
    while let Some(mut field) = multipart.next_field().await.map_err(cut_short)? {
        let name = field.file_name().unwrap_or_default().to_owned();
        let Some(mut file) = staging.create(&name).await? else {
            continue;
        };
        while let Some(chunk) = field.chunk().await.map_err(cut_short)? {
            file.write_all(&chunk).await.map_err(staging_error)?;
        }
        file.flush().await.map_err(staging_error)?;
    }
    Ok(())
}

/// The directory `url` names, with a trailing slash, or why it cannot be
/// used. A sentence for the form.
fn release_dir(url: &str) -> Result<String, String> {
    let url = url.trim();
    let parsed = reqwest::Url::parse(url)
        .map_err(|_| format!("`{url}` is not a URL; it starts with https://."))?;
    if !matches!(parsed.scheme(), "https" | "http") || parsed.host_str().is_none() {
        return Err(format!("`{url}` is not an http or https URL."));
    }
    Ok(format!("{}/", parsed.as_str().trim_end_matches('/')))
}

/// `POST /api/agent-releases/import-url`: downloads the files of a release
/// from a directory, on plain HTTP too: the signature protects them, not
/// the transport (ADR-0056).
pub async fn from_url(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<UrlRequest>, JsonRejection>,
) -> Result<Json<ImportResult>, ApiError> {
    require_global_admin(&user)?;
    let request = body(request)?;
    let base = release_dir(&request.url).map_err(ApiError::Invalid)?;
    let staging = Staging::begin(&state).await?;
    let filled = download(&staging, &base).await;
    // Only the host: a URL may embed a token.
    let host = reqwest::Url::parse(&base)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default();
    finish(&state, &user, staging, filled, &host).await
}

async fn download(staging: &Staging, base: &str) -> Result<(), ApiError> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(STALLED)
        .build()
        .expect("a reqwest client with rustls builds");
    let mut total = 0usize;
    for name in agent_releases::kept_files() {
        let url = format!("{base}{name}");
        let failed =
            |reason: String| ApiError::Invalid(format!("Could not download {url}: {reason}."));
        let mut response = client
            .get(&url)
            .send()
            .await
            .map_err(|e| failed(error_chain(&e)))?;
        // Only the signed files are required; `import` says what is absent.
        if response.status() == StatusCode::NOT_FOUND {
            continue;
        }
        if !response.status().is_success() {
            return Err(failed(format!("HTTP {}", response.status())));
        }
        let Some(mut file) = staging.create(name).await? else {
            continue;
        };
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| failed(error_chain(&e)))?
        {
            total += chunk.len();
            if total > MAX_RELEASE {
                return Err(failed("the release is larger than expected".into()));
            }
            file.write_all(&chunk).await.map_err(staging_error)?;
        }
        file.flush().await.map_err(staging_error)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_directories_end_in_a_slash() {
        assert_eq!(
            release_dir(" https://files.lan/mf/0.3.0 ").unwrap(),
            "https://files.lan/mf/0.3.0/"
        );
        assert_eq!(
            release_dir("http://10.0.0.5:8000/").unwrap(),
            "http://10.0.0.5:8000/"
        );
        assert!(release_dir("files.lan/mf")
            .unwrap_err()
            .contains("not a URL"));
        assert!(release_dir("ftp://files.lan/").is_err());
    }

    #[test]
    fn refusals_are_sentences_about_the_files() {
        let text = |error| match refusal(error) {
            ApiError::Invalid(text) => text,
            other => panic!("{other:?}"),
        };
        assert!(text(CacheError::Release(ReleaseError::BadSignature)).contains("not a genuine"));
        assert!(text(CacheError::Release(ReleaseError::Mismatch("x".into()))).starts_with("x is"));
        assert!(text(CacheError::NoBinary).contains("binary"));
        let disk = std::io::Error::other("full");
        assert!(matches!(
            refusal(CacheError::Io("p".into(), disk)),
            ApiError::Internal(_)
        ));
    }
}
