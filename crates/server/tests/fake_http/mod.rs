//! An in-process HTTP endpoint standing in for VictoriaMetrics and
//! VictoriaLogs: records every request body and answers with scripted
//! statuses.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{OriginalUri, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::Router;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

/// One request the fake received.
#[derive(Debug, Clone)]
pub struct Received {
    pub path_and_query: String,
    pub headers: HeaderMap,
    pub body: Bytes,
    pub status: StatusCode,
}

#[derive(Default)]
struct Inner {
    received: Mutex<Vec<Received>>,
    /// Statuses for the next requests; 204 once empty.
    script: Mutex<VecDeque<StatusCode>>,
    /// When set, each request waits for a permit.
    gate: Mutex<Option<Arc<Semaphore>>>,
}

#[derive(Clone)]
pub struct FakeHttp {
    pub url: String,
    inner: Arc<Inner>,
}

impl FakeHttp {
    pub async fn start() -> Self {
        let inner = Arc::new(Inner::default());
        let app = Router::new()
            .route("/api/v1/write", post(record))
            .route("/insert/jsonline", post(record))
            .with_state(inner.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { url, inner }
    }

    /// Answers the next requests with `statuses`, in order.
    pub fn script(&self, statuses: &[u16]) {
        let mut script = self.inner.script.lock().unwrap();
        script.extend(statuses.iter().map(|s| StatusCode::from_u16(*s).unwrap()));
    }

    /// Makes requests wait until permits are added to the returned gate.
    pub fn hold(&self) -> Arc<Semaphore> {
        let gate = Arc::new(Semaphore::new(0));
        *self.inner.gate.lock().unwrap() = Some(gate.clone());
        gate
    }

    pub fn received(&self) -> Vec<Received> {
        self.inner.received.lock().unwrap().clone()
    }

    /// Requests that were answered with a 2xx status.
    pub fn accepted(&self) -> Vec<Received> {
        let mut received = self.received();
        received.retain(|r| r.status.is_success());
        received
    }

    /// Waits until `done` holds for the received requests.
    pub async fn wait_for(&self, what: &str, done: impl Fn(&[Received]) -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !done(&self.received()) {
            assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

async fn record(
    State(inner): State<Arc<Inner>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let gate = inner.gate.lock().unwrap().clone();
    if let Some(gate) = gate {
        gate.acquire().await.unwrap().forget();
    }
    let status = inner
        .script
        .lock()
        .unwrap()
        .pop_front()
        .unwrap_or(StatusCode::NO_CONTENT);
    inner.received.lock().unwrap().push(Received {
        path_and_query: uri.to_string(),
        headers,
        body,
        status,
    });
    status
}
