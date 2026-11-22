//! An in-process stand-in for the HaloPSA API (ADR-0164): the token
//! endpoint and the API routes MonitorFlow calls, with sanitized answers
//! shaped like a real instance's. Never a real instance.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use serde_json::json;
use tokio::net::TcpListener;

pub const CLIENT_ID: &str = "test-client-id";
pub const CLIENT_SECRET: &str = "test-client-secret-0123456789";
pub const TOKEN: &str = "test-access-token";
pub const AGENT: &str = "MonitorFlow";

/// What the fake received and how it answers.
#[derive(Default)]
pub struct Inner {
    /// Token requests, as their form fields.
    pub token_requests: Mutex<Vec<HashMap<String, String>>>,
    /// Every API request: method, path and query.
    pub requests: Mutex<Vec<String>>,
}

#[derive(Clone)]
pub struct FakeHalo {
    pub url: String,
    pub inner: Arc<Inner>,
}

impl FakeHalo {
    pub async fn start() -> Self {
        let inner = Arc::new(Inner::default());
        let app = Router::new()
            .route("/auth/token", post(token))
            .route("/api/Agent/me", get(me))
            .with_state(inner.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { url, inner }
    }

    pub fn token_requests(&self) -> Vec<HashMap<String, String>> {
        self.inner.token_requests.lock().unwrap().clone()
    }

    pub fn requests(&self) -> Vec<String> {
        self.inner.requests.lock().unwrap().clone()
    }
}

async fn token(
    State(inner): State<Arc<Inner>>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    inner.token_requests.lock().unwrap().push(form.clone());
    let good = form.get("grant_type").map(String::as_str) == Some("client_credentials")
        && form.get("client_id").map(String::as_str) == Some(CLIENT_ID)
        && form.get("client_secret").map(String::as_str) == Some(CLIENT_SECRET);
    if !good {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "invalid_client"})),
        )
            .into_response();
    }
    Json(json!({
        "access_token": TOKEN,
        "token_type": "Bearer",
        "expires_in": 3600,
        "refresh_token": "ignored",
        "id_token": "ignored",
    }))
    .into_response()
}

/// Whether the request carries the token; records it.
fn authorized(inner: &Inner, headers: &HeaderMap, line: String) -> bool {
    inner.requests.lock().unwrap().push(line);
    headers.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {TOKEN}"))
}

fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, "").into_response()
}

async fn me(State(inner): State<Arc<Inner>>, headers: HeaderMap) -> Response {
    if !authorized(&inner, &headers, "GET /api/Agent/me".into()) {
        return unauthorized();
    }
    Json(json!({"id": 42, "name": AGENT, "email": ""})).into_response()
}
