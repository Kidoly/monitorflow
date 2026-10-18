//! `AgentService::Stream`: identity checks, `Hello`, then the per-connection
//! loop that tracks liveness and sequence numbers.
//!
//! Envelope payloads are never logged: command payloads and results will
//! carry sensitive data.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use chrono::Utc;
use monitorflow_common::proto::v1::agent_service_server::AgentService;
use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::{Envelope, Hello};
use monitorflow_common::tls::{self, PeerIdentity, PeerRole};
use monitorflow_common::PROTOCOL_VERSION;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{sleep, timeout, Instant};
use tokio_stream::Stream;
use tonic::{Request, Response, Status, Streaming};

use super::policy::RelayPolicy;
use super::registry::{CloseReason, Counters, PeerInfo, Registry};
use super::StreamSettings;

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
    #[error("invalid agent_id: {0}")]
    BadAgentId(String),
    #[error("{peer} may not speak for agent {agent_id}")]
    NotAllowed {
        peer: PeerIdentity,
        agent_id: String,
    },
}

impl From<HandshakeError> for Status {
    fn from(error: HandshakeError) -> Self {
        let message = error.to_string();
        match error {
            HandshakeError::ServerRole | HandshakeError::NotAllowed { .. } => {
                Status::permission_denied(message)
            }
            HandshakeError::HelloTimeout(_) => Status::deadline_exceeded(message),
            HandshakeError::VersionMismatch { .. } => Status::failed_precondition(message),
            HandshakeError::Transport(status) => *status,
            HandshakeError::ClosedBeforeHello
            | HandshakeError::NotHello
            | HandshakeError::BadVersion(_)
            | HandshakeError::BadAgentId(_) => Status::invalid_argument(message),
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

/// The `AgentService` implementation of the central server.
#[derive(Clone)]
pub struct AgentStreamService {
    registry: Registry,
    policy: Arc<dyn RelayPolicy>,
    settings: StreamSettings,
}

impl AgentStreamService {
    pub fn new(registry: Registry, policy: Arc<dyn RelayPolicy>, settings: StreamSettings) -> Self {
        Self {
            registry,
            policy,
            settings,
        }
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Waits for `Hello` and checks it against the peer's certificate.
    /// Returns its sequence, the Hello, and the relay speaking for the agent.
    async fn handshake(
        &self,
        peer: &PeerIdentity,
        inbound: &mut Streaming<Envelope>,
    ) -> Result<(u64, Hello, Option<String>), HandshakeError> {
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
        tls::validate_id(&hello.agent_id).map_err(HandshakeError::BadAgentId)?;
        let not_allowed = || HandshakeError::NotAllowed {
            peer: peer.clone(),
            agent_id: hello.agent_id.clone(),
        };
        let via_relay = match peer.role() {
            PeerRole::Agent if hello.agent_id != peer.id() => return Err(not_allowed()),
            // A relay's own Hello, or an agent's own direct connection.
            _ if hello.agent_id == peer.id() => None,
            PeerRole::Relay => {
                if !self.policy.may_speak_for(peer.id(), &hello.agent_id).await {
                    return Err(not_allowed());
                }
                Some(peer.id().to_owned())
            }
            PeerRole::Agent | PeerRole::Server => unreachable!("handled above"),
        };
        Ok((first.sequence, hello, via_relay))
    }
}

#[tonic::async_trait]
impl AgentService for AgentStreamService {
    type StreamStream = Outbound;

    async fn stream(
        &self,
        request: Request<Streaming<Envelope>>,
    ) -> Result<Response<Self::StreamStream>, Status> {
        let peer = tls::peer_identity(&request)?;
        let mut inbound = request.into_inner();
        let (sequence, hello, via_relay) = match self.handshake(&peer, &mut inbound).await {
            Ok(accepted) => accepted,
            Err(error) => {
                match &error {
                    HandshakeError::NotAllowed { agent_id, .. } => tracing::warn!(
                        role = %peer.role(),
                        cert_id = peer.id(),
                        hello_agent_id = %agent_id,
                        "Hello agent_id not allowed for this certificate"
                    ),
                    HandshakeError::ServerRole => tracing::warn!(%peer, %error, "stream refused"),
                    _ => tracing::info!(%peer, %error, "stream refused"),
                }
                return Err(error.into());
            }
        };

        let now = Utc::now();
        let info = PeerInfo {
            id: hello.agent_id,
            role: if via_relay.is_some() {
                PeerRole::Agent
            } else {
                peer.role()
            },
            via_relay,
            connection: 0,
            agent_version: hello.agent_version,
            hostname: hello.hostname,
            connected_at: now,
            last_seen: now,
            last_sequence: sequence,
            counters: Counters::default(),
        };
        let (outbound_tx, outbound_rx) = mpsc::channel(self.settings.outbound_capacity);
        let id = info.id.clone();
        let registration = self
            .registry
            .register(info, outbound_tx)
            .map_err(|e| Status::unavailable(e.to_string()))?;
        tracing::info!(%peer, agent_id = %id, connection = registration.connection, "stream opened");

        let (end_tx, end_rx) = oneshot::channel();
        let connection = Connection {
            registry: self.registry.clone(),
            id,
            number: registration.connection,
            last_sequence: sequence,
            counters: Counters::default(),
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
    number: u64,
    last_sequence: u64,
    counters: Counters,
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
                        break None;
                    }
                },
                reason = &mut close => break Some(match reason {
                    Ok(CloseReason::Replaced) => {
                        Status::aborted("replaced by a newer connection with the same ID")
                    }
                    Ok(CloseReason::Shutdown) | Err(_) => Status::unavailable("server shutting down"),
                }),
                () = &mut idle => {
                    tracing::info!(agent_id = %self.id, timeout = ?heartbeat_timeout, "heartbeat timeout");
                    break Some(Status::deadline_exceeded(format!(
                        "no message for {heartbeat_timeout:?}"
                    )));
                }
            }
        };
        // Remove first, so a peer that sees the stream end is already gone.
        self.registry.remove(&self.id, self.number);
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
        }
        let (sequence, counters) = (self.last_sequence, self.counters);
        self.registry.update(&self.id, self.number, |info| {
            info.last_seen = now;
            info.last_sequence = sequence;
            info.counters = counters;
        });
    }

    fn handle(&mut self, sequence: u64, payload: Option<Payload>) {
        let id = &self.id;
        match payload {
            Some(Payload::Heartbeat(_)) => tracing::trace!(agent_id = %id, sequence, "heartbeat"),
            // TODO(phase2-ingest): store samples in VictoriaMetrics.
            Some(Payload::Samples(batch)) => {
                self.counters.sample_batches += 1;
                self.counters.samples += batch.samples.len() as u64;
                tracing::debug!(agent_id = %id, sequence, samples = batch.samples.len(), "sample batch");
            }
            // TODO(phase2-ingest): record command results and audit them.
            Some(Payload::CommandResult(result)) => {
                self.counters.command_results += 1;
                tracing::debug!(agent_id = %id, sequence, command_id = %result.command_id, "command result");
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
    }
}
