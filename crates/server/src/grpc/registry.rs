//! Live registry of connected agents and relays, keyed by peer ID.
//!
//! Each stream registers once its `Hello` is accepted and gets a connection
//! number. Every later update or removal names that number, so a stream that
//! was replaced by a newer connection with the same ID can never touch or
//! remove its successor's entry.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::{DateTime, Utc};
use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::Envelope;
use monitorflow_common::tls::PeerRole;
use tokio::sync::{mpsc, oneshot};

use crate::ca::Serial;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RegistryError {
    #[error("peer {0} is not connected")]
    NotConnected(String),
    #[error("stream of peer {0} closed before the envelope was delivered")]
    Disconnected(String),
    #[error("the server is shutting down")]
    ShuttingDown,
}

/// Why the registry asks a stream to close.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// A newer connection registered the same ID.
    Replaced,
    /// The server is shutting down.
    Shutdown,
    /// An administrator revoked the peer, or the relay carrying it
    /// (ADR-0029).
    Revoked,
}

/// Counters for one connection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    /// Envelopes dropped because their sequence was not above the last one.
    pub stale_sequences: u64,
    pub sample_batches: u64,
    pub samples: u64,
    /// Samples dropped by validation (ADR-0013).
    pub invalid_samples: u64,
    /// Valid samples dropped because the metrics queue was full.
    pub dropped_samples: u64,
    pub command_results: u64,
}

/// A snapshot of one registered peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInfo {
    /// The agent or relay ID, the registry key.
    pub id: String,
    pub role: PeerRole,
    /// Set when a relay speaks for this agent.
    pub via_relay: Option<String>,
    /// Assigned by [`Registry::register`]; unique for the process lifetime.
    pub connection: u64,
    /// Serial of the connection's client certificate: the relay's when
    /// relayed. Changes when a peer reconnects with a renewed certificate.
    pub cert_serial: Option<Serial>,
    pub agent_version: String,
    pub hostname: String,
    pub connected_at: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub last_sequence: u64,
    pub counters: Counters,
    /// Revision of the last sensor configuration sent on this connection;
    /// `None` when none is sent, as to relays (ADR-0016).
    pub sensor_revision_sent: Option<u64>,
    /// Revision the agent last acknowledged on this connection.
    pub sensor_revision_applied: Option<u64>,
}

struct Entry {
    info: PeerInfo,
    outbound: mpsc::Sender<Envelope>,
    close: oneshot::Sender<CloseReason>,
}

impl Entry {
    fn close(self, reason: CloseReason) {
        // The stream may already be gone; nothing to do then.
        let _ = self.close.send(reason);
    }
}

#[derive(Default)]
struct Inner {
    peers: HashMap<String, Entry>,
    next_connection: u64,
    /// Sequence of the last envelope the server sent.
    last_sequence: u64,
    shutting_down: bool,
}

/// Shared handle to the registry; clones are cheap and see the same peers.
#[derive(Clone, Default)]
pub struct Registry {
    inner: Arc<Mutex<Inner>>,
}

/// What a stream gets back from [`Registry::register`].
#[derive(Debug)]
pub struct Registration {
    pub connection: u64,
    /// Resolves when the registry wants this stream closed. Dropped without a
    /// value when the entry is gone for another reason.
    pub close: oneshot::Receiver<CloseReason>,
}

impl Registry {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panic while holding the lock leaves the map consistent: every
        // critical section is a single insert, remove or field update.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Adds a peer, replacing and closing any connection with the same ID.
    /// `info.connection` is ignored and assigned here.
    pub fn register(
        &self,
        mut info: PeerInfo,
        outbound: mpsc::Sender<Envelope>,
    ) -> Result<Registration, RegistryError> {
        let (close, close_rx) = oneshot::channel();
        let mut inner = self.lock();
        if inner.shutting_down {
            return Err(RegistryError::ShuttingDown);
        }
        inner.next_connection += 1;
        info.connection = inner.next_connection;
        let connection = info.connection;
        let id = info.id.clone();
        let entry = Entry {
            info,
            outbound,
            close,
        };
        if let Some(old) = inner.peers.insert(id.clone(), entry) {
            tracing::info!(
                peer = %id,
                old_connection = old.info.connection,
                new_connection = connection,
                "replacing existing connection"
            );
            old.close(CloseReason::Replaced);
        }
        Ok(Registration {
            connection,
            close: close_rx,
        })
    }

    /// Applies `update` to the entry of `id` if `connection` still owns it.
    /// Returns false when the connection was replaced or removed.
    pub fn update(&self, id: &str, connection: u64, update: impl FnOnce(&mut PeerInfo)) -> bool {
        match self.lock().peers.get_mut(id) {
            Some(entry) if entry.info.connection == connection => {
                update(&mut entry.info);
                true
            }
            _ => false,
        }
    }

    /// Removes the entry of `id` if `connection` still owns it.
    pub fn remove(&self, id: &str, connection: u64) -> bool {
        let mut inner = self.lock();
        match inner.peers.get(id) {
            Some(entry) if entry.info.connection == connection => {
                inner.peers.remove(id);
                true
            }
            _ => false,
        }
    }

    /// All connected peers, sorted by ID.
    pub fn connected(&self) -> Vec<PeerInfo> {
        let mut peers: Vec<_> = self.lock().peers.values().map(|e| e.info.clone()).collect();
        peers.sort_by(|a, b| a.id.cmp(&b.id));
        peers
    }

    pub fn get(&self, id: &str) -> Option<PeerInfo> {
        self.lock().peers.get(id).map(|e| e.info.clone())
    }

    /// Wraps `payload` with the server's next sequence number, monotonic
    /// over every stream of this process.
    pub fn envelope(&self, payload: Payload) -> Envelope {
        let mut inner = self.lock();
        inner.last_sequence += 1;
        Envelope {
            sequence: inner.last_sequence,
            payload: Some(payload),
        }
    }

    /// Queues `envelope` on the stream of `id`. Waits while that stream's
    /// outbound queue is full.
    pub async fn send(&self, id: &str, envelope: Envelope) -> Result<(), RegistryError> {
        let outbound = self
            .lock()
            .peers
            .get(id)
            .map(|e| e.outbound.clone())
            .ok_or_else(|| RegistryError::NotConnected(id.to_owned()))?;
        outbound
            .send(envelope)
            .await
            .map_err(|_| RegistryError::Disconnected(id.to_owned()))
    }

    /// Closes the stream of `id` and every stream relayed by `id` with
    /// `reason`, and removes them at once. Returns how many were closed.
    pub fn close(&self, id: &str, reason: CloseReason) -> usize {
        let closed: Vec<Entry> = {
            let mut inner = self.lock();
            let ids: Vec<String> = inner
                .peers
                .values()
                .filter(|e| e.info.id == id || e.info.via_relay.as_deref() == Some(id))
                .map(|e| e.info.id.clone())
                .collect();
            ids.iter().filter_map(|i| inner.peers.remove(i)).collect()
        };
        let count = closed.len();
        for entry in closed {
            entry.close(reason);
        }
        count
    }

    /// Closes every stream and refuses new registrations.
    pub fn close_all(&self) {
        let peers = {
            let mut inner = self.lock();
            inner.shutting_down = true;
            std::mem::take(&mut inner.peers)
        };
        for entry in peers.into_values() {
            entry.close(CloseReason::Shutdown);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: &str) -> PeerInfo {
        let now = Utc::now();
        PeerInfo {
            id: id.into(),
            role: PeerRole::Agent,
            via_relay: None,
            connection: 0,
            cert_serial: None,
            agent_version: "0.1.0".into(),
            hostname: "host".into(),
            connected_at: now,
            last_seen: now,
            last_sequence: 1,
            counters: Counters::default(),
            sensor_revision_sent: None,
            sensor_revision_applied: None,
        }
    }

    #[test]
    fn envelopes_get_increasing_sequences() {
        let registry = Registry::default();
        let first = registry.envelope(Payload::Heartbeat(Default::default()));
        let second = registry
            .clone()
            .envelope(Payload::Heartbeat(Default::default()));
        assert_eq!((first.sequence, second.sequence), (1, 2));
    }

    fn register(registry: &Registry, id: &str) -> (Registration, mpsc::Receiver<Envelope>) {
        let (tx, rx) = mpsc::channel(4);
        (registry.register(info(id), tx).unwrap(), rx)
    }

    #[tokio::test]
    async fn replacement_closes_the_old_stream_and_keeps_the_new_entry() {
        let registry = Registry::default();
        let (old, _old_rx) = register(&registry, "a");
        let (new, _new_rx) = register(&registry, "a");
        assert_ne!(old.connection, new.connection);
        assert_eq!(old.close.await, Ok(CloseReason::Replaced));

        // The old stream's teardown must not touch its successor.
        assert!(!registry.update("a", old.connection, |i| i.last_sequence = 99));
        assert!(!registry.remove("a", old.connection));
        let entry = registry.get("a").unwrap();
        assert_eq!(entry.connection, new.connection);
        assert_eq!(entry.last_sequence, 1);

        assert!(registry.remove("a", new.connection));
        assert!(registry.connected().is_empty());
    }

    #[tokio::test]
    async fn send_reaches_the_registered_stream() {
        let registry = Registry::default();
        let (_reg, mut rx) = register(&registry, "a");
        let envelope = Envelope {
            sequence: 7,
            payload: None,
        };
        registry.send("a", envelope.clone()).await.unwrap();
        assert_eq!(rx.recv().await, Some(envelope.clone()));
        assert_eq!(
            registry.send("b", envelope.clone()).await,
            Err(RegistryError::NotConnected("b".into()))
        );
        drop(rx);
        assert_eq!(
            registry.send("a", envelope).await,
            Err(RegistryError::Disconnected("a".into()))
        );
    }

    #[tokio::test]
    async fn close_all_closes_streams_and_refuses_new_ones() {
        let registry = Registry::default();
        let (b, _) = register(&registry, "b");
        let (a, _) = register(&registry, "a");
        let ids: Vec<_> = registry.connected().into_iter().map(|p| p.id).collect();
        assert_eq!(ids, ["a", "b"]);
        registry.close_all();
        assert_eq!(a.close.await, Ok(CloseReason::Shutdown));
        assert_eq!(b.close.await, Ok(CloseReason::Shutdown));
        assert!(registry.connected().is_empty());
        let (tx, _rx) = mpsc::channel(1);
        assert_eq!(
            registry.register(info("c"), tx).unwrap_err(),
            RegistryError::ShuttingDown
        );
    }

    #[tokio::test]
    async fn close_ends_a_peer_and_the_streams_it_relays() {
        let registry = Registry::default();
        let (tx, _rx) = mpsc::channel(1);
        let relayed = |id: &str| PeerInfo {
            via_relay: Some("r1".into()),
            ..info(id)
        };
        let relay = registry.register(info("r1"), tx.clone()).unwrap();
        let behind = registry.register(relayed("a1"), tx.clone()).unwrap();
        let (_other, _) = register(&registry, "a2");
        assert_eq!(registry.close("r1", CloseReason::Revoked), 2);
        assert_eq!(relay.close.await, Ok(CloseReason::Revoked));
        assert_eq!(behind.close.await, Ok(CloseReason::Revoked));
        let ids: Vec<_> = registry.connected().into_iter().map(|p| p.id).collect();
        assert_eq!(ids, ["a2"]);
        assert_eq!(registry.close("nobody", CloseReason::Revoked), 0);
    }
}
