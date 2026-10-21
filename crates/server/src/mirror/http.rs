//! The plain HTTP listener serving mirror content (ADR-0030), with the
//! fixed layout relays and agents rely on:
//!
//! - `/apt/<mirror>/<snapshot>/` and `/apt/<mirror>/current/`
//! - `/dnf/<mirror>/<snapshot>/` and `/dnf/<mirror>/current/`
//! - `/keys/monitorflow-archive.asc`
//!
//! GET and HEAD only, no directory listings, and any path segment
//! starting with a dot is 404, which hides snapshots being built. Clients
//! check what they download by its signatures, not by this transport.

use std::future::Future;

use axum::extract::Request;
use axum::http::{Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::Router;
use tokio::net::TcpListener;
use tower_http::services::{ServeDir, ServeFile};

use super::layout::{Layout, PUBLIC_KEY};
use super::model::Family;

/// The routes over `layout`.
pub fn router(layout: &Layout) -> Router {
    let dir = |family| ServeDir::new(layout.served(family)).append_index_html_on_directories(false);
    Router::new()
        .nest_service("/apt", dir(Family::Apt))
        .nest_service("/dnf", dir(Family::Dnf))
        .route_service(
            &format!("/keys/{PUBLIC_KEY}"),
            ServeFile::new(layout.public_key()),
        )
        .fallback(|| async { StatusCode::NOT_FOUND })
        .layer(middleware::from_fn(guard))
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
    layout: Layout,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(listener, router(&layout))
        .with_graceful_shutdown(shutdown)
        .await
}
