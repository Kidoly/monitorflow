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

fn post(uri: &str) -> axum::http::request::Builder {
    Request::post(uri).header("content-type", "application/json")
}

const LOGIN: &str = r#"{"username":"admin","password":"correct horse battery"}"#;

#[tokio::test]
async fn me_without_session_is_unauthorized() {
    let (status, body) = call(router_without_db(), get("/api/auth/me")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        body,
        serde_json::json!({ "error": "authentication required" })
    );

    // A malformed cookie is rejected before any database lookup.
    let request = Request::get("/api/auth/me")
        .header("cookie", "__Host-mf_session=not-a-token")
        .body(Body::empty())
        .unwrap();
    let (status, _) = call(router_without_db(), request).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn state_changing_requests_need_the_csrf_header() {
    for uri in ["/api/auth/login", "/api/auth/logout"] {
        let request = post(uri).body(Body::from(LOGIN)).unwrap();
        let (status, body) = call(router_without_db(), request).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
        assert_eq!(
            body,
            serde_json::json!({ "error": "cross-site request rejected" })
        );
    }
    let request = post("/api/auth/login")
        .header(auth::CSRF_HEADER, "1")
        .header("sec-fetch-site", "cross-site")
        .body(Body::from(LOGIN))
        .unwrap();
    let (status, _) = call(router_without_db(), request).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn login_rejects_malformed_bodies() {
    let request = post("/api/auth/login")
        .header(auth::CSRF_HEADER, "1")
        .body(Body::from(r#"{"username":"admin"}"#))
        .unwrap();
    let (status, body) = call(router_without_db(), request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, serde_json::json!({ "error": "invalid request body" }));
}
