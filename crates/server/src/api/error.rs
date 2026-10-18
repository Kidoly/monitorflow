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
    #[error("internal error")]
    Database(#[from] sqlx::Error),
}

impl ApiError {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if let Self::Database(error) = &self {
            tracing::error!(%error, "API request failed");
        }
        (self.status(), Json(json!({ "error": self.to_string() }))).into_response()
    }
}
