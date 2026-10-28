//! API errors and their JSON rendering.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// What a handler can fail with. The `Display` text is what clients see, so
/// it stays generic; details go to the server log only.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("not found")]
    NotFound,
    /// The body is not the expected JSON.
    #[error("invalid request body")]
    BadRequest,
    /// The request is well-formed but its content is invalid; the message
    /// says why and is safe to show.
    #[error("{0}")]
    Invalid(String),
    /// The request conflicts with existing data; the message says how.
    #[error("{0}")]
    Conflict(String),
    /// A one-time link that was used, replaced or expired; the message
    /// says how to get a new one.
    #[error("{0}")]
    Gone(String),
    /// No valid session.
    #[error("authentication required")]
    Unauthorized,
    /// Wrong username or password, or a disabled account: one message for
    /// all, so it does not reveal which accounts exist.
    #[error("invalid credentials")]
    InvalidCredentials,
    /// Authenticated, but no role allows the request.
    #[error("forbidden")]
    Forbidden,
    /// A state-changing request without the CSRF header, or cross-site.
    #[error("cross-site request rejected")]
    CrossSite,
    /// VictoriaMetrics is unreachable, failing or not configured; the
    /// message is logged, not returned.
    #[error("metrics backend unavailable")]
    MetricsUnavailable(String),
    #[error("metrics backend timed out")]
    MetricsTimeout,
    #[error("internal error")]
    Database(#[from] sqlx::Error),
    /// Any other server-side failure; the message is logged, not returned.
    #[error("internal error")]
    Internal(String),
}

impl ApiError {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::BadRequest | Self::Invalid(_) => StatusCode::BAD_REQUEST,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Gone(_) => StatusCode::GONE,
            Self::Unauthorized | Self::InvalidCredentials => StatusCode::UNAUTHORIZED,
            Self::Forbidden | Self::CrossSite => StatusCode::FORBIDDEN,
            Self::MetricsUnavailable(_) => StatusCode::BAD_GATEWAY,
            Self::MetricsTimeout => StatusCode::GATEWAY_TIMEOUT,
            Self::Database(_) | Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match &self {
            Self::Database(error) => tracing::error!(%error, "API request failed"),
            Self::Internal(error) => tracing::error!(%error, "API request failed"),
            Self::MetricsUnavailable(error) => tracing::warn!(%error, "metric query failed"),
            Self::MetricsTimeout => tracing::warn!("metric query timed out"),
            _ => {}
        }
        (self.status(), Json(json!({ "error": self.to_string() }))).into_response()
    }
}
