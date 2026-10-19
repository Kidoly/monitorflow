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
//! | POST | `/api/auth/login` | none | body `{"username", "password"}`; 200 [`Me`] and the session cookie, or 401 |
//! | POST | `/api/auth/logout` | session | 204, clears the cookie |
//! | GET | `/api/auth/me` | session | [`Me`]: `{"id", "username", "roles": [{"role", "site_id"}]}` |
//! | GET | `/api/servers` | any role | [`ServerList`], see below |
//! | GET | `/api/servers/{id}/sensors` | read | [`sensors::Effective`]: effective sensors, origins and drift |
//! | GET | `/api/servers/{id}/sensor_configs` | read | [`sensors::EntryList`] of the server's own rows |
//! | POST | `/api/servers/{id}/sensor_configs` | modify | body [`sensors::EntryRequest`]; 201 [`sensors::Entry`] |
//! | GET | `/api/sites/{id}/sensor_configs` | read | [`sensors::EntryList`] of the site's rows |
//! | POST | `/api/sites/{id}/sensor_configs` | modify | body [`sensors::EntryRequest`]; 201 [`sensors::Entry`] |
//! | PUT | `/api/sensor_configs/{id}` | modify | body [`sensors::EntryRequest`], replaces the row; 200 [`sensors::Entry`] |
//! | DELETE | `/api/sensor_configs/{id}` | modify | 204 |
//!
//! # Authentication
//!
//! Login sets a session cookie (`__Host-mf_session`, or `mf_session` when
//! `api_cookie_secure` is off) that is `HttpOnly` and `SameSite=Strict`.
//! Sessions live server-side (see [`session`]) and end on logout, after
//! `api_session_lifetime`, or after `api_session_idle_timeout` without a
//! request. A missing or dead session gives 401 `authentication required`.
//!
//! Every request other than GET, HEAD, OPTIONS and TRACE, login included,
//! must carry the header `X-MonitorFlow-CSRF: 1` and must not be marked
//! cross-site by `Sec-Fetch-Site`; otherwise 403.
//!
//! # Authorization
//!
//! Every role (viewer, operator, approver, administrator) can read the sites
//! it is granted on; a grant with a null `site_id` covers every site. A user
//! without any role gets 403 on data routes.
//!
//! "read" routes need a role covering the site of the resource, "modify"
//! routes an operator role or above on it. A site or server outside the
//! user's sites is 404, as if it did not exist; one the user reads but may
//! not change is 403.
//!
//! # Sensor configuration
//!
//! Site and server rows of `sensor_configs` (ADR-0016). A body is
//! `{"kind", "name"?, "instance"?, "interval_ms", "timeout_ms"?, "params"?,
//! "enabled"?}`: `name` defaults to the kind, `params` is a flat object of
//! strings, numbers and booleans. Rows are checked with the agent's rules
//! and sensor registry; an invalid one is 400 with `{"error"}` saying why,
//! a second row with the same name and instance for one owner 409. Every
//! change is audited (`sensor_config.create`, `.update`, `.delete`,
//! parameter names only) and bumps the revision, so agents get it at the
//! next poll.
//!
//! # `GET /api/servers`
//!
//! ```json
//! {"servers": [{
//!   "id": "uuid", "peer_id": "agent-1", "hostname": "web-1",
//!   "site_id": "uuid", "site_name": "paris", "relay_id": null,
//!   "os_family": "debian", "os_version": "12", "agent_version": "0.1.0",
//!   "created_at": "2026-10-18T12:00:00Z", "revoked_at": null,
//!   "status": "online",
//!   "connected_at": "2026-10-18T12:00:00Z",
//!   "last_heartbeat": "2026-10-18T12:05:00Z",
//!   "via_relay": null
//! }]}
//! ```
//!
//! `status` is `online` while the agent has an open stream, else `offline`.
//! Servers come from the database only; a connected agent without a row is
//! not listed. See [`ServerView`] for each field.

pub(crate) mod audit;
pub mod auth;
mod error;
mod health;
pub mod password;
pub mod roles;
pub mod sensors;
pub mod servers;
pub mod session;
pub mod users;

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::DefaultBodyLimit;
use axum::middleware;
use axum::routing::{get, post, put};
use axum::Router;
use monitorflow_common::config::ServerConfig;
use sqlx::PgPool;
use tokio::net::TcpListener;

use crate::grpc::Registry;

pub use auth::{CurrentUser, Me};
pub use error::ApiError;
pub use roles::{Role, RoleGrant};
pub use servers::{ServerList, ServerView};

/// Largest accepted request body.
const BODY_LIMIT: usize = 64 * 1024;

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
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/auth/me", get(auth::me))
        .route("/api/servers", get(servers::list))
        .route("/api/servers/{id}/sensors", get(sensors::effective))
        .route(
            "/api/servers/{id}/sensor_configs",
            get(sensors::list_server).post(sensors::create_server),
        )
        .route(
            "/api/sites/{id}/sensor_configs",
            get(sensors::list_site).post(sensors::create_site),
        )
        .route(
            "/api/sensor_configs/{id}",
            put(sensors::update).delete(sensors::delete),
        )
        .fallback(not_found)
        .layer(middleware::from_fn(auth::csrf_guard))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
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
