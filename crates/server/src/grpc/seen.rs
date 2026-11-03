//! `last_seen_at` writes of every stream, through one task: a stream
//! queues its peer without waiting, and the task writes whatever gathered
//! meanwhile in one statement per role. A slow database then holds one
//! connection, and the peers queued meanwhile go in its next statement,
//! instead of a task per stream waiting for the pool (docs/load-test.md).

use std::collections::BTreeSet;
use std::sync::Arc;

use monitorflow_common::tls::PeerRole;
use tokio::sync::mpsc;

use super::inventory::Inventory;

/// Peers queued and not written yet; more are dropped, and their stream
/// queues them again after its interval.
const QUEUE: usize = 16_384;

/// Producer handle; clones share the task.
#[derive(Clone)]
pub struct SeenWriter {
    tx: mpsc::Sender<(PeerRole, String)>,
}

impl SeenWriter {
    /// Starts the task, which runs until every handle is dropped.
    pub fn spawn(inventory: Arc<dyn Inventory>) -> Self {
        let (tx, rx) = mpsc::channel(QUEUE);
        tokio::spawn(run(inventory, rx));
        Self { tx }
    }

    /// Queues `id` for a write. Never waits.
    pub fn record(&self, role: PeerRole, id: &str) {
        if self.tx.try_send((role, id.to_owned())).is_err() {
            tracing::debug!(
                agent_id = id,
                "last_seen_at queue full; the next interval writes it"
            );
        }
    }
}

async fn run(inventory: Arc<dyn Inventory>, mut rx: mpsc::Receiver<(PeerRole, String)>) {
    while let Some(first) = rx.recv().await {
        let (mut agents, mut relays) = (BTreeSet::new(), BTreeSet::new());
        let mut add = |(role, id): (PeerRole, String)| match role {
            PeerRole::Relay => relays.insert(id),
            _ => agents.insert(id),
        };
        add(first);
        while let Ok(next) = rx.try_recv() {
            add(next);
        }
        for (role, ids) in [(PeerRole::Agent, agents), (PeerRole::Relay, relays)] {
            if ids.is_empty() {
                continue;
            }
            let ids: Vec<String> = ids.into_iter().collect();
            if let Err(error) = inventory.seen(role, &ids).await {
                tracing::warn!(%role, peers = ids.len(), %error, "could not record last_seen_at");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use async_trait::async_trait;

    use super::*;
    use crate::grpc::{AcceptedHello, InventoryError};

    /// Records each call; the first one waits for `gate`.
    struct Recording {
        calls: Mutex<Vec<(PeerRole, Vec<String>)>>,
        gate: tokio::sync::Semaphore,
    }

    #[async_trait]
    impl Inventory for Recording {
        async fn hello(&self, _hello: AcceptedHello<'_>) -> Result<String, InventoryError> {
            Ok(String::new())
        }

        async fn seen(&self, role: PeerRole, ids: &[String]) -> Result<(), InventoryError> {
            let first = self.calls.lock().unwrap().is_empty();
            self.calls.lock().unwrap().push((role, ids.to_vec()));
            if first {
                let _ = self.gate.acquire().await.unwrap();
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn peers_queued_during_a_write_go_in_the_next_one() {
        let inventory = Arc::new(Recording {
            calls: Mutex::default(),
            gate: tokio::sync::Semaphore::new(0),
        });
        let writer = SeenWriter::spawn(inventory.clone());
        writer.record(PeerRole::Agent, "web-01");
        // The first write is now stuck, like on a slow database.
        while inventory.calls.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        for id in ["web-03", "web-02", "web-03"] {
            writer.record(PeerRole::Agent, id);
        }
        writer.record(PeerRole::Relay, "relay-1");
        inventory.gate.add_permits(1);
        while inventory.calls.lock().unwrap().len() < 3 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let calls = inventory.calls.lock().unwrap().clone();
        let ids = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            calls,
            [
                (PeerRole::Agent, ids(&["web-01"])),
                (PeerRole::Agent, ids(&["web-02", "web-03"])),
                (PeerRole::Relay, ids(&["relay-1"])),
            ]
        );
    }
}
