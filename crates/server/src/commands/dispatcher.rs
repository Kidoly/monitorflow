//! Sends commands on agents' streams, one in flight per agent.
//!
//! One task for the whole server. It looks at an agent when its stream
//! opens, when a command is queued for it, when one of its commands ends,
//! and every [`DispatcherSettings::poll_interval`] at every connected
//! agent, after expiring stale commands. A command in flight that was not
//! sent on the agent's current connection is sent again: the agent answers
//! a command it already ran from its journal (ADR-0031).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use monitorflow_common::command;
use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::{Command, CommandResult};
use monitorflow_common::tls::PeerRole;
use tokio::sync::mpsc;
use tokio::time::{interval, timeout, MissedTickBehavior};
use uuid::Uuid;

use super::{CommandRow, CommandStore, State};
use crate::ca::CertificateAuthority;
use crate::grpc::Registry;

/// Agents waiting to be looked at; more are dropped and the next poll
/// covers them.
const WAKE_QUEUE: usize = 1024;

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
    wake: mpsc::Sender<String>,
}

impl Commands {
    /// Starts the dispatcher, which runs until every handle is dropped.
    pub fn spawn(
        store: CommandStore,
        registry: Registry,
        ca: Arc<CertificateAuthority>,
        settings: DispatcherSettings,
    ) -> Self {
        let (wake, rx) = mpsc::channel(WAKE_QUEUE);
        let dispatcher = Dispatcher {
            store: store.clone(),
            registry,
            ca,
            settings,
            wake: rx,
            delivered: HashMap::new(),
        };
        tokio::spawn(dispatcher.run());
        Self { store, wake }
    }

    pub fn store(&self) -> &CommandStore {
        &self.store
    }

    /// Asks the dispatcher to look at the agent `peer_id`: its stream
    /// opened or a command was queued for it.
    pub fn dispatch(&self, peer_id: &str) {
        if self.wake.try_send(peer_id.to_owned()).is_err() {
            tracing::debug!(
                agent_id = peer_id,
                "dispatch queue full; the next poll sends it"
            );
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
    wake: mpsc::Receiver<String>,
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
        self.delivered.retain(|id, _| agents.contains(id));
        for id in agents {
            self.dispatch(&id).await;
        }
    }

    async fn dispatch(&mut self, peer_id: &str) {
        let Some(peer) = self.registry.get(peer_id) else {
            return;
        };
        if peer.role != PeerRole::Agent {
            return;
        }
        let row = match self.store.next(peer_id).await {
            Ok(Some(row)) => row,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(agent_id = peer_id, %error, "could not load commands; will retry");
                return;
            }
        };
        if row.state == State::Queued {
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
            self.registry.send(peer_id, envelope),
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
