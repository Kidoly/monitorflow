//! An in-process stand-in for the HaloPSA API (ADR-0164): the token
//! endpoint and the API routes MonitorFlow calls, with sanitized answers
//! shaped like a real instance's. Never a real instance.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, State};
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
            .route("/api/Client", get(clients))
            .route("/api/Client/{id}", get(client))
            .route("/api/Site", get(sites))
            .route("/api/Site/{id}", get(site))
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

fn query_line(path: &str, query: &HashMap<String, String>) -> String {
    let mut pairs: Vec<String> = query.iter().map(|(k, v)| format!("{k}={v}")).collect();
    pairs.sort();
    format!("GET {path}?{}", pairs.join("&"))
}

/// Clients of the fake instance.
pub const CLIENTS: [(i64, &str); 3] = [
    (10, "Example Industries"),
    (11, "Example Logistics"),
    (12, "Sample Corp"),
];
/// Sites of the fake instance: id, client, name.
pub const SITES: [(i64, i64, &str); 3] = [
    (100, 10, "Main office"),
    (101, 10, "Warehouse"),
    (110, 11, "Head office"),
];

async fn clients(
    State(inner): State<Arc<Inner>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if !authorized(&inner, &headers, query_line("/api/Client", &query)) {
        return unauthorized();
    }
    let search = query
        .get("search")
        .cloned()
        .unwrap_or_default()
        .to_lowercase();
    let found: Vec<_> = CLIENTS
        .iter()
        .filter(|(_, name)| name.to_lowercase().contains(&search))
        .map(|(id, name)| json!({"id": id, "name": name, "toplevel_id": 1, "inactive": false}))
        .collect();
    Json(json!({"record_count": found.len(), "clients": found})).into_response()
}

async fn client(
    State(inner): State<Arc<Inner>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    if !authorized(&inner, &headers, format!("GET /api/Client/{id}")) {
        return unauthorized();
    }
    match CLIENTS.iter().find(|(c, _)| *c == id) {
        Some((id, name)) => Json(json!({"id": id, "name": name})).into_response(),
        None => (StatusCode::NOT_FOUND, "Client not found").into_response(),
    }
}

async fn sites(
    State(inner): State<Arc<Inner>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if !authorized(&inner, &headers, query_line("/api/Site", &query)) {
        return unauthorized();
    }
    let client: i64 = query
        .get("client_id")
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    // A bare array: the other shape a list may take.
    let found: Vec<_> = SITES
        .iter()
        .filter(|(_, c, _)| *c == client)
        .map(|(id, c, name)| json!({"id": id, "client_id": c, "name": name}))
        .collect();
    Json(json!(found)).into_response()
}

async fn site(
    State(inner): State<Arc<Inner>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    if !authorized(&inner, &headers, format!("GET /api/Site/{id}")) {
        return unauthorized();
    }
    match SITES.iter().find(|(s, _, _)| *s == id) {
        Some((id, client, name)) => {
            Json(json!({"id": id, "client_id": client, "name": name})).into_response()
        }
        None => (StatusCode::NOT_FOUND, "Site not found").into_response(),
    }
}
