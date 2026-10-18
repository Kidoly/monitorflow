//! REST API consumed by the web interface (architecture §9).
//!
//! The listener speaks plain HTTP and sits behind a TLS reverse proxy
//! (ADR-0006). Every route lives under `/api`.
//!
//! # JSON conventions
//!
//! - Bodies are JSON objects; field names are `snake_case`.
//! - Timestamps are RFC 3339 strings in UTC, IDs are UUID strings.
//! - Errors use the HTTP status and a body `{"error": "<message>"}`. Messages
//!   are generic and never carry internal details.
//!
//! # Routes
//!
//! | Method | Path | Auth | Response |
//! | --- | --- | --- | --- |
//! | GET | `/api/health` | none | `{"status": "ok"}`: the process is up |
//! | GET | `/api/health/ready` | none | `{"status": "ready"}`, or 503 `{"status": "unavailable"}` when PostgreSQL is unreachable |

mod audit;
mod error;
mod health;
pub mod password;
pub mod roles;
pub mod session;
pub mod users;

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use axum::routing::get;
use axum::Router;
use monitorflow_common::config::ServerConfig;
use sqlx::PgPool;
use tokio::net::TcpListener;

use crate::grpc::Registry;

pub use error::ApiError;
pub use roles::{Role, RoleGrant};

/// Session and cookie settings, from [`ServerConfig`].
#[derive(Debug, Clone)]
pub struct ApiSettings {
    pub cookie_secure: bool,
    pub session_lifetime: Duration,
    pub session_idle_timeout: Duration,
}

impl ApiSettings {
    pub fn from_config(config: &ServerConfig) -> Self {
        Self {
            cookie_secure: config.api_cookie_secure,
            session_lifetime: Duration::from_secs(config.api_session_lifetime),
            session_idle_timeout: Duration::from_secs(config.api_session_idle_timeout),
        }
    }
}

impl Default for ApiSettings {
    fn default() -> Self {
        Self::from_config(&ServerConfig::default())
    }
}

/// Shared by every handler; clones are cheap.
#[derive(Clone)]
pub struct ApiState {
    pub pool: PgPool,
    /// Live view of connected agents, shared with the gRPC service.
    pub registry: Registry,
    pub settings: Arc<ApiSettings>,
}

impl ApiState {
    pub fn new(pool: PgPool, registry: Registry, settings: ApiSettings) -> Self {
        Self {
            pool,
            registry,
            settings: Arc::new(settings),
        }
    }
}

/// Every API route, ready to serve.
pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/api/health", get(health::live))
        .route("/api/health/ready", get(health::ready))
        .fallback(not_found)
        .with_state(state)
}

/// Serves [`router`] on `listener` until `shutdown` resolves, then lets
/// in-flight requests finish.
pub async fn serve(
    listener: TcpListener,
    state: ApiState,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    if let Ok(addr) = listener.local_addr() {
        if !addr.ip().is_loopback() {
            tracing::warn!(%addr, "API listens on plain HTTP beyond loopback; keep it behind a TLS reverse proxy (ADR-0006)");
        }
    }
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown)
        .await
}

async fn not_found() -> ApiError {
    ApiError::NotFound
}

#[cfg(test)]
mod tests;
