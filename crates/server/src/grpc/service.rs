//! `AgentService::Stream`: identity checks, `Hello`, then the per-connection
//! loop that tracks liveness and sequence numbers and hands samples to
//! ingest (ADR-0013) and to the threshold engine (ADR-0017). Agents get their sensor configuration right after
//! `Hello` (ADR-0016), and their commands from the dispatcher (ADR-0031).
//!
//! Envelope payloads are never logged: command payloads and results will
//! carry sensitive data.

use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use chrono::Utc;
use monitorflow_common::proto::v1::agent_service_server::AgentService;
use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::{Envelope, Hello, SampleBatch, SensorConfigAck};
use monitorflow_common::release::Version;
use monitorflow_common::tls::{self, PeerIdentity, PeerRole};
use monitorflow_common::via::Via;
use monitorflow_common::{MIN_PEER_VERSION, PROTOCOL_VERSION};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{sleep, timeout, Instant};
use tokio_stream::Stream;
use tonic::{Request, Response, Status, Streaming};

use super::inventory::{AcceptedHello, Inventory, InventoryError, NoInventory};
use super::policy::RelayPolicy;
use super::registry::{CloseReason, Counters, PeerInfo, Registry};
use super::StreamSettings;
use crate::alerting::Alerts;
use crate::commands::Commands;
use crate::ingest::{self, Identity, Ingest, LogRecord};
use crate::relay_commands::RelayCommands;
use crate::renew::PresentedCert;
use crate::sensor_config::{Resolved, SensorConfigs};

/// Least time between two warnings about invalid samples on one stream.
const INVALID_SAMPLE_LOG_INTERVAL: Duration = Duration::from_secs(60);
/// Longest error text from an agent written to the log.
const MAX_LOGGED_ERROR: usize = 256;

/// Why a stream was refused before registration.
#[derive(Debug, thiserror::Error)]
pub enum HandshakeError {
    #[error("role server may not open an agent stream")]
    ServerRole,
    #[error("no Hello within {0:?}")]
    HelloTimeout(Duration),
    #[error("stream closed before Hello")]
    ClosedBeforeHello,
    #[error("the first envelope must be Hello")]
    NotHello,
    #[error("stream failed before Hello: {0}")]
    Transport(Box<Status>),
    #[error("invalid protocol_version `{0}`, expected MAJOR.MINOR")]
    BadVersion(String),
    #[error("protocol version {peer} is not compatible with server version {server}")]
    VersionMismatch { peer: String, server: &'static str },
    #[error(
        "version {peer} is older than {minimum}, the oldest this server supports; reinstall it"
    )]
    TooOld { peer: String, minimum: &'static str },
    #[error("invalid agent_id: {0}")]
    BadAgentId(String),
    #[error("{peer} may not speak for agent {agent_id}")]
    NotAllowed {
        peer: PeerIdentity,
        agent_id: String,
    },
    #[error("invalid Hello: {0}")]
    BadHello(String),
    #[error(transparent)]
    Inventory(#[from] InventoryError),
    #[error("sensor configuration database error: {0}")]
    SensorConfig(#[source] sqlx::Error),
}

impl From<HandshakeError> for Status {
    fn from(error: HandshakeError) -> Self {
        let message = error.to_string();
        match error {
            HandshakeError::ServerRole | HandshakeError::NotAllowed { .. } => {
                Status::permission_denied(message)
            }
            HandshakeError::HelloTimeout(_) => Status::deadline_exceeded(message),
            HandshakeError::VersionMismatch { .. } | HandshakeError::TooOld { .. } => {
                Status::failed_precondition(message)
            }
            HandshakeError::Transport(status) => *status,
            HandshakeError::ClosedBeforeHello
            | HandshakeError::NotHello
            | HandshakeError::BadVersion(_)
            | HandshakeError::BadAgentId(_)
            | HandshakeError::BadHello(_) => Status::invalid_argument(message),
            HandshakeError::Inventory(InventoryError::Db(_)) => {
                Status::unavailable("inventory unavailable; retry later")
            }
            // Agents retry UNAVAILABLE, so they connect once approved.
            HandshakeError::Inventory(InventoryError::AwaitingApproval) => {
                Status::unavailable(message)
            }
            HandshakeError::SensorConfig(_) => {
                Status::unavailable("sensor configuration unavailable; retry later")
            }
            HandshakeError::Inventory(_) => Status::permission_denied(message),
        }
    }
}

/// Major component of a `MAJOR.MINOR` version.
fn major(version: &str) -> Option<u64> {
    let (major, minor) = version.split_once('.')?;
    minor.parse::<u64>().ok()?;
    major.parse().ok()
}

/// Accepts any minor of our major: the server must accept agents one minor
/// behind, and newer minors only add fields we ignore.
fn check_version(peer: &str) -> Result<(), HandshakeError> {
    let server = major(PROTOCOL_VERSION).expect("PROTOCOL_VERSION is MAJOR.MINOR");
    match major(peer) {
        None => Err(HandshakeError::BadVersion(peer.to_owned())),
        Some(m) if m != server => Err(HandshakeError::VersionMismatch {
            peer: peer.to_owned(),
            server: PROTOCOL_VERSION,
        }),
        Some(_) => Ok(()),
    }
}

/// Refuses releases older than [`MIN_PEER_VERSION`] (ADR-0055). Builds that
/// report no version, or something else, are accepted: development builds.
fn check_release(peer: &str) -> Result<(), HandshakeError> {
    let minimum: Version = MIN_PEER_VERSION
        .parse()
        .expect("MIN_PEER_VERSION is a version");
    match Version::from_str(peer) {
        Ok(version) if version < minimum => Err(HandshakeError::TooOld {
            peer: peer.to_owned(),
            minimum: MIN_PEER_VERSION,
        }),
        _ => Ok(()),
    }
}

/// Values stored in the `servers` row (ADR-0008); the schema accepts these
/// `os_family` values only; `windows` since ADR-0047, `linux` since
/// ADR-0063.
const OS_FAMILIES: [&str; 4] = ["debian", "rhel", "windows", "linux"];
const MAX_HOSTNAME: usize = 253;
const MAX_VERSION: usize = 128;

/// Checks the fields of an agent's Hello that go into its `servers` row.
fn check_agent_hello(hello: &Hello) -> Result<(), HandshakeError> {
    let bad = |why: String| Err(HandshakeError::BadHello(why));
    if !OS_FAMILIES.contains(&hello.os_family.as_str()) {
        return bad(format!(
            "os_family `{}` must be one of {}",
            hello.os_family.escape_debug(),
            OS_FAMILIES.join(", ")
        ));
    }
    if hello.hostname.is_empty() || hello.hostname.len() > MAX_HOSTNAME {
        return bad(format!("hostname must be 1 to {MAX_HOSTNAME} bytes"));
    }
    if hello.os_version.len() > MAX_VERSION || hello.agent_version.len() > MAX_VERSION {
        return bad(format!(
            "os_version and agent_version must be at most {MAX_VERSION} bytes"
        ));
    }
    Ok(())
}

/// The `AgentService` implementation of the central server.
#[derive(Clone)]
pub struct AgentStreamService {
    registry: Registry,
    policy: Arc<dyn RelayPolicy>,
    inventory: Arc<dyn Inventory>,
    ingest: Ingest,
    sensor_configs: Option<SensorConfigs>,
    alerts: Option<Alerts>,
    commands: Option<Commands>,
    relay_commands: Option<RelayCommands>,
    settings: StreamSettings,
}

impl AgentStreamService {
    /// A service that records nothing in the inventory; the server adds a
    /// [`DbInventory`](super::DbInventory) with [`Self::with_inventory`].
    pub fn new(registry: Registry, policy: Arc<dyn RelayPolicy>, settings: StreamSettings) -> Self {
        Self {
            registry,
            policy,
            inventory: Arc::new(NoInventory),
            ingest: Ingest::default(),
            sensor_configs: None,
            alerts: None,
            commands: None,
            relay_commands: None,
            settings,
        }
    }

    /// Sends relays their own commands and records results (ADR-0055).
    pub fn with_relay_commands(mut self, commands: RelayCommands) -> Self {
        self.relay_commands = Some(commands);
        self
    }

    /// Sends agents their commands and records results (ADR-0031);
    /// without it results are counted and discarded.
    pub fn with_commands(mut self, commands: Commands) -> Self {
        self.commands = Some(commands);
        self
    }

    /// Sends agents their sensor configuration after Hello and records
    /// their acknowledgements; without it agents get none.
    pub fn with_sensor_configs(mut self, sensor_configs: SensorConfigs) -> Self {
        self.sensor_configs = Some(sensor_configs);
        self
    }

    /// Sends samples and stream events to these queues; without it they
    /// are counted and discarded.
    pub fn with_ingest(mut self, ingest: Ingest) -> Self {
        self.ingest = ingest;
        self
    }

    /// Evaluates valid samples against thresholds (ADR-0017); without it
    /// no incident opens.
    pub fn with_alerts(mut self, alerts: Alerts) -> Self {
        self.alerts = Some(alerts);
        self
    }

    pub fn with_inventory(mut self, inventory: Arc<dyn Inventory>) -> Self {
        self.inventory = inventory;
        self
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Waits for `Hello`, checks it against the peer's certificate and
    /// records it in the inventory.
    ///
    /// `via` is the path a relay sent (ADR-0028): the Hello is then about
    /// its origin, attached to the last relay of the path.
    async fn handshake(
        &self,
        peer: &PeerIdentity,
        via: Option<Via>,
        inbound: &mut Streaming<Envelope>,
    ) -> Result<Accepted, HandshakeError> {
        if peer.role() == PeerRole::Server {
            return Err(HandshakeError::ServerRole);
        }
        let first = match timeout(self.settings.hello_timeout, inbound.message()).await {
            Err(_) => return Err(HandshakeError::HelloTimeout(self.settings.hello_timeout)),
            Ok(Err(status)) => return Err(HandshakeError::Transport(Box::new(status))),
            Ok(Ok(None)) => return Err(HandshakeError::ClosedBeforeHello),
            Ok(Ok(Some(first))) => first,
        };
        let Some(Payload::Hello(hello)) = first.payload else {
            return Err(HandshakeError::NotHello);
        };
        check_version(&hello.protocol_version)?;
        check_release(&hello.agent_version)?;
        tls::validate_id(&hello.agent_id).map_err(HandshakeError::BadAgentId)?;
        let not_allowed = || HandshakeError::NotAllowed {
            peer: peer.clone(),
            agent_id: hello.agent_id.clone(),
        };
        let (role, via_relay) = match (peer.role(), via) {
            // Forwarded by relays: the server checks the whole path.
            (PeerRole::Relay, Some(via)) => {
                let origin = via.origin_peer();
                if origin.id() != hello.agent_id {
                    return Err(not_allowed());
                }
                let relays: Vec<&str> = std::iter::once(peer.id())
                    .chain(via.relays().rev().map(PeerIdentity::id))
                    .collect();
                if !self.policy.may_route(&relays, origin).await {
                    return Err(not_allowed());
                }
                let attached = relays.last().map(|id| (*id).to_owned());
                (origin.role(), attached)
            }
            (PeerRole::Agent, _) if hello.agent_id != peer.id() => return Err(not_allowed()),
            // A relay's own Hello, or an agent's own direct connection.
            (role, _) if hello.agent_id == peer.id() => (role, None),
            // A relay that sends no path speaks for an agent attached to it.
            (PeerRole::Relay, None) => {
                if !self.policy.may_speak_for(peer.id(), &hello.agent_id).await {
                    return Err(not_allowed());
                }
                (PeerRole::Agent, Some(peer.id().to_owned()))
            }
            (PeerRole::Agent | PeerRole::Server, _) => unreachable!("handled above"),
        };
        let accepted = AcceptedHello {
            peer,
            hello: &hello,
            via_relay: via_relay.as_deref(),
            role,
        };
        if accepted.role() == PeerRole::Agent {
            check_agent_hello(&hello)?;
        }
        let site = self.inventory.hello(accepted).await?;
        let sensor_config = match (&self.sensor_configs, accepted.role()) {
            (Some(configs), PeerRole::Agent) => configs
                .load(&hello.agent_id)
                .await
                .map_err(HandshakeError::SensorConfig)?,
            _ => None,
        };
        Ok(Accepted {
            sequence: first.sequence,
            hello,
            role,
            via_relay,
            site,
            sensor_config,
        })
    }
}

/// What the handshake established.
struct Accepted {
    sequence: u64,
    hello: Hello,
    /// Role of the peer the Hello is about.
    role: PeerRole,
    /// The relay the peer is attached to, if any.
    via_relay: Option<String>,
    /// The peer's site, from the inventory.
    site: String,
    /// What to send right after registration.
    sensor_config: Option<Resolved>,
}

#[tonic::async_trait]
impl AgentService for AgentStreamService {
    type StreamStream = Outbound;

    async fn stream(
        &self,
        request: Request<Streaming<Envelope>>,
    ) -> Result<Response<Self::StreamStream>, Status> {
        let peer = tls::peer_identity(&request)?;
        let cert_serial = PresentedCert::from_request(&request).ok().map(|c| c.serial);
        // Only relays forward; an agent speaks for itself whatever it sends.
        let via = match peer.role() {
            PeerRole::Relay => Via::from_metadata(request.metadata())?,
            _ => None,
        };
        let mut inbound = request.into_inner();
        let accepted = match self.handshake(&peer, via, &mut inbound).await {
            Ok(accepted) => accepted,
            Err(error) => {
                match &error {
                    HandshakeError::NotAllowed { agent_id, .. } => tracing::warn!(
                        role = %peer.role(),
                        cert_id = peer.id(),
                        hello_agent_id = %agent_id,
                        "Hello agent_id not allowed for this certificate"
                    ),
                    HandshakeError::ServerRole
                    | HandshakeError::Inventory(
                        InventoryError::Revoked { .. } | InventoryError::NotEnrolled { .. },
                    ) => tracing::warn!(%peer, %error, "stream refused"),
                    HandshakeError::Inventory(InventoryError::Db(_))
                    | HandshakeError::SensorConfig(_) => {
                        tracing::error!(%peer, %error, "stream refused")
                    }
                    _ => tracing::info!(%peer, %error, "stream refused"),
                }
                return Err(error.into());
            }
        };

        let Accepted {
            sequence,
            hello,
            role,
            via_relay,
            site,
            sensor_config,
        } = accepted;
        let now = Utc::now();
        let info = PeerInfo {
            id: hello.agent_id,
            role,
            via_relay: via_relay.clone(),
            connection: 0,
            cert_serial,
            agent_version: hello.agent_version,
            hostname: hello.hostname,
            connected_at: now,
            last_seen: now,
            last_sequence: sequence,
            counters: Counters::default(),
            sensor_revision_sent: sensor_config.as_ref().map(|c| c.revision),
            sensor_revision_applied: None,
        };
        let (outbound_tx, outbound_rx) = mpsc::channel(self.settings.outbound_capacity);
        let id = info.id.clone();
        let registration = self
            .registry
            .register(info, outbound_tx.clone())
            .map_err(|e| Status::unavailable(e.to_string()))?;
        if let Some(config) = sensor_config {
            let envelope = self
                .registry
                .envelope(Payload::SensorConfig(config.to_proto()));
            // The queue is new and empty.
            let _ = outbound_tx.try_send(envelope);
        }
        drop(outbound_tx);
        tracing::info!(%peer, agent_id = %id, connection = registration.connection, "stream opened");
        if let (Some(commands), PeerRole::Agent) = (&self.commands, role) {
            commands.dispatch(&id);
        }
        if let (Some(commands), PeerRole::Relay) = (&self.relay_commands, role) {
            commands.dispatch(&id);
        }
        let event = StreamEvent {
            peer: id.clone(),
            site: site.clone(),
            role,
            via_relay: via_relay.unwrap_or_default(),
            connection: registration.connection,
        };
        self.ingest.event(event.record("stream opened", None));

        let (end_tx, end_rx) = oneshot::channel();
        let identity = Identity {
            peer: id.clone(),
            site,
        };
        let connection = Connection {
            registry: self.registry.clone(),
            id,
            role,
            number: registration.connection,
            last_sequence: sequence,
            counters: Counters::default(),
            inventory: self.inventory.clone(),
            // The Hello just wrote last_seen_at.
            last_recorded: Instant::now(),
            last_seen_interval: self.settings.last_seen_interval,
            identity,
            ingest: self.ingest.clone(),
            sensor_configs: self.sensor_configs.clone(),
            alerts: self.alerts.clone(),
            commands: self.commands.clone(),
            relay_commands: self.relay_commands.clone(),
            event,
            invalid_logged: None,
        };
        tokio::spawn(connection.run(
            inbound,
            registration.close,
            end_tx,
            self.settings.heartbeat_timeout,
        ));
        Ok(Response::new(Outbound {
            envelopes: outbound_rx,
            end: Some(end_rx),
        }))
    }
}

/// Response stream: envelopes queued through the registry, until the
/// connection task ends it, with an error status or cleanly.
pub struct Outbound {
    envelopes: mpsc::Receiver<Envelope>,
    end: Option<oneshot::Receiver<Status>>,
}

impl Stream for Outbound {
    type Item = Result<Envelope, Status>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let Some(end) = self.end.as_mut() else {
            return Poll::Ready(None);
        };
        if let Poll::Ready(status) = Pin::new(end).poll(cx) {
            self.end = None;
            return Poll::Ready(status.ok().map(Err));
        }
        self.envelopes.poll_recv(cx).map(|e| e.map(Ok))
    }
}

/// State of one accepted stream.
struct Connection {
    registry: Registry,
    id: String,
    role: PeerRole,
    number: u64,
    last_sequence: u64,
    counters: Counters,
    inventory: Arc<dyn Inventory>,
    last_recorded: Instant,
    last_seen_interval: Duration,
    /// The `peer` and `site` labels of this stream's series.
    identity: Identity,
    ingest: Ingest,
    sensor_configs: Option<SensorConfigs>,
    alerts: Option<Alerts>,
    commands: Option<Commands>,
    relay_commands: Option<RelayCommands>,
    event: StreamEvent,
    /// When invalid samples were last logged.
    invalid_logged: Option<Instant>,
}

/// Fields of the stream events recorded in VictoriaLogs. Identifiers only.
struct StreamEvent {
    peer: String,
    site: String,
    role: PeerRole,
    via_relay: String,
    connection: u64,
}

impl StreamEvent {
    fn record(&self, msg: &str, reason: Option<&str>) -> LogRecord {
        LogRecord::new(msg)
            .field(
                "event",
                if reason.is_some() {
                    "peer.disconnect"
                } else {
                    "peer.connect"
                },
            )
            .field("peer", &self.peer)
            .field("site", &self.site)
            .field("role", self.role.to_string())
            .field("relay", &self.via_relay)
            .field("connection", self.connection.to_string())
            .field("reason", reason.unwrap_or_default())
    }
}

impl Connection {
    async fn run(
        mut self,
        mut inbound: Streaming<Envelope>,
        mut close: oneshot::Receiver<CloseReason>,
        end: oneshot::Sender<Status>,
        heartbeat_timeout: Duration,
    ) {
        let idle = sleep(heartbeat_timeout);
        tokio::pin!(idle);
        let mut reason = "closed";
        let status = loop {
            tokio::select! {
                message = inbound.message() => match message {
                    Ok(Some(envelope)) => {
                        idle.as_mut().reset(Instant::now() + heartbeat_timeout);
                        self.receive(envelope);
                    }
                    Ok(None) => break None,
                    Err(status) => {
                        tracing::debug!(agent_id = %self.id, %status, "stream failed");
                        reason = "error";
                        break None;
                    }
                },
                close_reason = &mut close => break Some(match close_reason {
                    Ok(CloseReason::Replaced) => {
                        reason = "replaced";
                        Status::aborted("replaced by a newer connection with the same ID")
                    }
                    Ok(CloseReason::Revoked) => {
                        reason = "revoked";
                        Status::permission_denied("revoked by an administrator")
                    }
                    Ok(CloseReason::Shutdown) | Err(_) => {
                        reason = "shutdown";
                        Status::unavailable("server shutting down")
                    }
                }),
                () = &mut idle => {
                    tracing::info!(agent_id = %self.id, timeout = ?heartbeat_timeout, "heartbeat timeout");
                    reason = "heartbeat_timeout";
                    break Some(Status::deadline_exceeded(format!(
                        "no message for {heartbeat_timeout:?}"
                    )));
                }
            }
        };
        // Remove first, so a peer that sees the stream end is already gone.
        self.registry.remove(&self.id, self.number);
        self.ingest
            .event(self.event.record("stream closed", Some(reason)));
        if let Some(status) = status {
            let _ = end.send(status);
        }
        tracing::info!(agent_id = %self.id, connection = self.number, "stream closed");
    }

    fn receive(&mut self, envelope: Envelope) {
        let now = Utc::now();
        if envelope.sequence <= self.last_sequence {
            self.counters.stale_sequences += 1;
            tracing::debug!(
                agent_id = %self.id,
                sequence = envelope.sequence,
                last = self.last_sequence,
                "ignoring envelope with stale sequence"
            );
        } else {
            self.last_sequence = envelope.sequence;
            self.handle(envelope.sequence, envelope.payload);
            self.record_seen();
        }
        let (sequence, counters) = (self.last_sequence, self.counters);
        self.registry.update(&self.id, self.number, |info| {
            info.last_seen = now;
            info.last_sequence = sequence;
            info.counters = counters;
        });
    }

    /// Writes `last_seen_at` at most once per `last_seen_interval`. Any
    /// accepted envelope counts, not only heartbeats. The write runs in its
    /// own task so a slow database never stalls the stream; a failure is
    /// logged and the next interval tries again.
    fn record_seen(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.last_recorded) < self.last_seen_interval {
            return;
        }
        self.last_recorded = now;
        let (inventory, role, id) = (self.inventory.clone(), self.role, self.id.clone());
        tokio::spawn(async move {
            if let Err(error) = inventory.seen(role, &id).await {
                tracing::warn!(agent_id = %id, %role, %error, "could not record last_seen_at");
            }
        });
    }

    /// Validates a batch and queues it for VictoriaMetrics. Invalid
    /// samples are dropped and counted; the stream stays open (ADR-0013).
    fn samples(&mut self, batch: SampleBatch) {
        self.counters.sample_batches += 1;
        self.counters.samples += batch.samples.len() as u64;
        let checked = ingest::check_batch(&self.identity, batch, Utc::now().timestamp_millis());
        let (invalid, first_error) = (checked.invalid, checked.first_error);
        self.counters.invalid_samples += invalid;
        // Never waits: a full engine queue drops the batch and counts it.
        if let Some(alerts) = &self.alerts {
            alerts.submit(&checked.series);
        }
        self.counters.dropped_samples += self.ingest.samples(checked);
        let Some(error) = first_error else { return };
        let now = Instant::now();
        if self
            .invalid_logged
            .is_some_and(|at| now.duration_since(at) < INVALID_SAMPLE_LOG_INTERVAL)
        {
            return;
        }
        self.invalid_logged = Some(now);
        tracing::warn!(
            agent_id = %self.id,
            invalid,
            total_invalid = self.counters.invalid_samples,
            %error,
            "dropped invalid samples"
        );
    }

    /// Records the revision an agent applied. Refused specs are logged:
    /// the operator fixes them in the configuration.
    fn sensor_config_ack(&mut self, sequence: u64, ack: SensorConfigAck) {
        let id = &self.id;
        if self.role != PeerRole::Agent {
            tracing::debug!(agent_id = %id, sequence, "ignoring sensor configuration ack from a relay");
            return;
        }
        if let Some(first) = ack.errors.first() {
            let message: String = first
                .message
                .escape_debug()
                .take(MAX_LOGGED_ERROR)
                .collect();
            tracing::warn!(
                agent_id = %id,
                revision = ack.revision,
                refused = ack.errors.len(),
                sensor = %first.name.escape_debug(),
                instance = %first.instance.escape_debug(),
                error = %message,
                "agent refused sensors of its configuration"
            );
        } else {
            tracing::debug!(agent_id = %id, revision = ack.revision, "sensor configuration applied");
        }
        let revision = ack.revision;
        self.registry.update(id, self.number, |info| {
            info.sensor_revision_applied = Some(revision);
        });
        if let Some(configs) = &self.sensor_configs {
            configs.applied(id, revision);
        }
    }

    fn handle(&mut self, sequence: u64, payload: Option<Payload>) {
        let id = &self.id;
        match payload {
            Some(Payload::Heartbeat(_)) => tracing::trace!(agent_id = %id, sequence, "heartbeat"),
            Some(Payload::Samples(batch)) => {
                tracing::debug!(agent_id = %id, sequence, samples = batch.samples.len(), "sample batch");
                self.samples(batch);
            }
            Some(Payload::SensorConfigAck(ack)) => self.sensor_config_ack(sequence, ack),
            Some(Payload::SensorConfig(_)) => {
                tracing::warn!(agent_id = %id, sequence, "ignoring SensorConfig sent upstream")
            }
            Some(Payload::CommandResult(result)) => {
                self.counters.command_results += 1;
                tracing::debug!(agent_id = %id, sequence, command_id = %result.command_id.escape_debug(), "command result");
                // A relay's own results are of its self-updates (ADR-0055);
                // its agents' results come on their own streams.
                if let (Some(commands), PeerRole::Agent) = (&self.commands, self.role) {
                    let (commands, id) = (commands.clone(), id.clone());
                    tokio::spawn(async move { commands.record(&id, result).await });
                } else if let (Some(commands), PeerRole::Relay) = (&self.relay_commands, self.role)
                {
                    let (commands, id) = (commands.clone(), id.clone());
                    tokio::spawn(async move { commands.record(&id, result).await });
                }
            }
            Some(Payload::PackageReport(report)) => {
                tracing::debug!(agent_id = %id, sequence, packages = report.packages.len(), "package report");
                if let (Some(commands), PeerRole::Agent) = (&self.commands, self.role) {
                    let (commands, id) = (commands.clone(), id.clone());
                    tokio::spawn(async move { commands.report(&id, report).await });
                }
            }
            Some(Payload::RelayMirrors(report)) => {
                tracing::debug!(agent_id = %id, sequence, mirrors = report.mirrors.len(), "relay mirror report");
                // Only a relay speaks for its own mirrors.
                if self.role == PeerRole::Relay {
                    let (inventory, id) = (self.inventory.clone(), id.clone());
                    let received_at = Utc::now();
                    tokio::spawn(async move {
                        if let Err(error) = inventory.relay_mirrors(&id, received_at, report).await
                        {
                            tracing::warn!(relay_id = %id, %error, "could not record the relay's mirrors");
                        }
                    });
                } else {
                    tracing::warn!(agent_id = %id, sequence, "ignoring RelayMirrors from an agent");
                }
            }
            // TODO(relay-mux): on a relay connection, a later Hello opens a
            // sub-session for another agent behind the relay. It must pass
            // the RelayPolicy check, get its own registry entry and sequence
            // tracking, and envelopes need an agent_id to be routed.
            Some(Payload::Hello(_)) => {
                tracing::debug!(agent_id = %id, sequence, "ignoring repeated Hello")
            }
            Some(Payload::Command(_)) => {
                tracing::warn!(agent_id = %id, sequence, "ignoring Command sent upstream")
            }
            None => tracing::debug!(agent_id = %id, sequence, "ignoring envelope without payload"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_major_is_accepted_other_majors_are_not() {
        assert!(check_version("1.0").is_ok());
        assert!(check_version("1.7").is_ok());
        let err = check_version("2.0").unwrap_err().to_string();
        assert!(
            err.contains("2.0") && err.contains(PROTOCOL_VERSION),
            "{err}"
        );
        for bad in ["", "1", "x.1", "1.x", "1.0.0"] {
            assert!(
                matches!(check_version(bad), Err(HandshakeError::BadVersion(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn releases_older_than_the_minimum_are_refused() {
        let minimum: Version = MIN_PEER_VERSION.parse().unwrap();
        assert!(check_release(MIN_PEER_VERSION).is_ok());
        assert!(check_release("99.0.0").is_ok());
        assert!(check_release("").is_ok(), "no version: a development build");
        assert!(check_release("dev").is_ok());
        if minimum > "0.0.0".parse().unwrap() {
            let error = check_release("0.0.0").unwrap_err();
            assert!(matches!(error, HandshakeError::TooOld { .. }));
            assert_eq!(Status::from(error).code(), tonic::Code::FailedPrecondition);
        }
    }

    #[test]
    fn handshake_errors_map_to_grpc_codes() {
        use tonic::Code;
        let code = |e: HandshakeError| Status::from(e).code();
        assert_eq!(code(HandshakeError::ServerRole), Code::PermissionDenied);
        assert_eq!(
            code(HandshakeError::HelloTimeout(Duration::ZERO)),
            Code::DeadlineExceeded
        );
        assert_eq!(code(HandshakeError::NotHello), Code::InvalidArgument);
        let mismatch = HandshakeError::VersionMismatch {
            peer: "2.0".into(),
            server: PROTOCOL_VERSION,
        };
        assert_eq!(code(mismatch), Code::FailedPrecondition);
        let revoked = InventoryError::Revoked {
            role: PeerRole::Agent,
            id: "web-01".into(),
        };
        assert_eq!(code(revoked.into()), Code::PermissionDenied);
        assert_eq!(
            code(InventoryError::AwaitingApproval.into()),
            Code::Unavailable
        );
        let status = Status::from(HandshakeError::from(InventoryError::Db(
            sqlx::Error::PoolClosed,
        )));
        assert_eq!(status.code(), Code::Unavailable);
        assert!(!status.message().contains("pool"), "{status}");
        let status = Status::from(HandshakeError::SensorConfig(sqlx::Error::PoolClosed));
        assert_eq!(status.code(), Code::Unavailable);
        assert!(!status.message().contains("pool"), "{status}");
    }

    fn agent_hello(os_family: &str, hostname: &str) -> Hello {
        Hello {
            agent_id: "web-01".into(),
            hostname: hostname.into(),
            os_family: os_family.into(),
            ..Hello::default()
        }
    }

    #[test]
    fn agent_hello_needs_a_known_os_family_and_a_hostname() {
        assert!(check_agent_hello(&agent_hello("debian", "web-01")).is_ok());
        assert!(check_agent_hello(&agent_hello("rhel", "web-01")).is_ok());
        assert!(check_agent_hello(&agent_hello("windows", "WIN-01")).is_ok());
        for (os_family, hostname) in [
            ("", "web-01"),
            ("Debian", "web-01"),
            ("Windows", "web-01"),
            ("debian", ""),
            ("debian", &"a".repeat(MAX_HOSTNAME + 1)),
        ] {
            let error = check_agent_hello(&agent_hello(os_family, hostname)).unwrap_err();
            assert_eq!(Status::from(error).code(), tonic::Code::InvalidArgument);
        }
        let long = Hello {
            os_version: "x".repeat(MAX_VERSION + 1),
            ..agent_hello("debian", "web-01")
        };
        assert!(check_agent_hello(&long).is_err());
    }
}
