//! API errors and their JSON rendering.

use std::io;

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
    /// VictoriaLogs is unreachable, failing or not configured; the message
    /// is logged, not returned.
    #[error("logs backend unavailable")]
    LogsUnavailable(String),
    #[error("logs backend timed out")]
    LogsTimeout,
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
            Self::MetricsTimeout | Self::LogsTimeout => StatusCode::GATEWAY_TIMEOUT,
            Self::LogsUnavailable(_) => StatusCode::BAD_GATEWAY,
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
            Self::LogsUnavailable(error) => tracing::warn!(%error, "log query failed"),
            Self::LogsTimeout => tracing::warn!("log query timed out"),
            _ => {}
        }
        (self.status(), Json(json!({ "error": self.to_string() }))).into_response()
    }
}

/// Whether an error from `ServeFile::try_call` means the file cannot be
/// found. `try_call` returns these rather than a 404; these are the ones
/// tower-http's own `Service::call` answers with 404, and a handler serving
/// a file keeps to the same answer.
pub(super) fn is_missing_file(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied | io::ErrorKind::NotADirectory
    )
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use tempfile::TempDir;
    use tower_http::services::ServeFile;

    use super::*;

    #[tokio::test]
    async fn the_errors_of_a_file_that_is_not_there_count_as_missing() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("file"), "x").unwrap();
        for path in ["missing", "file/child"] {
            for method in ["GET", "HEAD"] {
                let request = Request::builder()
                    .method(method)
                    .body(Body::empty())
                    .unwrap();
                match ServeFile::new(dir.path().join(path))
                    .try_call(request)
                    .await
                {
                    Ok(response) => panic!("{method} {path}: {}", response.status()),
                    Err(error) => assert!(is_missing_file(&error), "{method} {path}: {error}"),
                }
            }
        }
    }

    #[test]
    fn other_errors_do_not() {
        assert!(is_missing_file(&io::ErrorKind::PermissionDenied.into()));
        for kind in [io::ErrorKind::IsADirectory, io::ErrorKind::Other] {
            assert!(!is_missing_file(&kind.into()), "{kind}");
        }
    }
}
