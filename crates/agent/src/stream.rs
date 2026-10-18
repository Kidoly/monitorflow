//! The long-lived `AgentService::Stream` to the upstream: `Hello` first,
//! then a `Heartbeat` every interval, reconnecting with backoff.
//!
//! Failures fall in three classes, each logged differently:
//! - transient (network, server restart, heartbeat timeout): retry with
//!   exponential backoff and full jitter, reset once a connection stayed up
//!   for [`StreamSettings::healthy_after`];
//! - rejected (the upstream refused our certificate or our `Hello`): only an
//!   operator can fix it, so retry slowly, at error level, without exiting,
//!   which keeps the fleet alive through a server-side mistake like a stale
//!   CRL;
//! - revoked: only a new enrollment helps; retry very slowly.
//!
//! When the renewal task publishes a new identity, the stream closes and
//! reconnects with it at once.
//!
//! Envelope payloads are never logged, and commands are never executed:
//! signed command execution is Phase 3.

use std::error::Error as StdError;
use std::io;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use monitorflow_common::config::AgentConfig;
use monitorflow_common::proto::v1::agent_service_client::AgentServiceClient;
use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::{Envelope, Heartbeat, Hello};
use monitorflow_common::tls::{self, error_chain, TlsError};
use monitorflow_common::PROTOCOL_VERSION;
use rustls::AlertDescription;
use tokio::sync::{mpsc, watch};
use tokio::time::{interval_at, timeout, Instant, MissedTickBehavior};
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Channel;
use tonic::{Code, Status, Streaming};

use crate::backoff::Backoff;
use crate::identity::AgentIdentity;
use crate::os::HostInfo;
use crate::shutdown::Shutdown;

/// Envelopes queued towards the upstream. Heartbeats are skipped, not
/// queued, when it is full.
const OUTBOUND_CAPACITY: usize = 16;
/// Time allowed for the server to accept `Hello`.
const OPEN_TIMEOUT: Duration = Duration::from_secs(30);
/// HTTP/2 pings detect a dead upstream that never closed the connection.
const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(20);
/// Time given to the upstream to see our half-close on shutdown.
const CLOSE_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct StreamSettings {
    pub upstream_url: String,
    pub heartbeat_interval: Duration,
    pub connect_timeout: Duration,
    pub backoff: Backoff,
    /// A connection open this long resets the backoff.
    pub healthy_after: Duration,
    /// Wait after the upstream rejected our certificate or `Hello`.
    pub rejected_retry: Duration,
    /// Wait after the upstream said our certificate is revoked.
    pub revoked_retry: Duration,
}

impl StreamSettings {
    pub fn from_config(config: &AgentConfig) -> Self {
        let heartbeat_interval = Duration::from_secs(config.heartbeat_interval);
        Self {
            upstream_url: config.upstream_url.clone(),
            heartbeat_interval,
            connect_timeout: tls::DEFAULT_TIMEOUT,
            backoff: Backoff::default(),
            healthy_after: heartbeat_interval.max(Duration::from_secs(30)),
            rejected_retry: Duration::from_secs(5 * 60),
            revoked_retry: Duration::from_secs(60 * 60),
        }
    }
}

/// Why a connection ended.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Failure {
    #[error("{0}")]
    Transient(String),
    /// The upstream registered a newer connection with our identity.
    #[error("replaced by a newer connection with the same identity")]
    Replaced,
    #[error("the upstream rejected this agent: {0}")]
    Rejected(String),
    #[error("the upstream says this agent's certificate is revoked")]
    Revoked,
    /// Not a failure: the certificate was renewed, reconnect with it.
    #[error("the certificate was renewed")]
    Renewed,
}

/// Maps a status from the upstream, looking through transport errors for
/// a TLS alert about our certificate.
pub fn classify_status(status: &Status) -> Failure {
    match status.code() {
        Code::Aborted => Failure::Replaced,
        Code::PermissionDenied
        | Code::Unauthenticated
        | Code::FailedPrecondition
        | Code::InvalidArgument => {
            Failure::Rejected(format!("{:?}: {}", status.code(), status.message()))
        }
        _ => classify_error(
            status,
            &format!("{:?}: {}", status.code(), error_chain(status)),
        ),
    }
}

/// Alerts the upstream sends when it refuses our certificate. rustls sends
/// `DecryptError` for a bad signature on it, as from a CA with the same
/// name but another key.
const CERT_ALERTS: [AlertDescription; 9] = [
    AlertDescription::CertificateRevoked,
    AlertDescription::BadCertificate,
    AlertDescription::UnsupportedCertificate,
    AlertDescription::CertificateExpired,
    AlertDescription::CertificateUnknown,
    AlertDescription::CertificateRequired,
    AlertDescription::UnknownCA,
    AlertDescription::AccessDenied,
    AlertDescription::DecryptError,
];

fn classify_error(error: &(dyn StdError + 'static), message: &str) -> Failure {
    match find_alert(error) {
        Some(AlertDescription::CertificateRevoked) => Failure::Revoked,
        Some(alert) if CERT_ALERTS.contains(&alert) => {
            Failure::Rejected(format!("TLS alert {alert:?}"))
        }
        _ => Failure::Transient(message.to_owned()),
    }
}

/// The TLS alert the peer sent about our certificate, if `error` was caused
/// by one. h2 and `io::Error` hide their inner error from `source()`, so
/// when downcasting finds nothing, look for rustls's own wording of the
/// alert in the error chain.
fn find_alert(error: &(dyn StdError + 'static)) -> Option<AlertDescription> {
    let mut next = Some(error);
    while let Some(error) = next {
        if let Some(rustls::Error::AlertReceived(alert)) = error.downcast_ref::<rustls::Error>() {
            return Some(*alert);
        }
        if let Some(inner) = error.downcast_ref::<io::Error>().and_then(|e| e.get_ref()) {
            if let Some(alert) = find_alert(inner) {
                return Some(alert);
            }
        }
        next = error.source();
    }
    let chain = error_chain(error);
    CERT_ALERTS
        .into_iter()
        .find(|alert| chain.contains(&rustls::Error::AlertReceived(*alert).to_string()))
}

/// One agent's connection to its upstream.
pub struct Agent {
    settings: StreamSettings,
    identity: AgentIdentity,
    updates: Option<watch::Receiver<AgentIdentity>>,
    host: HostInfo,
    // TODO(phase2-buffer): persist with the sample buffer so sequences stay
    // monotonic across restarts; the server only needs them per stream.
    sequence: u64,
}

impl Agent {
    pub fn new(settings: StreamSettings, identity: AgentIdentity, host: HostInfo) -> Self {
        Self {
            settings,
            identity,
            updates: None,
            host,
            sequence: 0,
        }
    }

    /// Uses each identity published on `updates`, from the renewal task,
    /// reconnecting when it changes.
    pub fn follow(mut self, updates: watch::Receiver<AgentIdentity>) -> Self {
        self.updates = Some(updates);
        self
    }

    /// Keeps a stream open until `shutdown`. Only fails when the TLS client
    /// configuration cannot be built at all.
    pub async fn run(mut self, mut shutdown: Shutdown) -> Result<(), TlsError> {
        let mut backoff = self.settings.backoff.clone();
        loop {
            let mut opened = None;
            let failure = match self.session(&mut shutdown, &mut opened).await {
                Ok(()) => return Ok(()),
                Err(failure) => failure,
            };
            if opened.is_some_and(|at: Instant| at.elapsed() >= self.settings.healthy_after) {
                backoff.reset();
            }
            let delay = match &failure {
                Failure::Renewed => {
                    tracing::info!("reconnecting with the renewed certificate");
                    continue;
                }
                Failure::Transient(_) => {
                    let delay = backoff.next_delay();
                    tracing::warn!(error = %failure, retry_in = ?delay, "upstream connection lost");
                    delay
                }
                Failure::Replaced => {
                    let delay = backoff.next_delay();
                    tracing::warn!(
                        agent_id = self.identity.agent_id(),
                        retry_in = ?delay,
                        "another connection took over this agent's identity; is a copy of this host running?"
                    );
                    delay
                }
                Failure::Rejected(_) => {
                    let delay = self.settings.rejected_retry;
                    tracing::error!(error = %failure, retry_in = ?delay, "fatal: the upstream refuses this agent; check its certificate and the server logs");
                    delay
                }
                Failure::Revoked => {
                    let delay = self.settings.revoked_retry;
                    tracing::error!(retry_in = ?delay, "fatal: this agent's certificate is revoked; remove the identity files and enroll again with a new token");
                    delay
                }
            };
            tokio::select! {
                () = tokio::time::sleep(delay) => {}
                () = shutdown.wait() => return Ok(()),
            }
        }
    }

    /// One connection, from dial to failure. `Ok` means shutdown. `opened`
    /// is set once the upstream accepted `Hello`.
    pub async fn session(
        &mut self,
        shutdown: &mut Shutdown,
        opened: &mut Option<Instant>,
    ) -> Result<(), Failure> {
        if let Some(updates) = &mut self.updates {
            self.identity = updates.borrow_and_update().clone();
        }
        let channel = tokio::select! {
            channel = self.connect() => channel?,
            () = shutdown.wait() => return Ok(()),
        };
        let (tx, rx) = mpsc::channel(OUTBOUND_CAPACITY);
        let hello = self.envelope(Payload::Hello(self.hello()));
        tx.try_send(hello)
            .expect("a new channel has room for Hello");
        let mut client = AgentServiceClient::new(channel);
        let open = timeout(OPEN_TIMEOUT, client.stream(ReceiverStream::new(rx)));
        let mut inbound = tokio::select! {
            response = open => match response {
                Err(_) => return Err(Failure::Transient("the upstream did not answer Hello".into())),
                Ok(Err(status)) => return Err(classify_status(&status)),
                Ok(Ok(response)) => response.into_inner(),
            },
            () = shutdown.wait() => return Ok(()),
        };
        *opened = Some(Instant::now());
        tracing::info!(upstream = %self.settings.upstream_url, agent_id = self.identity.agent_id(), "stream opened");

        let every = self.settings.heartbeat_interval;
        let mut heartbeat = interval_at(Instant::now() + every, every);
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = shutdown.wait() => {
                    drop(tx);
                    let _ = timeout(CLOSE_GRACE, drain(&mut inbound)).await;
                    tracing::info!("stream closed");
                    return Ok(());
                }
                () = changed(&mut self.updates) => {
                    drop(tx);
                    let _ = timeout(CLOSE_GRACE, drain(&mut inbound)).await;
                    return Err(Failure::Renewed);
                }
                _ = heartbeat.tick() => {
                    let envelope = self.envelope(Payload::Heartbeat(Heartbeat {
                        sent_at_unix_ms: unix_ms(),
                    }));
                    if let Err(mpsc::error::TrySendError::Full(_)) = tx.try_send(envelope) {
                        tracing::debug!("upstream is not reading; heartbeat skipped");
                    }
                }
                message = inbound.message() => match message {
                    Ok(Some(envelope)) => receive(envelope),
                    Ok(None) => return Err(Failure::Transient("the upstream closed the stream".into())),
                    Err(status) => return Err(classify_status(&status)),
                },
            }
        }
    }

    async fn connect(&self) -> Result<Channel, Failure> {
        let url = &self.settings.upstream_url;
        let transient = |e: &dyn std::fmt::Display| Failure::Transient(e.to_string());
        let config = self
            .identity
            .tls
            .client_config()
            .map_err(|e| transient(&e))?;
        let timeout = self.settings.connect_timeout;
        tls::endpoint(url)
            .map_err(|e| transient(&e))?
            .connect_timeout(timeout)
            .http2_keep_alive_interval(self.settings.heartbeat_interval)
            .keep_alive_timeout(KEEPALIVE_TIMEOUT)
            .keep_alive_while_idle(true)
            .connect_with_connector(tls::connector(config, timeout))
            .await
            .map_err(|error| {
                let message = format!("cannot connect to {url}: {}", error_chain(&error));
                classify_error(&error, &message)
            })
    }

    fn hello(&self) -> Hello {
        Hello {
            agent_id: self.identity.agent_id().to_owned(),
            protocol_version: PROTOCOL_VERSION.into(),
            agent_version: env!("CARGO_PKG_VERSION").into(),
            hostname: self.host.hostname.clone(),
            os_family: self.host.os_family.as_str().into(),
            os_version: self.host.os_version.clone(),
        }
    }

    fn envelope(&mut self, payload: Payload) -> Envelope {
        self.sequence += 1;
        Envelope {
            sequence: self.sequence,
            payload: Some(payload),
        }
    }
}

/// Logs what came down, never the payload itself.
fn receive(envelope: Envelope) {
    let sequence = envelope.sequence;
    match envelope.payload {
        // TODO(phase3-commands): verify the signature, classify, execute.
        Some(Payload::Command(command)) => tracing::warn!(
            sequence,
            command_id = %command.command_id,
            kind = %command.kind,
            "ignoring command: command execution is not supported by this agent yet"
        ),
        Some(other) => tracing::debug!(sequence, kind = kind(&other), "ignoring envelope"),
        None => tracing::debug!(sequence, "ignoring envelope without payload"),
    }
}

fn kind(payload: &Payload) -> &'static str {
    match payload {
        Payload::Hello(_) => "hello",
        Payload::Heartbeat(_) => "heartbeat",
        Payload::Samples(_) => "samples",
        Payload::Command(_) => "command",
        Payload::CommandResult(_) => "command_result",
    }
}

/// Resolves when a new identity is published; never without a publisher.
async fn changed(updates: &mut Option<watch::Receiver<AgentIdentity>>) {
    let Some(updates) = updates else {
        return std::future::pending().await;
    };
    if updates.changed().await.is_err() {
        std::future::pending::<()>().await;
    }
}

async fn drain(inbound: &mut Streaming<Envelope>) {
    while let Ok(Some(_)) = inbound.message().await {}
}

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_from_the_server_are_classified() {
        let replaced = Status::aborted("replaced by a newer connection with the same ID");
        assert_eq!(classify_status(&replaced), Failure::Replaced);
        for status in [
            Status::permission_denied("agent a may not speak for agent b"),
            Status::unauthenticated("no client certificate"),
            Status::failed_precondition("protocol version 2.0 is not compatible"),
            Status::invalid_argument("the first envelope must be Hello"),
        ] {
            assert!(
                matches!(classify_status(&status), Failure::Rejected(_)),
                "{status:?}"
            );
        }
        for status in [
            Status::deadline_exceeded("no message for 90s"),
            Status::unavailable("server shutting down"),
            Status::unknown("transport error"),
        ] {
            assert!(
                matches!(classify_status(&status), Failure::Transient(_)),
                "{status:?}"
            );
        }
    }

    #[test]
    fn tls_alerts_are_found_inside_io_errors() {
        let wrap = |alert| {
            let tls = rustls::Error::AlertReceived(alert);
            io::Error::new(io::ErrorKind::ConnectionAborted, tls)
        };
        let revoked = wrap(AlertDescription::CertificateRevoked);
        assert_eq!(classify_error(&revoked, "x"), Failure::Revoked);
        let unknown_ca = wrap(AlertDescription::UnknownCA);
        assert!(matches!(
            classify_error(&unknown_ca, "x"),
            Failure::Rejected(_)
        ));
        let reset = io::Error::new(io::ErrorKind::ConnectionReset, "reset");
        assert_eq!(
            classify_error(&reset, "reset"),
            Failure::Transient("reset".into())
        );
        let other = wrap(AlertDescription::InternalError);
        assert!(matches!(classify_error(&other, "x"), Failure::Transient(_)));
        // Hidden behind a wrapper that drops the source: found by its text.
        let flat = io::Error::other(format!(
            "connection error: {}",
            wrap(AlertDescription::CertificateRevoked)
        ));
        assert_eq!(classify_error(&flat, "x"), Failure::Revoked);
    }

    #[test]
    fn settings_follow_the_config() {
        let config = AgentConfig {
            upstream_url: "https://relay:7443".into(),
            heartbeat_interval: 120,
            ..AgentConfig::default()
        };
        let settings = StreamSettings::from_config(&config);
        assert_eq!(settings.heartbeat_interval, Duration::from_secs(120));
        assert_eq!(settings.healthy_after, Duration::from_secs(120));
        assert!(settings.rejected_retry >= Duration::from_secs(60));
    }
}
