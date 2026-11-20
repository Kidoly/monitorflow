//! Sends commands on agents' streams, one in flight per agent.
//!
//! One task for the whole server. It looks at an agent when its stream
//! opens, when a command is queued for it, when one of its commands ends,
//! and every [`DispatcherSettings::poll_interval`] at every connected
//! agent with a command, after expiring stale commands. A command in
//! flight that was not sent on the agent's current connection is sent
//! again: the agent answers a command it already ran from its journal
//! (ADR-0031).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use monitorflow_common::command;
use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::{Command, CommandResult, PackageReport};
use monitorflow_common::tls::PeerRole;
use tokio::sync::mpsc;
use tokio::time::{interval, timeout, MissedTickBehavior};
use uuid::Uuid;

use super::{CommandRow, CommandStore, PackageStore, State};
use crate::ca::CertificateAuthority;
use crate::grpc::Registry;

/// Agents waiting to be looked at, each at most once: a second wake of an
/// agent already waiting adds nothing, so the queue never holds more than
/// the fleet. It held 1,024 wakes before, and a command queued through
/// the API wakes the dispatcher twice (the API, then its NOTIFY, ADR-0089):
/// 2,000 commands queued at once overflowed it and the rest waited for the
/// sweep (docs/load-test.md).
#[derive(Debug, Clone)]
struct WakeSender {
    tx: mpsc::UnboundedSender<String>,
    queued: Arc<Mutex<HashSet<String>>>,
}

struct WakeReceiver {
    rx: mpsc::UnboundedReceiver<String>,
    queued: Arc<Mutex<HashSet<String>>>,
}

fn wakes() -> (WakeSender, WakeReceiver) {
    let (tx, rx) = mpsc::unbounded_channel();
    let queued = Arc::new(Mutex::new(HashSet::new()));
    (
        WakeSender {
            tx,
            queued: queued.clone(),
        },
        WakeReceiver { rx, queued },
    )
}

fn lock(queued: &Mutex<HashSet<String>>) -> std::sync::MutexGuard<'_, HashSet<String>> {
    // Inserts and removes only: a panic leaves the set consistent.
    queued.lock().unwrap_or_else(|e| e.into_inner())
}

impl WakeSender {
    /// Queues `peer_id` unless it is waiting already; `false` when it was,
    /// or the dispatcher is gone.
    fn push(&self, peer_id: &str) -> bool {
        let mut queued = lock(&self.queued);
        if !queued.insert(peer_id.to_owned()) {
            return false;
        }
        if self.tx.send(peer_id.to_owned()).is_err() {
            queued.remove(peer_id);
            return false;
        }
        true
    }
}

impl WakeReceiver {
    /// The next agent; a wake arriving while it is looked at queues it
    /// again. `None` once every sender is dropped.
    async fn recv(&mut self) -> Option<String> {
        let peer = self.rx.recv().await?;
        lock(&self.queued).remove(&peer);
        Some(peer)
    }
}

#[derive(Debug, Clone)]
pub struct DispatcherSettings {
    /// Time between two sweeps of every connected agent.
    pub poll_interval: Duration,
    /// Time allowed to queue a command on a full stream.
    pub send_timeout: Duration,
    /// A sent command that did not report it started this long after its
    /// expiry is expired.
    pub sent_grace: Duration,
    /// A running command without a result this long after it started fails.
    pub run_timeout: Duration,
}

impl Default for DispatcherSettings {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(10),
            send_timeout: Duration::from_secs(1),
            sent_grace: Duration::from_secs(5 * 60),
            run_timeout: Duration::from_secs(2 * 60 * 60),
        }
    }
}

/// Handle shared by agent streams and the API; clones are cheap.
#[derive(Debug, Clone)]
pub struct Commands {
    store: CommandStore,
    packages: PackageStore,
    wake: WakeSender,
}

impl Commands {
    /// Starts the dispatcher, which runs until every handle is dropped.
    pub fn spawn(
        store: CommandStore,
        registry: Registry,
        ca: Arc<CertificateAuthority>,
        settings: DispatcherSettings,
    ) -> Self {
        let (wake, rx) = wakes();
        let dispatcher = Dispatcher {
            store: store.clone(),
            registry,
            ca,
            settings,
            wake: rx,
            delivered: HashMap::new(),
        };
        let packages = PackageStore::new(store.pool().clone());
        tokio::spawn(dispatcher.run());
        Self {
            store,
            packages,
            wake,
        }
    }

    pub fn store(&self) -> &CommandStore {
        &self.store
    }

    /// Asks the dispatcher to look at the agent `peer_id`: its stream
    /// opened or a command was queued for it.
    pub fn dispatch(&self, peer_id: &str) {
        self.wake.push(peer_id);
    }

    /// Stores the package report of the agent `peer_id`. Failures are
    /// logged; the agent reports again within its interval.
    pub async fn report(&self, peer_id: &str, report: PackageReport) {
        let count = report.packages.len();
        match self.packages.replace(peer_id, &report).await {
            Ok(true) => tracing::info!(
                agent_id = peer_id,
                packages = count,
                reboot_required = report.reboot_required,
                "package report stored"
            ),
            Ok(false) => tracing::debug!(
                agent_id = peer_id,
                "ignoring a package report older than the stored one"
            ),
            Err(error) => {
                tracing::warn!(agent_id = peer_id, %error, "could not store a package report")
            }
        }
    }

    /// Records a result from the agent `peer_id` and sends its next
    /// command once this one ended. Failures are logged: the command stays
    /// in flight and is sent again, and the agent answers from its journal.
    pub async fn record(&self, peer_id: &str, result: CommandResult) {
        match self.store.record(peer_id, &result).await {
            Ok(Some(state)) => {
                tracing::info!(agent_id = peer_id, command_id = %result.command_id, state = state.as_str(), "command result");
                if state.is_final() {
                    self.dispatch(peer_id);
                }
            }
            Ok(None) => tracing::debug!(
                agent_id = peer_id,
                command_id = %result.command_id.escape_debug(),
                "ignoring a result for no command in flight of this agent"
            ),
            Err(error) => tracing::warn!(
                agent_id = peer_id,
                command_id = %result.command_id.escape_debug(),
                %error,
                "could not record a command result"
            ),
        }
    }
}

struct Dispatcher {
    store: CommandStore,
    registry: Registry,
    ca: Arc<CertificateAuthority>,
    settings: DispatcherSettings,
    wake: WakeReceiver,
    /// The command last sent to each agent, and on which connection.
    delivered: HashMap<String, (u64, Uuid)>,
}

impl Dispatcher {
    async fn run(mut self) {
        let mut tick = interval(self.settings.poll_interval);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                peer = self.wake.recv() => match peer {
                    Some(peer) => self.dispatch(&peer).await,
                    None => return,
                },
                _ = tick.tick() => self.poll().await,
            }
        }
    }

    async fn poll(&mut self) {
        match self
            .store
            .expire(self.settings.sent_grace, self.settings.run_timeout)
            .await
        {
            Ok(0) => {}
            Ok(count) => tracing::info!(count, "expired commands"),
            Err(error) => tracing::warn!(%error, "could not expire commands; will retry"),
        }
        let agents: Vec<String> = self
            .registry
            .connected()
            .into_iter()
            .filter(|p| p.role == PeerRole::Agent)
            .map(|p| p.id)
            .collect();
        let connected: std::collections::HashSet<&String> = agents.iter().collect();
        self.delivered.retain(|id, _| connected.contains(id));
        // One query for the sweep, not one per connected agent.
        let busy = match self.store.with_commands(&agents).await {
            Ok(busy) => busy,
            Err(error) => {
                tracing::warn!(%error, "could not look for commands; will retry");
                return;
            }
        };
        for id in busy {
            self.dispatch(&id).await;
        }
    }

    async fn dispatch(&mut self, peer_id: &str) {
        let Some(peer) = self.registry.get(PeerRole::Agent, peer_id) else {
            return;
        };
        let row = match self.store.next(peer_id).await {
            Ok(Some(row)) => row,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(agent_id = peer_id, %error, "could not load commands; will retry");
                return;
            }
        };
        // The agent may leave between the registry check and the send.
        let fresh = row.state == State::Queued;
        if fresh {
            match self.store.mark_sent(row.id).await {
                Ok(true) => {}
                Ok(false) => return,
                Err(error) => {
                    tracing::warn!(agent_id = peer_id, %error, "could not mark a command sent; will retry");
                    return;
                }
            }
        } else if self.delivered.get(peer_id) == Some(&(peer.connection, row.id)) {
            return;
        }
        if self.send(peer_id, &row).await {
            self.delivered
                .insert(peer_id.to_owned(), (peer.connection, row.id));
        } else if fresh {
            if let Err(error) = self.store.unmark_sent(row.id).await {
                tracing::warn!(agent_id = peer_id, command_id = %row.id, %error, "could not return an unsent command to the queue; it is sent again");
            }
        }
    }

    /// Signs and queues `row` on the stream of `peer_id`. A failure leaves
    /// it in flight, to be sent again.
    async fn send(&self, peer_id: &str, row: &CommandRow) -> bool {
        let mut command = Command {
            command_id: row.id.to_string(),
            kind: row.kind.clone(),
            payload: row.payload.clone(),
            signature: Vec::new(),
            issued_at_unix_ms: row.issued_at.timestamp_millis(),
            expires_at_unix_ms: row.expires_at.timestamp_millis(),
            agent_id: peer_id.to_owned(),
        };
        command.signature = match self.ca.sign_command(&command::signed_bytes(&command)) {
            Ok(signature) => signature,
            Err(error) => {
                tracing::error!(command_id = %row.id, %error, "could not sign a command");
                return false;
            }
        };
        let envelope = self.registry.envelope(Payload::Command(command));
        match timeout(
            self.settings.send_timeout,
            self.registry.send(PeerRole::Agent, peer_id, envelope),
        )
        .await
        {
            Ok(Ok(())) => {
                tracing::info!(agent_id = peer_id, command_id = %row.id, kind = %row.kind, "command sent");
                true
            }
            Ok(Err(error)) => {
                tracing::debug!(agent_id = peer_id, %error, "command not sent");
                false
            }
            Err(_) => {
                tracing::debug!(agent_id = peer_id, "outbound queue full; command not sent");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_fleet_of_wakes_is_kept_once_each() {
        let (tx, mut rx) = wakes();
        // 2,000 commands queued at once, each waking twice: the API, then
        // the NOTIFY. The old queue of 1,024 dropped about half.
        for round in 0..2 {
            for n in 0..2_000 {
                assert_eq!(tx.push(&format!("agent-{n}")), round == 0);
            }
        }
        let mut seen = HashSet::new();
        for _ in 0..2_000 {
            assert!(seen.insert(rx.recv().await.unwrap()));
        }
        assert!(rx.rx.try_recv().is_err(), "each agent once");
    }

    #[tokio::test]
    async fn a_wake_while_an_agent_is_looked_at_queues_it_again() {
        let (tx, mut rx) = wakes();
        assert!(tx.push("web-01"));
        assert_eq!(rx.recv().await.as_deref(), Some("web-01"));
        // Its command ended while the dispatcher was sending the last one.
        assert!(tx.push("web-01"));
        assert_eq!(rx.recv().await.as_deref(), Some("web-01"));
        drop(tx);
        assert_eq!(rx.recv().await, None);
    }
}
