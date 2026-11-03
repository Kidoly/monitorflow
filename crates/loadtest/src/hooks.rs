//! A webhook receiver on loopback for the alert storm: counts the
//! notification requests the server sends.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::Router;

pub struct Receiver {
    addr: SocketAddr,
    received: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Receiver {
    pub async fn start() -> anyhow::Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let received = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route("/hook", post(receive))
            .with_state(received.clone());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self {
            addr,
            received,
            task,
        })
    }

    pub fn url(&self) -> String {
        format!("http://{}/hook", self.addr)
    }

    /// Requests received so far.
    pub fn count(&self) -> usize {
        self.received.load(Ordering::Relaxed)
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn receive(State(received): State<Arc<AtomicUsize>>, _body: String) -> StatusCode {
    received.fetch_add(1, Ordering::Relaxed);
    StatusCode::NO_CONTENT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn counts_posts() {
        let receiver = Receiver::start().await.unwrap();
        let client = reqwest::Client::new();
        for _ in 0..3 {
            let response = client.post(receiver.url()).body("{}").send().await.unwrap();
            assert_eq!(response.status(), 204);
        }
        assert_eq!(receiver.count(), 3);
    }
}
