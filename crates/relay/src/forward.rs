//! The relay's `AgentService`: checks an agent's certificate and `Hello`,
//! then pipes its stream to a stream of its own on the upstream, envelopes
//! unchanged both ways (ADR-0025).
//!
//! The TLS listener already checked the certificate against the CA and the
//! CRL. Each agent stream gets its own upstream connection, authenticated
//! with the relay's certificate, so the server applies its relay policy and
//! its per-agent stream logic as for any relayed agent.
//!
//! Envelope payloads are never logged.

// Every check here ends as the `Status` tonic returns by value.
#![allow(clippy::result_large_err)]

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use monitorflow_common::client::identity::StoredIdentity;
use monitorflow_common::config::RelayConfig;
use monitorflow_common::proto::v1::agent_service_client::AgentServiceClient;
use monitorflow_common::proto::v1::agent_service_server::AgentService;
use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::Envelope;
use monitorflow_common::tls::{self, error_chain, PeerIdentity, PeerRole};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::timeout;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream, StreamExt};
use tonic::transport::Channel;
use tonic::{Code, Request, Response, Status, Streaming};

/// What agents get when the relay cannot reach its upstream. Details stay
/// in the relay's log: they may name TLS alerts about the relay's own
/// certificate, which an agent would read as being about its own.
const UPSTREAM_UNAVAILABLE: &str = "upstream unavailable; retry later";
/// HTTP/2 pings on upstream connections detect a dead server.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone)]
pub struct ForwardSettings {
    pub upstream_url: String,
    /// Time allowed for the agent's first envelope, which must be `Hello`.
    pub hello_timeout: Duration,
    pub connect_timeout: Duration,
    /// Time allowed for the upstream to accept the `Hello`. Below the
    /// agent's own 30 s, so the agent gets our answer.
    pub open_timeout: Duration,
    /// Envelopes queued towards the upstream per stream; a full queue stops
    /// reading from the agent.
    pub queue: usize,
}

impl ForwardSettings {
    pub fn from_config(config: &RelayConfig) -> Self {
        Self {
            upstream_url: config.upstream_url.clone(),
            hello_timeout: Duration::from_secs(10),
            connect_timeout: tls::DEFAULT_TIMEOUT,
            open_timeout: Duration::from_secs(15),
            queue: 16,
        }
    }
}

/// The relay's `AgentService`. Cheap to clone.
#[derive(Clone)]
pub struct Forwarder {
    settings: Arc<ForwardSettings>,
    identity: watch::Receiver<StoredIdentity>,
    upstream_up: watch::Receiver<bool>,
}

impl Forwarder {
    /// `identity` is the relay's own, followed through renewals; agent
    /// streams are refused without dialing while `upstream_up` is false.
    pub fn new(
        settings: ForwardSettings,
        identity: watch::Receiver<StoredIdentity>,
        upstream_up: watch::Receiver<bool>,
    ) -> Self {
        Self {
            settings: Arc::new(settings),
            identity,
            upstream_up,
        }
    }

    async fn connect(&self) -> Result<Channel, String> {
        let url = &self.settings.upstream_url;
        let config = self
            .identity
            .borrow()
            .tls
            .client_config()
            .map_err(|e| e.to_string())?;
        let connect_timeout = self.settings.connect_timeout;
        tls::endpoint(url)
            .map_err(|e| e.to_string())?
            .connect_timeout(connect_timeout)
            .http2_keep_alive_interval(KEEPALIVE_INTERVAL)
            .keep_alive_timeout(KEEPALIVE_TIMEOUT)
            .keep_alive_while_idle(true)
            .connect_with_connector(tls::connector(config, connect_timeout))
            .await
            .map_err(|e| format!("cannot connect to {url}: {}", error_chain(&e)))
    }
}

/// Only agents may open a stream through the relay, as themselves.
pub fn check_peer(peer: &PeerIdentity) -> Result<(), Status> {
    match peer.role() {
        PeerRole::Agent => Ok(()),
        PeerRole::Relay => Err(Status::permission_denied(
            "relay chaining is not supported yet",
        )),
        PeerRole::Server => Err(Status::permission_denied(
            "role server may not open an agent stream",
        )),
    }
}

/// The `Hello` must name the certificate's agent; nothing else in it is
/// the relay's business.
pub fn check_hello(peer: &PeerIdentity, first: &Envelope) -> Result<(), Status> {
    match &first.payload {
        Some(Payload::Hello(hello)) if hello.agent_id == peer.id() => Ok(()),
        Some(Payload::Hello(hello)) => Err(Status::permission_denied(format!(
            "{peer} may not speak for agent {}",
            hello.agent_id.escape_debug()
        ))),
        _ => Err(Status::invalid_argument("the first envelope must be Hello")),
    }
}

/// The status an agent gets for one from the upstream link. Statuses the
/// upstream sent keep their code and message, so the agent applies its
/// reconnect policy (ADR-0009); errors of the link itself carry a source
/// and become a bare `UNAVAILABLE`.
pub fn downstream_status(agent_id: &str, status: Status) -> Status {
    if std::error::Error::source(&status).is_some() || status.code() == Code::Unknown {
        tracing::warn!(agent_id, error = %error_chain(&status), "upstream stream failed");
        return Status::unavailable(UPSTREAM_UNAVAILABLE);
    }
    Status::new(status.code(), status.message())
}

async fn first_envelope(
    inbound: &mut Streaming<Envelope>,
    hello_timeout: Duration,
) -> Result<Envelope, Status> {
    match timeout(hello_timeout, inbound.message()).await {
        Err(_) => Err(Status::deadline_exceeded(format!(
            "no Hello within {hello_timeout:?}"
        ))),
        Ok(Err(status)) => Err(status),
        Ok(Ok(None)) => Err(Status::invalid_argument("stream closed before Hello")),
        Ok(Ok(Some(first))) => Ok(first),
    }
}

/// Envelopes from the agent to the upstream, until either side ends.
/// Dropping `upstream` ends the upstream stream, so the server sees the
/// agent leave.
async fn pump_up(
    agent_id: String,
    mut inbound: Streaming<Envelope>,
    upstream: mpsc::Sender<Envelope>,
    mut done: oneshot::Receiver<()>,
) {
    let mut forwarded = 0u64;
    let reason = loop {
        tokio::select! {
            message = inbound.message() => match message {
                Ok(Some(envelope)) => {
                    tokio::select! {
                        sent = upstream.send(envelope) => if sent.is_err() {
                            break "upstream closed";
                        },
                        _ = &mut done => break "downstream closed",
                    }
                    forwarded += 1;
                }
                Ok(None) => break "agent closed",
                Err(status) => {
                    tracing::debug!(%agent_id, %status, "agent stream failed");
                    break "agent stream failed";
                }
            },
            _ = &mut done => break "downstream closed",
        }
    };
    tracing::info!(%agent_id, forwarded, reason, "stream closed");
}

type Downstream = Pin<Box<dyn Stream<Item = Result<Envelope, Status>> + Send>>;

#[tonic::async_trait]
impl AgentService for Forwarder {
    type StreamStream = Downstream;

    async fn stream(
        &self,
        request: Request<Streaming<Envelope>>,
    ) -> Result<Response<Downstream>, Status> {
        let peer = tls::peer_identity(&request)?;
        check_peer(&peer).inspect_err(|status| {
            tracing::warn!(%peer, %status, "stream refused");
        })?;
        let mut inbound = request.into_inner();
        let hello = first_envelope(&mut inbound, self.settings.hello_timeout).await?;
        check_hello(&peer, &hello).inspect_err(|status| {
            tracing::warn!(%peer, %status, "stream refused");
        })?;
        let agent_id = peer.id().to_owned();
        if !*self.upstream_up.borrow() {
            tracing::info!(%agent_id, "stream refused: the upstream is not connected");
            return Err(Status::unavailable(UPSTREAM_UNAVAILABLE));
        }
        let channel = self.connect().await.map_err(|error| {
            tracing::warn!(%agent_id, %error, "cannot reach the upstream");
            Status::unavailable(UPSTREAM_UNAVAILABLE)
        })?;

        let (up_tx, up_rx) = mpsc::channel(self.settings.queue.max(1));
        up_tx
            .try_send(hello)
            .expect("a new channel has room for Hello");
        let mut client = AgentServiceClient::new(channel);
        let open = timeout(
            self.settings.open_timeout,
            client.stream(ReceiverStream::new(up_rx)),
        );
        let upstream = match open.await {
            Err(_) => {
                tracing::warn!(%agent_id, "the upstream did not answer Hello in time");
                return Err(Status::unavailable(UPSTREAM_UNAVAILABLE));
            }
            Ok(Err(status)) => {
                let status = downstream_status(&agent_id, status);
                tracing::info!(%agent_id, %status, "the upstream refused the agent");
                return Err(status);
            }
            Ok(Ok(response)) => response.into_inner(),
        };
        tracing::info!(%agent_id, "stream opened");

        // Dropped with the response stream, which tonic drops when the
        // agent goes away: that stops the upward pump.
        let (done_tx, done_rx) = oneshot::channel();
        tokio::spawn(pump_up(agent_id.clone(), inbound, up_tx, done_rx));
        let down = upstream.map(move |item| {
            let _alive = &done_tx;
            item.map_err(|status| downstream_status(&agent_id, status))
        });
        Ok(Response::new(Box::pin(down)))
    }
}

#[cfg(test)]
mod tests {
    use monitorflow_common::proto::v1::{Heartbeat, Hello};

    use super::*;

    fn agent(id: &str) -> PeerIdentity {
        PeerIdentity::new(PeerRole::Agent, id).unwrap()
    }

    fn hello(agent_id: &str) -> Envelope {
        Envelope {
            sequence: 1,
            payload: Some(Payload::Hello(Hello {
                agent_id: agent_id.into(),
                ..Hello::default()
            })),
        }
    }

    #[test]
    fn only_agents_speaking_for_themselves_pass() {
        assert!(check_peer(&agent("web-01")).is_ok());
        for role in [PeerRole::Relay, PeerRole::Server] {
            let peer = PeerIdentity::new(role, "x").unwrap();
            assert_eq!(
                check_peer(&peer).unwrap_err().code(),
                Code::PermissionDenied
            );
        }
        assert!(check_hello(&agent("web-01"), &hello("web-01")).is_ok());
        let other = check_hello(&agent("web-01"), &hello("web-02")).unwrap_err();
        assert_eq!(other.code(), Code::PermissionDenied);
        assert!(other.message().contains("web-02"), "{other}");
        let heartbeat = Envelope {
            sequence: 1,
            payload: Some(Payload::Heartbeat(Heartbeat::default())),
        };
        let status = check_hello(&agent("web-01"), &heartbeat).unwrap_err();
        assert_eq!(status.code(), Code::InvalidArgument);
    }

    #[test]
    fn upstream_statuses_pass_but_link_errors_do_not() {
        for status in [
            Status::permission_denied("relay:r1 may not speak for agent web-02"),
            Status::aborted("replaced by a newer connection with the same ID"),
            Status::deadline_exceeded("no message for 90s"),
            Status::unavailable("server shutting down"),
        ] {
            let down = downstream_status("web-01", status.clone());
            assert_eq!(
                (down.code(), down.message()),
                (status.code(), status.message())
            );
        }
        // A local transport error naming a TLS alert about the relay's
        // certificate must not reach the agent, which would take it as its
        // own.
        let alert = std::io::Error::other("received fatal alert: CertificateRevoked");
        let local = Status::from_error(Box::new(alert));
        let down = downstream_status("web-01", local);
        assert_eq!(down.code(), Code::Unavailable);
        assert_eq!(down.message(), UPSTREAM_UNAVAILABLE);
        let unknown = downstream_status("web-01", Status::unknown("h2 protocol error"));
        assert_eq!(unknown.message(), UPSTREAM_UNAVAILABLE);
    }
}
