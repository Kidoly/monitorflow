//! Resends configurations when they change and records acknowledgements.
//!
//! One task for the whole server. Every [`WatcherSettings::poll_interval`]
//! it reads the revisions of the connected agents in one query and sends a
//! new configuration to each agent whose revision differs from the one its
//! connection last got. Acknowledgements are written as they arrive; a
//! failed write is kept and retried at the next poll, so is a failed poll.
//! Polling was preferred to LISTEN/NOTIFY: see ADR-0016.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::tls::PeerRole;
use tokio::sync::mpsc;
use tokio::time::{interval, timeout, MissedTickBehavior};

use super::{Resolved, SensorConfigStore};
use crate::grpc::Registry;

/// Acknowledgements wait in a map, the latest revision per agent, so none
/// is ever dropped and the map never holds more than the fleet: a queue of
/// 1,024 dropped most of a 1,600-agent site's acknowledgements, and one of
/// 16,384 those of a 20,000-agent site (docs/load-test.md). A channel of
/// one wakes the watcher.
type Acks = Arc<Mutex<HashMap<String, u64>>>;

/// Most acknowledgements in one statement.
const ACK_BATCH: usize = 2_000;

#[derive(Debug, Clone)]
pub struct WatcherSettings {
    /// Time between two revision checks.
    pub poll_interval: Duration,
    /// Time allowed to queue a configuration on a stream whose outbound
    /// queue is full; the next poll tries again.
    pub send_timeout: Duration,
}

impl Default for WatcherSettings {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(10),
            send_timeout: Duration::from_secs(1),
        }
    }
}

/// Handle used by agent streams: loads configurations at Hello and passes
/// acknowledgements to the watcher task.
#[derive(Debug, Clone)]
pub struct SensorConfigs {
    store: SensorConfigStore,
    acks: Acks,
    wake: mpsc::Sender<()>,
}

impl SensorConfigs {
    /// Starts the watcher task, which runs until every handle is dropped.
    pub fn spawn(store: SensorConfigStore, registry: Registry, settings: WatcherSettings) -> Self {
        let (wake, woken) = mpsc::channel(1);
        let acks = Acks::default();
        let watcher = Watcher {
            store: store.clone(),
            registry,
            settings,
            acks: acks.clone(),
            woken,
            pending: HashMap::new(),
        };
        tokio::spawn(watcher.run());
        Self { store, acks, wake }
    }

    /// The effective configuration of the agent `peer_id`.
    pub async fn load(&self, peer_id: &str) -> Result<Option<Resolved>, sqlx::Error> {
        self.store.load(peer_id).await
    }

    /// Queues the revision acknowledged by `peer_id` for writing.
    pub fn applied(&self, peer_id: &str, revision: u64) {
        lock(&self.acks).insert(peer_id.to_owned(), revision);
        // Full: a wake is pending already, and takes this one too.
        let _ = self.wake.try_send(());
    }
}

fn lock(acks: &Mutex<HashMap<String, u64>>) -> std::sync::MutexGuard<'_, HashMap<String, u64>> {
    // Inserts and takes only: a panic leaves the map consistent.
    acks.lock().unwrap_or_else(|e| e.into_inner())
}

struct Watcher {
    store: SensorConfigStore,
    registry: Registry,
    settings: WatcherSettings,
    acks: Acks,
    woken: mpsc::Receiver<()>,
    /// Acknowledgements not written yet, latest per agent.
    pending: HashMap<String, u64>,
}

impl Watcher {
    async fn run(mut self) {
        let mut tick = interval(self.settings.poll_interval);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                woken = self.woken.recv() => match woken {
                    Some(()) => {
                        let acks = std::mem::take(&mut *lock(&self.acks));
                        self.pending.extend(acks);
                        self.record().await;
                    }
                    None => return,
                },
                _ = tick.tick() => {
                    self.record().await;
                    self.poll().await;
                }
            }
        }
    }

    async fn record(&mut self) {
        let pending: Vec<(String, u64)> = std::mem::take(&mut self.pending).into_iter().collect();
        for batch in pending.chunks(ACK_BATCH) {
            if let Err(error) = self.store.record_applied_many(batch).await {
                tracing::warn!(agents = batch.len(), %error, "could not record the applied sensor configurations; will retry");
                for (id, revision) in batch {
                    self.pending.entry(id.clone()).or_insert(*revision);
                }
            }
        }
    }

    async fn poll(&self) {
        let peers: Vec<_> = self
            .registry
            .connected()
            .into_iter()
            .filter(|p| p.role == PeerRole::Agent && p.sensor_revision_sent.is_some())
            .map(|p| (p.id, (p.connection, p.sensor_revision_sent)))
            .collect();
        if peers.is_empty() {
            return;
        }
        let ids: Vec<String> = peers.iter().map(|(id, _)| id.clone()).collect();
        let revisions = match self.store.revisions(&ids).await {
            Ok(revisions) => revisions,
            Err(error) => {
                tracing::warn!(%error, "could not check sensor configuration revisions; will retry");
                return;
            }
        };
        let peers: HashMap<_, _> = peers.into_iter().collect();
        for (id, revision) in revisions {
            match peers.get(&id) {
                Some(&(connection, sent)) if sent != Some(revision) => {
                    self.resend(&id, connection).await;
                }
                _ => {}
            }
        }
    }

    /// Sends the current configuration of `id`; a failure is retried at
    /// the next poll since the sent revision stays the old one.
    async fn resend(&self, id: &str, connection: u64) {
        let config = match self.store.load(id).await {
            Ok(Some(config)) => config,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(agent_id = id, %error, "could not load the sensor configuration; will retry");
                return;
            }
        };
        let envelope = self
            .registry
            .envelope(Payload::SensorConfig(config.to_proto()));
        let sending = self.registry.send(PeerRole::Agent, id, envelope);
        match timeout(self.settings.send_timeout, sending).await {
            Ok(Ok(())) => {
                tracing::info!(
                    agent_id = id,
                    revision = config.revision,
                    "sent a new sensor configuration"
                );
                self.registry
                    .update(PeerRole::Agent, id, connection, |info| {
                        info.sensor_revision_sent = Some(config.revision);
                    });
            }
            Ok(Err(error)) => {
                tracing::debug!(agent_id = id, %error, "sensor configuration not sent")
            }
            Err(_) => tracing::debug!(
                agent_id = id,
                "outbound queue full; sensor configuration not sent"
            ),
        }
    }
}
