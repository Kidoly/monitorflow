//! Agent release files behind the web interface (ADR-0054): served from
//! the cache without a session, `latest` resolved, nothing else served.
//! Needs no database.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use monitorflow_server::agent_releases::ReleaseCache;
use monitorflow_server::api::{self, agent_files, ApiSettings, ApiState};
use monitorflow_server::grpc::Registry;
use sqlx::postgres::PgPoolOptions;
use tempfile::TempDir;
use tower::ServiceExt;

fn release(cache: &ReleaseCache, version: &str) {
    let dir = cache.dir().join(version);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("VERSION"), format!("{version}\n")).unwrap();
    std::fs::write(dir.join("install.sh"), format!("#!/bin/sh\n# {version}\n")).unwrap();
}

async fn get(mirror: &TempDir, method: &str, uri: &str) -> (StatusCode, Option<String>, String) {
    let mut settings = ApiSettings::default();
    settings.agent_releases.cache = ReleaseCache::in_mirror_dir(mirror.path());
    // Never connects: these routes do not touch the database.
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://unused@127.0.0.1:1/none")
        .unwrap();
    let app = api::router(ApiState::new(pool, Registry::default(), settings));
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let cache_control = response
        .headers()
        .get(header::CACHE_CONTROL)
        .map(|v| v.to_str().unwrap().to_owned());
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        cache_control,
        String::from_utf8_lossy(&body).into_owned(),
    )
}

#[tokio::test]
async fn an_empty_cache_says_how_to_fill_it() {
    let mirror = TempDir::new().unwrap();
    let (status, _, body) = get(&mirror, "GET", "/agent/latest/install.sh").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, agent_files::NONE_CACHED);
}

#[tokio::test]
async fn serves_cached_release_files_without_a_session() {
    let mirror = TempDir::new().unwrap();
    let cache = ReleaseCache::in_mirror_dir(mirror.path());
    release(&cache, "0.2.0");
    release(&cache, "0.3.0");
    release(&cache, "0.4.0-rc.1");

    let (status, cache_control, body) = get(&mirror, "GET", "/agent/latest/install.sh").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "#!/bin/sh\n# 0.3.0\n", "latest skips pre-releases");
    assert_eq!(cache_control.as_deref(), Some("no-cache"));
    let (status, _, body) = get(&mirror, "GET", "/agent/0.2.0/install.sh").await;
    assert_eq!(
        (status, body.as_str()),
        (StatusCode::OK, "#!/bin/sh\n# 0.2.0\n")
    );
    let (status, _, body) = get(&mirror, "HEAD", "/agent/0.4.0-rc.1/VERSION").await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, ""));

    for uri in [
        // Not in the release, or not cached.
        "/agent/0.3.0/monitorflow-agent-windows-x86_64.msi",
        "/agent/0.9.0/install.sh",
        // Not a release file name, or not a version.
        "/agent/0.3.0/notes.txt",
        "/agent/0.3.0/.install.sh",
        "/agent/..%2f0.3.0/install.sh",
        "/agent/0.3.0/..%2fVERSION",
        "/agent/0.3.0",
    ] {
        assert_eq!(
            get(&mirror, "GET", uri).await.0,
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
    // Read-only: the CSRF guard or the router refuses anything else.
    for method in ["POST", "PUT", "DELETE"] {
        let (status, _, _) = get(&mirror, method, "/agent/latest/install.sh").await;
        assert!(status.is_client_error(), "{method}: {status}");
    }
}
