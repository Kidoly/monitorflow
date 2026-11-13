//! Live registry of connected agents and relays, keyed by role and peer ID:
//! an ID is unique among agents and among relays, not across both, so an
//! agent and a relay of the same name are two peers that never replace,
//! receive for or close each other (ADR-0105).
//!
//! Each stream registers once its `Hello` is accepted and gets a connection
//! number. Every later update or removal names that number, so a stream that
//! was replaced by a newer connection of the same peer can never touch or
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
    /// A newer connection registered the same role and ID.
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
    /// Agent log lines received (ADR-0091).
    pub log_lines: u64,
    /// Log lines dropped by validation or this server's rate limit.
    pub dropped_log_lines: u64,
}

/// A snapshot of one registered peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInfo {
    /// The agent or relay ID: the registry key together with `role`.
    pub id: String,
    pub role: PeerRole,
    /// Set when a relay speaks for this agent.
    pub via_relay: Option<String>,
    /// Every relay the stream passes through, the connecting one first and
    /// `via_relay` last (ADR-0028). Empty for a peer's own connection, and
    /// for streams other instances hold.
    pub relay_path: Vec<String>,
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
    peers: HashMap<Key, Entry>,
    next_connection: u64,
    /// Sequence of the last envelope the server sent.
    last_sequence: u64,
    shutting_down: bool,
    /// Streams other server instances hold (ADR-0089), as last read.
    remote: HashMap<Key, PeerInfo>,
}

/// A peer: its role and its ID.
type Key = (PeerRole, String);

fn key(role: PeerRole, id: &str) -> Key {
    (role, id.to_owned())
}

/// Order of the listings: by ID, agents before relays of the same ID.
fn by_id(a: &PeerInfo, b: &PeerInfo) -> std::cmp::Ordering {
    (&a.id, a.role.as_str()).cmp(&(&b.id, b.role.as_str()))
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

    /// Adds a peer, replacing and closing any connection with the same
    /// role and ID.
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
        let (role, id) = (info.role, info.id.clone());
        let entry = Entry {
            info,
            outbound,
            close,
        };
        if let Some(old) = inner.peers.insert((role, id.clone()), entry) {
            tracing::info!(
                %role,
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

    /// Applies `update` to the entry of the peer if `connection` still owns
    /// it. Returns false when the connection was replaced or removed.
    pub fn update(
        &self,
        role: PeerRole,
        id: &str,
        connection: u64,
        update: impl FnOnce(&mut PeerInfo),
    ) -> bool {
        match self.lock().peers.get_mut(&key(role, id)) {
            Some(entry) if entry.info.connection == connection => {
                update(&mut entry.info);
                true
            }
            _ => false,
        }
    }

    /// Removes the entry of the peer if `connection` still owns it.
    pub fn remove(&self, role: PeerRole, id: &str, connection: u64) -> bool {
        let mut inner = self.lock();
        let key = key(role, id);
        match inner.peers.get(&key) {
            Some(entry) if entry.info.connection == connection => {
                inner.peers.remove(&key);
                true
            }
            _ => false,
        }
    }

    /// All connected peers, sorted by ID.
    pub fn connected(&self) -> Vec<PeerInfo> {
        let mut peers: Vec<_> = self.lock().peers.values().map(|e| e.info.clone()).collect();
        peers.sort_by(by_id);
        peers
    }

    pub fn get(&self, role: PeerRole, id: &str) -> Option<PeerInfo> {
        self.lock()
            .peers
            .get(&key(role, id))
            .map(|e| e.info.clone())
    }

    /// Replaces the streams other server instances hold (ADR-0089). Their
    /// entries have connection 0 and no counters; nothing can be sent on
    /// them from here.
    pub fn set_remote(&self, peers: Vec<PeerInfo>) {
        // A later entry, connected later, wins.
        self.lock().remote = peers
            .into_iter()
            .map(|p| ((p.role, p.id.clone()), p))
            .collect();
    }

    /// The peer's stream on this instance, or else on another one.
    pub fn live(&self, role: PeerRole, id: &str) -> Option<PeerInfo> {
        let inner = self.lock();
        let key = key(role, id);
        inner
            .peers
            .get(&key)
            .map(|e| e.info.clone())
            .or_else(|| inner.remote.get(&key).cloned())
    }

    /// Every stream of every instance, this one's first, sorted by ID.
    pub fn live_all(&self) -> Vec<PeerInfo> {
        let inner = self.lock();
        let mut peers: Vec<_> = inner.peers.values().map(|e| e.info.clone()).collect();
        peers.extend(
            inner
                .remote
                .iter()
                .filter(|(key, _)| !inner.peers.contains_key(*key))
                .map(|(_, p)| p.clone()),
        );
        peers.sort_by(by_id);
        peers
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

    /// Queues `envelope` on the stream of the peer. Waits while that
    /// stream's outbound queue is full.
    pub async fn send(
        &self,
        role: PeerRole,
        id: &str,
        envelope: Envelope,
    ) -> Result<(), RegistryError> {
        let outbound = self
            .lock()
            .peers
            .get(&key(role, id))
            .map(|e| e.outbound.clone())
            .ok_or_else(|| RegistryError::NotConnected(id.to_owned()))?;
        outbound
            .send(envelope)
            .await
            .map_err(|_| RegistryError::Disconnected(id.to_owned()))
    }

    /// Closes the stream of the peer with `reason` and, for a relay, every
    /// stream that passes through it, however many relays further down
    /// (ADR-0029), and removes them at once. Returns how many were closed.
    pub fn close(&self, role: PeerRole, id: &str, reason: CloseReason) -> usize {
        let closed: Vec<Entry> = {
            let mut inner = self.lock();
            let carried = |info: &PeerInfo| {
                role == PeerRole::Relay
                    && (info.via_relay.as_deref() == Some(id)
                        || info.relay_path.iter().any(|relay| relay == id))
            };
            let keys: Vec<Key> = inner
                .peers
                .iter()
                .filter(|(key, e)| (key.0 == role && key.1 == id) || carried(&e.info))
                .map(|(key, _)| key.clone())
                .collect();
            keys.iter().filter_map(|k| inner.peers.remove(k)).collect()
        };
        let count = closed.len();
        for entry in closed {
            entry.close(reason);
        }
        count
    }

    /// Closes with `reason` every stream whose connection presented one of
    /// `serials`, and removes them at once. Returns how many were closed.
    pub fn close_serials(&self, serials: &[Serial], reason: CloseReason) -> usize {
        let closed: Vec<Entry> = {
            let mut inner = self.lock();
            let keys: Vec<Key> = inner
                .peers
                .iter()
                .filter(|(_, e)| {
                    e.info
                        .cert_serial
                        .as_ref()
                        .is_some_and(|s| serials.contains(s))
                })
                .map(|(key, _)| key.clone())
                .collect();
            keys.iter().filter_map(|k| inner.peers.remove(k)).collect()
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
            relay_path: Vec::new(),
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
    fn live_views_merge_other_instances_and_prefer_this_one() {
        let registry = Registry::default();
        let (tx, _rx) = mpsc::channel(1);
        registry.register(info("here"), tx).unwrap();
        let mut elsewhere = info("here");
        elsewhere.hostname = "other".into();
        registry.set_remote(vec![elsewhere, info("there")]);
        assert_eq!(
            registry.get(PeerRole::Agent, "there"),
            None,
            "nothing to send on"
        );
        assert_eq!(registry.live(PeerRole::Agent, "there").unwrap().id, "there");
        assert_eq!(
            registry.live(PeerRole::Agent, "here").unwrap().hostname,
            "host"
        );
        let ids: Vec<_> = registry.live_all().into_iter().map(|p| p.id).collect();
        assert_eq!(ids, ["here", "there"]);
        registry.set_remote(Vec::new());
        assert_eq!(registry.live(PeerRole::Agent, "there"), None);
    }

    #[tokio::test]
    async fn closing_serials_closes_only_those_connections() {
        let registry = Registry::default();
        let (tx, _rx) = mpsc::channel(4);
        let with = |id: &str, serial: Option<u8>| PeerInfo {
            cert_serial: serial.map(|s| Serial::from_bytes(&[s])),
            ..info(id)
        };
        let minted = registry.register(with("a", Some(1)), tx.clone()).unwrap();
        let _kept = registry.register(with("b", Some(2)), tx.clone()).unwrap();
        let _unknown = registry.register(with("c", None), tx).unwrap();
        let serials = [Serial::from_bytes(&[1]), Serial::from_bytes(&[3])];
        assert_eq!(registry.close_serials(&serials, CloseReason::Revoked), 1);
        assert_eq!(minted.close.await, Ok(CloseReason::Revoked));
        assert!(registry.get(PeerRole::Agent, "a").is_none());
        assert!(registry.get(PeerRole::Agent, "b").is_some());
        assert!(registry.get(PeerRole::Agent, "c").is_some());
        assert_eq!(registry.close_serials(&[], CloseReason::Revoked), 0);
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
        assert!(
            !registry.update(PeerRole::Agent, "a", old.connection, |i| i.last_sequence =
                99)
        );
        assert!(!registry.remove(PeerRole::Agent, "a", old.connection));
        let entry = registry.get(PeerRole::Agent, "a").unwrap();
        assert_eq!(entry.connection, new.connection);
        assert_eq!(entry.last_sequence, 1);

        assert!(registry.remove(PeerRole::Agent, "a", new.connection));
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
        registry
            .send(PeerRole::Agent, "a", envelope.clone())
            .await
            .unwrap();
        assert_eq!(rx.recv().await, Some(envelope.clone()));
        assert_eq!(
            registry.send(PeerRole::Agent, "b", envelope.clone()).await,
            Err(RegistryError::NotConnected("b".into()))
        );
        drop(rx);
        assert_eq!(
            registry.send(PeerRole::Agent, "a", envelope).await,
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
        let relay = registry.register(relay_info("r1"), tx.clone()).unwrap();
        let behind = registry.register(relayed("a1"), tx.clone()).unwrap();
        let (_other, _) = register(&registry, "a2");
        // An agent of the relay's name is another peer: nothing closes.
        assert_eq!(
            registry.close(PeerRole::Agent, "r1", CloseReason::Revoked),
            0
        );
        assert_eq!(
            registry.close(PeerRole::Relay, "r1", CloseReason::Revoked),
            2
        );
        assert_eq!(relay.close.await, Ok(CloseReason::Revoked));
        assert_eq!(behind.close.await, Ok(CloseReason::Revoked));
        let ids: Vec<_> = registry.connected().into_iter().map(|p| p.id).collect();
        assert_eq!(ids, ["a2"]);
        assert_eq!(
            registry.close(PeerRole::Relay, "nobody", CloseReason::Revoked),
            0
        );
    }

    fn relay_info(id: &str) -> PeerInfo {
        PeerInfo {
            role: PeerRole::Relay,
            ..info(id)
        }
    }

    #[tokio::test]
    async fn an_agent_and_a_relay_of_one_id_are_two_peers() {
        let registry = Registry::default();
        let (agent_tx, mut agent_rx) = mpsc::channel(1);
        let (relay_tx, mut relay_rx) = mpsc::channel(1);
        let mut agent = registry.register(info("web-01"), agent_tx).unwrap();
        let relay = registry.register(relay_info("web-01"), relay_tx).unwrap();
        // Neither replaced the other.
        assert!(agent.close.try_recv().is_err());
        assert_eq!(registry.connected().len(), 2);
        assert_eq!(
            registry.get(PeerRole::Agent, "web-01").unwrap().connection,
            agent.connection
        );
        assert_eq!(
            registry.get(PeerRole::Relay, "web-01").unwrap().connection,
            relay.connection
        );
        // A connection number of one role owns nothing of the other.
        assert!(!registry.update(PeerRole::Agent, "web-01", relay.connection, |_| {}));
        assert!(!registry.remove(PeerRole::Agent, "web-01", relay.connection));

        let envelope = registry.envelope(Payload::Heartbeat(Default::default()));
        registry
            .send(PeerRole::Relay, "web-01", envelope.clone())
            .await
            .unwrap();
        assert_eq!(relay_rx.recv().await, Some(envelope));
        assert!(agent_rx.try_recv().is_err(), "the agent got nothing");

        // Streams other instances hold are kept apart the same way.
        registry.set_remote(vec![relay_info("db-01")]);
        assert_eq!(registry.live(PeerRole::Agent, "db-01"), None);
        assert!(registry.live(PeerRole::Relay, "db-01").is_some());
        let listed: Vec<_> = registry
            .live_all()
            .into_iter()
            .map(|p| (p.id, p.role))
            .collect();
        assert_eq!(
            listed,
            [
                ("db-01".to_owned(), PeerRole::Relay),
                ("web-01".to_owned(), PeerRole::Agent),
                ("web-01".to_owned(), PeerRole::Relay),
            ]
        );

        // Revoking the relay leaves the agent of the same name connected.
        assert_eq!(
            registry.close(PeerRole::Relay, "web-01", CloseReason::Revoked),
            1
        );
        assert_eq!(relay.close.await, Ok(CloseReason::Revoked));
        assert!(agent.close.try_recv().is_err());
        assert!(registry.get(PeerRole::Agent, "web-01").is_some());
    }

    #[tokio::test]
    async fn closing_a_relay_reaches_every_stream_passing_through_it() {
        let registry = Registry::default();
        let (tx, _rx) = mpsc::channel(1);
        // r0 -> r1 -> x: x is attached to r1, on r0's connection.
        let chained = |id: &str, role| PeerInfo {
            role,
            via_relay: Some("r1".into()),
            relay_path: vec!["r0".into(), "r1".into()],
            ..info(id)
        };
        let _r0 = registry.register(relay_info("r0"), tx.clone()).unwrap();
        let r1 = PeerInfo {
            via_relay: Some("r0".into()),
            relay_path: vec!["r0".into()],
            ..relay_info("r1")
        };
        let r1 = registry.register(r1, tx.clone()).unwrap();
        let x = registry
            .register(chained("x", PeerRole::Agent), tx.clone())
            .unwrap();
        let (mut direct, _) = register(&registry, "y");
        assert_eq!(
            registry.close(PeerRole::Relay, "r0", CloseReason::Revoked),
            3
        );
        assert_eq!(r1.close.await, Ok(CloseReason::Revoked));
        assert_eq!(x.close.await, Ok(CloseReason::Revoked));
        assert!(direct.close.try_recv().is_err());
    }
}
