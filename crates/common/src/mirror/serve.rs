//! The plain HTTP mirror listener of the server (ADR-0030) and of relays
//! (ADR-0034), with the fixed layout relays and agents rely on:
//!
//! - `/apt/<mirror>/<snapshot>/` and `/apt/<mirror>/current/`
//! - `/dnf/<mirror>/<snapshot>/` and `/dnf/<mirror>/current/`
//! - `/keys/monitorflow-archive.asc`
//! - `/index.json`, see [`Index`]
//!
//! GET and HEAD only, no directory listings, and any path segment
//! starting with a dot is 404, which hides snapshots being built. Clients
//! check what they download by its signatures, not by this transport.

use std::future::Future;
use std::path::{Path, PathBuf};

use axum::extract::{Request, State};
use axum::http::{header, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use tokio::net::TcpListener;
use tower_http::services::{ServeDir, ServeFile};

use super::{Family, Index, INDEX_PATH, PUBLIC_KEY};

/// The served directories: one per family, and the public key file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Served {
    pub apt: PathBuf,
    pub dnf: PathBuf,
    pub public_key: PathBuf,
}

impl Served {
    /// The relay's layout: `apt/`, `dnf/` and `keys/` under `root`.
    pub fn under(root: &Path) -> Self {
        Self {
            apt: root.join(Family::Apt.as_str()),
            dnf: root.join(Family::Dnf.as_str()),
            public_key: root.join("keys").join(PUBLIC_KEY),
        }
    }

    pub fn root(&self, family: Family) -> &Path {
        match family {
            Family::Apt => &self.apt,
            Family::Dnf => &self.dnf,
        }
    }
}

/// The routes over `served`.
pub fn router(served: &Served) -> Router {
    let dir = |family| ServeDir::new(served.root(family)).append_index_html_on_directories(false);
    Router::new()
        .nest_service("/apt", dir(Family::Apt))
        .nest_service("/dnf", dir(Family::Dnf))
        .route_service(
            &format!("/keys/{PUBLIC_KEY}"),
            ServeFile::new(&served.public_key),
        )
        .route(INDEX_PATH, get(index).with_state(served.clone()))
        .fallback(|| async { StatusCode::NOT_FOUND })
        .layer(middleware::from_fn(guard))
}

async fn index(State(served): State<Served>) -> Response {
    match tokio::task::spawn_blocking(move || Index::scan(&served)).await {
        Ok(Ok(index)) => (
            [(header::CONTENT_TYPE, "application/json")],
            serde_json::to_vec(&index).unwrap_or_default(),
        )
            .into_response(),
        Ok(Err(error)) => {
            tracing::warn!(%error, "cannot list the published mirrors");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn guard(request: Request, next: Next) -> Response {
    if !matches!(*request.method(), Method::GET | Method::HEAD) {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    // Decoded first: `%2e` is a dot too.
    let path = percent_encoding::percent_decode_str(request.uri().path()).decode_utf8_lossy();
    if path
        .split('/')
        .any(|s| s.starts_with('.') || s.contains('\\'))
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    next.run(request).await
}

/// Serves [`router`] on `listener` until `shutdown` resolves.
pub async fn serve(
    listener: TcpListener,
    served: Served,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(listener, router(&served))
        .with_graceful_shutdown(shutdown)
        .await
}
