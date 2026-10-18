//! Router tests that need no database: the pool points at a closed port.

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;

use super::*;

/// A router whose database is unreachable.
pub(super) fn router_without_db() -> Router {
    let pool = PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(200))
        .connect_lazy("postgres://mf:mf@127.0.0.1:1/mf")
        .unwrap();
    router(ApiState::new(
        pool,
        Registry::default(),
        ApiSettings::default(),
    ))
}

pub(super) async fn call(app: Router, request: Request<Body>) -> (StatusCode, serde_json::Value) {
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let body = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
}

fn get(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).unwrap()
}

#[tokio::test]
async fn health_is_ok_without_database() {
    let (status, body) = call(router_without_db(), get("/api/health")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!({ "status": "ok" }));
}

#[tokio::test]
async fn readiness_reports_unreachable_database() {
    let (status, body) = call(router_without_db(), get("/api/health/ready")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body, serde_json::json!({ "status": "unavailable" }));
}

#[tokio::test]
async fn unknown_route_is_json_404() {
    let (status, body) = call(router_without_db(), get("/api/nope")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, serde_json::json!({ "error": "not found" }));
}

#[test]
fn settings_follow_config() {
    let settings = ApiSettings::default();
    assert!(settings.cookie_secure);
    assert_eq!(settings.session_lifetime, Duration::from_secs(12 * 60 * 60));
    assert_eq!(settings.session_idle_timeout, Duration::from_secs(30 * 60));
}
