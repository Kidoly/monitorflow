//! The relay's `AgentService`: checks an agent's certificate and `Hello`,
//! then hands its stream to a [`Session`] that forwards it to a stream of
//! its own on the upstream, payloads unchanged both ways (ADR-0025), or
//! buffers its batches on disk while there is none (ADR-0027).
//!
//! The TLS listener already checked the certificate against the CA and the
//! CRL. Each agent stream gets its own upstream connection, authenticated
//! with the relay's certificate and carrying the stream's `monitorflow-via`
//! path (ADR-0028), so the server applies its relay policy and its
//! per-agent stream logic as for any relayed agent. The relay numbers the
//! envelopes of the upstream streams it opens, so buffered batches fit
//! between the `Hello` and live data.
//!
//! Envelope payloads are never logged.

// Every check here ends as the `Status` tonic returns by value.
#![allow(clippy::result_large_err)]

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use monitorflow_common::client::backoff::Backoff;
use monitorflow_common::client::identity::StoredIdentity;
use monitorflow_common::client::shutdown::Shutdown;
use monitorflow_common::config::RelayConfig;
use monitorflow_common::proto::v1::agent_service_client::AgentServiceClient;
use monitorflow_common::proto::v1::agent_service_server::AgentService;
use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::Envelope;
use monitorflow_common::tls::{self, error_chain, PeerIdentity, PeerRole};
use monitorflow_common::via::Via;
use tokio::sync::{mpsc, watch};
use tokio::time::{interval, timeout, Instant, MissedTickBehavior};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::Stream;
use tonic::transport::Channel;
use tonic::{Code, Request, Response, Status, Streaming};

use crate::buffer::Buffer;
use crate::session::{self, AgentSide, Down, Ended, Lanes, Session, Upstream};

/// What agents get when the relay cannot reach its upstream. Details stay
/// in the relay's log: they may name TLS alerts about the relay's own
/// certificate, which an agent would read as being about its own.
pub const UPSTREAM_UNAVAILABLE: &str = "upstream unavailable; retry later";
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
    /// Retries of an agent's upstream stream after its link failed.
    pub backoff: Backoff,
    /// Buffered envelopes read from disk at a time during a replay.
    pub replay_batch: usize,
    /// How often agents with buffered records and no session are replayed.
    pub replay_scan: Duration,
}

impl ForwardSettings {
    pub fn from_config(config: &RelayConfig) -> Self {
        Self {
            upstream_url: config.upstream_url.clone(),
            hello_timeout: Duration::from_secs(10),
            connect_timeout: tls::DEFAULT_TIMEOUT,
            open_timeout: Duration::from_secs(15),
            queue: 16,
            backoff: Backoff::default(),
            replay_batch: 64,
            replay_scan: Duration::from_secs(10),
        }
    }
}

/// The relay's `AgentService`. Cheap to clone.
#[derive(Clone)]
pub struct Forwarder {
    settings: Arc<ForwardSettings>,
    identity: watch::Receiver<StoredIdentity>,
    upstream_up: watch::Receiver<bool>,
    buffer: Buffer,
    lanes: Arc<Lanes>,
}

impl Forwarder {
    /// `identity` is the relay's own, followed through renewals. While
    /// `upstream_up` is false, which [`crate::control`] sets while the
    /// relay's own stream is down, agents are accepted without dialing and
    /// their batches go to `buffer`.
    pub fn new(
        settings: ForwardSettings,
        identity: watch::Receiver<StoredIdentity>,
        upstream_up: watch::Receiver<bool>,
        buffer: Buffer,
    ) -> Self {
        Self {
            settings: Arc::new(settings),
            identity,
            upstream_up,
            buffer,
            lanes: Arc::default(),
        }
    }

    pub(crate) fn settings(&self) -> &Arc<ForwardSettings> {
        &self.settings
    }

    pub(crate) fn buffer(&self) -> &Buffer {
        &self.buffer
    }

    pub(crate) fn upstream_up(&self) -> watch::Receiver<bool> {
        self.upstream_up.clone()
    }

    /// Dials the upstream and opens a stream with `hello`, numbered 1, and
    /// the stream's path. What comes down goes to `down`.
    pub(crate) async fn open(
        &self,
        hello: &Envelope,
        via: &Via,
        down: Option<Down>,
    ) -> Result<Upstream, Ended> {
        let agent_id = via.origin_peer().id().to_owned();
        let channel = self.connect().await.map_err(|error| {
            tracing::warn!(%agent_id, %error, "cannot reach the upstream");
            Ended::Link
        })?;
        let (up_tx, up_rx) = mpsc::channel(self.settings.queue.max(1));
        up_tx
            .try_send(Envelope {
                sequence: 1,
                ..hello.clone()
            })
            .expect("a new channel has room for Hello");
        let mut client = AgentServiceClient::new(channel);
        let mut up_request = Request::new(ReceiverStream::new(up_rx));
        via.insert_into(up_request.metadata_mut());
        let open = timeout(self.settings.open_timeout, client.stream(up_request));
        match open.await {
            Err(_) => {
                tracing::warn!(%agent_id, "the upstream did not answer Hello in time");
                Err(Ended::Link)
            }
            Ok(Err(status)) => {
                let ended = session::classify(&agent_id, status);
                if let Ended::Status(status) = &ended {
                    tracing::info!(%agent_id, %status, "the upstream refused the agent");
                }
                Err(ended)
            }
            Ok(Ok(response)) => Ok(Upstream::start(
                agent_id,
                up_tx,
                response.into_inner(),
                down,
            )),
        }
    }

    /// Replays the records of agents without a session, every
    /// `replay_scan` and when the relay's own stream opens, until
    /// `shutdown`.
    pub async fn run_replays(self, mut shutdown: Shutdown) {
        let mut up = self.upstream_up.clone();
        let mut tick = interval(self.settings.replay_scan);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = shutdown.wait() => return,
                _ = tick.tick() => {}
                changed = up.changed() => if changed.is_err() { return },
            }
            if !*up.borrow_and_update() {
                continue;
            }
            let pending = match self.buffer.pending().await {
                Ok(pending) => pending,
                Err(error) => {
                    tracing::error!(%error, "cannot list buffered agents");
                    continue;
                }
            };
            for agent_id in pending {
                let Some(held) = self.lanes.try_take(&agent_id) else {
                    continue;
                };
                let (hello, via) = match self.buffer.hello(&agent_id).await {
                    Ok(Some((hello, via))) if names_origin(&hello, &via, &agent_id) => (hello, via),
                    Ok(_) => {
                        tracing::warn!(%agent_id, "buffered batches without a valid Hello and path; dropping them");
                        let _ = self.buffer.discard(&agent_id).await;
                        continue;
                    }
                    Err(error) => {
                        tracing::error!(%agent_id, %error, "cannot read the stored Hello");
                        continue;
                    }
                };
                tracing::info!(%agent_id, %via, "replaying buffered batches");
                let replay = Session {
                    forwarder: self.clone(),
                    agent_id,
                    hello,
                    via,
                    held,
                    sequence: 1,
                };
                tokio::spawn(replay.run(None, None, Instant::now()));
            }
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

/// Agents open streams through the relay, and so do downstream relays,
/// for themselves or for what is below them (ADR-0028).
pub fn check_peer(peer: &PeerIdentity) -> Result<(), Status> {
    match peer.role() {
        PeerRole::Agent | PeerRole::Relay => Ok(()),
        PeerRole::Server => Err(Status::permission_denied(
            "role server may not open an agent stream",
        )),
    }
}

/// The `Hello` must name the origin of the stream: the certificate's peer,
/// or the origin of a downstream relay's path, which the upstream checks
/// against its relays' parents. Nothing else in it is the relay's
/// business.
pub fn check_hello(peer: &PeerIdentity, via: &Via, first: &Envelope) -> Result<(), Status> {
    let origin = match peer.role() {
        PeerRole::Relay => via.origin_peer(),
        _ => peer,
    };
    match &first.payload {
        Some(Payload::Hello(hello)) if hello.agent_id == origin.id() => Ok(()),
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

/// A stored `Hello` and path are the agent's they are filed under.
fn names_origin(envelope: &Envelope, via: &Via, agent_id: &str) -> bool {
    via.origin_peer().id() == agent_id
        && matches!(&envelope.payload, Some(Payload::Hello(hello)) if hello.agent_id == agent_id)
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
        let received = match peer.role() {
            PeerRole::Relay => Via::from_metadata(request.metadata())?,
            _ => None,
        };
        let via = Via::extend(received, &peer)?;
        let mut inbound = request.into_inner();
        let hello = first_envelope(&mut inbound, self.settings.hello_timeout).await?;
        check_hello(&peer, &via, &hello).inspect_err(|status| {
            tracing::warn!(%peer, %status, "stream refused");
        })?;
        let agent_id = via.origin_peer().id().to_owned();
        // Waits for an older session of this agent to stop, so its records
        // stay in order.
        let held = self.lanes.take(&agent_id).await;
        if let Err(error) = self.buffer.set_hello(&agent_id, &hello, &via).await {
            tracing::error!(%agent_id, %error, "cannot store the Hello; buffered batches cannot be replayed without the agent");
        }
        let (down_tx, down_rx) = mpsc::channel(self.settings.queue.max(1));
        let mut retry_at = Instant::now();
        let upstream = if *self.upstream_up.borrow() {
            match self.open(&hello, &via, Some(down_tx.clone())).await {
                Ok(upstream) => Some(upstream),
                Err(Ended::Status(status)) => {
                    if session::is_permanent(&status) {
                        let _ = self.buffer.discard(&agent_id).await;
                    }
                    return Err(status);
                }
                Err(Ended::Link) => {
                    retry_at += self.settings.backoff.clone().next_delay();
                    None
                }
            }
        } else {
            None
        };
        match upstream {
            Some(_) => tracing::info!(%agent_id, %via, "stream opened"),
            None => {
                tracing::info!(%agent_id, %via, "stream opened; buffering until the upstream is back")
            }
        }
        let side = AgentSide {
            inbound,
            down: down_tx,
            last_sequence: hello.sequence,
        };
        let session = Session {
            forwarder: self.clone(),
            agent_id,
            hello,
            via,
            held,
            sequence: 1,
        };
        tokio::spawn(session.run(Some(side), upstream, retry_at));
        Ok(Response::new(Box::pin(ReceiverStream::new(down_rx))))
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
    fn hellos_must_name_the_origin() {
        assert!(check_peer(&agent("web-01")).is_ok());
        let relay = PeerIdentity::new(PeerRole::Relay, "b").unwrap();
        assert!(check_peer(&relay).is_ok());
        let server = PeerIdentity::new(PeerRole::Server, "x").unwrap();
        assert_eq!(
            check_peer(&server).unwrap_err().code(),
            Code::PermissionDenied
        );

        let web01 = agent("web-01");
        // Whatever path an agent sends is replaced by itself.
        let forged = Some(Via::origin(agent("web-02")));
        let via = Via::extend(forged, &web01).unwrap();
        assert!(check_hello(&web01, &via, &hello("web-01")).is_ok());
        let other = check_hello(&web01, &via, &hello("web-02")).unwrap_err();
        assert_eq!(other.code(), Code::PermissionDenied);
        assert!(other.message().contains("web-02"), "{other}");
        let heartbeat = Envelope {
            sequence: 1,
            payload: Some(Payload::Heartbeat(Heartbeat::default())),
        };
        let status = check_hello(&web01, &via, &heartbeat).unwrap_err();
        assert_eq!(status.code(), Code::InvalidArgument);

        // A downstream relay speaks for itself without a path, or for the
        // origin of its path.
        let own = Via::extend(None, &relay).unwrap();
        assert!(check_hello(&relay, &own, &hello("b")).is_ok());
        assert!(check_hello(&relay, &own, &hello("web-01")).is_err());
        let below = Via::extend(Some(via), &relay).unwrap();
        assert!(check_hello(&relay, &below, &hello("web-01")).is_ok());
        assert!(check_hello(&relay, &below, &hello("b")).is_err());
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
