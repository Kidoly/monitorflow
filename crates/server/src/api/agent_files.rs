//! Agent release files over the web interface's HTTPS (ADR-0054):
//! `GET /agent/<version>/<file>` and `/agent/latest/<file>`, from the
//! server's release cache, without authentication. The reverse proxy
//! forwards `/agent/*` here as it does `/api/*`, so hosts install from the
//! address operators already use, in the same layout as the mirror
//! listener's `/agent/<version>/` (ADR-0053).
//!
//! Only the file names of a release are served, from versions whose
//! `VERSION` matches their directory. Everything here is public and
//! signed; clients check it by the signature, not by this transport.

use std::path::PathBuf;

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use monitorflow_common::release::Version;
use tower_http::services::ServeFile;

use super::ApiState;
use crate::agent_releases::{self, ReleaseCache};

/// The version segment that names the newest release.
pub const LATEST: &str = "latest";

/// The answer when the cache is empty, for `curl` users and the guide.
pub const NONE_CACHED: &str = "No agent release is cached on this server yet. An administrator \
     imports one with `monitorflow-server agent-release import --dir DIR` (docs/deploy-agent.md).\n";

#[derive(Debug, PartialEq, Eq)]
enum Found {
    Dir(PathBuf),
    /// `latest` with nothing cached.
    Empty,
    Unknown,
}

fn resolve(cache: &ReleaseCache, version: &str) -> Found {
    let versions = cache.versions().unwrap_or_default();
    let version = if version == LATEST {
        match agent_releases::latest(&versions) {
            Some(latest) => latest.clone(),
            None => return Found::Empty,
        }
    } else {
        match version.parse::<Version>() {
            Ok(version) if versions.contains(&version) => version,
            _ => return Found::Unknown,
        }
    };
    Found::Dir(cache.dir().join(version.to_string()))
}

fn not_found(text: &'static str) -> Response {
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        text,
    )
        .into_response()
}

/// `GET /agent/{version}/{file}`.
pub async fn file(
    State(state): State<ApiState>,
    Path((version, name)): Path<(String, String)>,
    request: Request,
) -> Response {
    let Some(name) = agent_releases::kept_files()
        .into_iter()
        .find(|n| *n == name)
    else {
        return not_found("Not a release file.\n");
    };
    let cache = state.settings.agent_releases.cache.clone();
    let dir = match tokio::task::spawn_blocking(move || resolve(&cache, &version)).await {
        Ok(Found::Dir(dir)) => dir,
        Ok(Found::Empty) => return not_found(NONE_CACHED),
        Ok(Found::Unknown) => return not_found("This server holds no such agent release.\n"),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    match ServeFile::new(dir.join(name)).try_call(request).await {
        Ok(response) => {
            let mut response = response.map(Body::new);
            // `latest` moves, and an import may replace a version.
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            response
        }
        Err(error) => {
            tracing::warn!(%error, "cannot serve an agent release file");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn latest_and_versions_resolve_to_listed_directories() {
        let mirror = TempDir::new().unwrap();
        let cache = ReleaseCache::in_mirror_dir(mirror.path());
        assert_eq!(resolve(&cache, LATEST), Found::Empty);
        assert_eq!(resolve(&cache, "0.2.0"), Found::Unknown);
        for version in ["0.2.0", "0.3.0", "0.4.0-rc.1"] {
            let dir = cache.dir().join(version);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("VERSION"), format!("{version}\n")).unwrap();
        }
        // A directory that is not a version, or whose VERSION disagrees.
        fs::create_dir_all(cache.dir().join("0.9.0")).unwrap();
        assert_eq!(
            resolve(&cache, LATEST),
            Found::Dir(cache.dir().join("0.3.0"))
        );
        assert_eq!(
            resolve(&cache, "0.4.0-rc.1"),
            Found::Dir(cache.dir().join("0.4.0-rc.1"))
        );
        for bad in ["0.9.0", "..", "v0.2.0", ""] {
            assert_eq!(resolve(&cache, bad), Found::Unknown, "{bad}");
        }
    }
}
