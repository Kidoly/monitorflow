//! Liveness and readiness probes. Neither needs authentication nor reveals
//! anything beyond up or down.

use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Serialize;

use super::ApiState;

/// Readiness fails if PostgreSQL does not answer within this delay.
const READY_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Status {
    pub status: &'static str,
}

/// `GET /api/health`: the process is up and serving requests.
pub async fn live() -> Json<Status> {
    Json(Status { status: "ok" })
}

/// `GET /api/health/ready`: the database answers a trivial query.
pub async fn ready(State(state): State<ApiState>) -> (StatusCode, Json<Status>) {
    let check = sqlx::query("SELECT 1").execute(&state.pool);
    match tokio::time::timeout(READY_TIMEOUT, check).await {
        Ok(Ok(_)) => (StatusCode::OK, Json(Status { status: "ready" })),
        Ok(Err(error)) => {
            tracing::warn!(%error, "readiness check failed");
            unavailable()
        }
        Err(_) => {
            tracing::warn!("readiness check timed out");
            unavailable()
        }
    }
}

fn unavailable() -> (StatusCode, Json<Status>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(Status {
            status: "unavailable",
        }),
    )
}
