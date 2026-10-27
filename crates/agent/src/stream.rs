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
//! Sensor configurations from the upstream go to the sensors task, and its
//! acknowledgements go up on the current stream (ADR-0016). Samples go up
//! in `SampleBatch` envelopes taken from the [`Uplink`] when a batch is
//! full or at each flush interval; while disconnected they stay there.
//!
//! Package reports go up when published, or on the next connection
//! (ADR-0031). Commands go to the command dispatcher, which checks and runs them, and
//! its results go up on the current stream (ADR-0031). Results produced
//! while disconnected wait in its queue.
//!
//! Envelope payloads are never logged.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use monitorflow_common::client::failure::classify_error;
pub use monitorflow_common::client::failure::{classify_status, Failure};
use monitorflow_common::config::AgentConfig;
use monitorflow_common::proto::v1::agent_service_client::AgentServiceClient;
use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::{
    CommandResult, Envelope, Heartbeat, Hello, PackageReport, SampleBatch, SensorConfigAck,
};
use monitorflow_common::tls::{self, error_chain, TlsError};
use monitorflow_common::PROTOCOL_VERSION;
use tokio::sync::{mpsc, watch};
use tokio::time::{interval_at, timeout, Instant, MissedTickBehavior};
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Channel;
use tonic::Streaming;

use crate::backoff::Backoff;
use crate::commands;
use crate::identity::AgentIdentity;
use crate::os::HostInfo;
use crate::sensor_config::StreamEnd;
use crate::shutdown::Shutdown;
use crate::upload::Uplink;

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

/// One agent's connection to its upstream.
pub struct Agent {
    settings: StreamSettings,
    identity: AgentIdentity,
    updates: Option<watch::Receiver<AgentIdentity>>,
    host: HostInfo,
    sensors: Option<StreamEnd>,
    uplink: Option<Uplink>,
    commands: Option<commands::StreamEnd>,
    reports: Option<watch::Receiver<Option<PackageReport>>>,
    on_open: Option<Arc<dyn Fn() + Send + Sync>>,
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
            sensors: None,
            uplink: None,
            commands: None,
            reports: None,
            on_open: None,
            sequence: 0,
        }
    }

    /// Sends the samples queued in `uplink` upstream.
    pub fn with_upload(mut self, uplink: Uplink) -> Self {
        self.uplink = Some(uplink);
        self
    }

    /// Passes sensor configurations to the sensors task and sends its
    /// acknowledgements upstream; without it they are ignored.
    pub fn with_sensors(mut self, sensors: StreamEnd) -> Self {
        self.sensors = Some(sensors);
        self
    }

    /// Passes commands to the command dispatcher and sends its results
    /// upstream; without it commands are ignored.
    pub fn with_commands(mut self, commands: commands::StreamEnd) -> Self {
        self.commands = Some(commands);
        self
    }

    /// Sends each package report published on `reports` (ADR-0031): at
    /// once when connected, else on the next connection.
    pub fn with_reports(mut self, reports: watch::Receiver<Option<PackageReport>>) -> Self {
        self.reports = Some(reports);
        self
    }

    /// Calls `on_open` each time the upstream accepts `Hello`, like the
    /// self-update's health record (ADR-0053).
    pub fn on_open(mut self, on_open: Arc<dyn Fn() + Send + Sync>) -> Self {
        self.on_open = Some(on_open);
        self
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
                        agent_id = self.identity.peer_id(),
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
        tracing::info!(upstream = %self.settings.upstream_url, agent_id = self.identity.peer_id(), "stream opened");
        if let Some(on_open) = &self.on_open {
            on_open();
        }

        let every = self.settings.heartbeat_interval;
        let mut heartbeat = interval_at(Instant::now() + every, every);
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let flush_every = self
            .uplink
            .as_ref()
            .map_or(every, |u| u.settings().flush_interval);
        let mut flush = interval_at(Instant::now() + flush_every, flush_every);
        flush.set_missed_tick_behavior(MissedTickBehavior::Delay);
        // Set by a flush tick while samples wait; cleared once all went.
        let mut flush_due = false;
        loop {
            let ready = self
                .uplink
                .as_ref()
                .is_some_and(|u| u.is_full_batch() || (flush_due && !u.is_empty()));
            // Set when this connection must close.
            let close = tokio::select! {
                () = shutdown.wait() => Some(Ok(())),
                () = changed(&mut self.updates) => Some(Err(Failure::Renewed)),
                _ = heartbeat.tick() => {
                    let envelope = self.envelope(Payload::Heartbeat(Heartbeat {
                        sent_at_unix_ms: unix_ms(),
                    }));
                    if let Err(mpsc::error::TrySendError::Full(_)) = tx.try_send(envelope) {
                        tracing::debug!("upstream is not reading; heartbeat skipped");
                    }
                    None
                }
                _ = flush.tick(), if self.uplink.is_some() => {
                    flush_due = self.uplink.as_ref().is_some_and(|u| !u.is_empty());
                    None
                }
                () = filled(&self.uplink), if !ready => None,
                permit = tx.reserve(), if ready => {
                    if let (Ok(permit), Some(uplink)) = (permit, &self.uplink) {
                        let samples = uplink.take();
                        flush_due &= !uplink.is_empty();
                        tracing::trace!(samples = samples.len(), "sample batch");
                        permit.send(self.envelope(Payload::Samples(SampleBatch { samples })));
                    }
                    None
                }
                Some(ack) = next_ack(&mut self.sensors) => {
                    let envelope = self.envelope(Payload::SensorConfigAck(ack));
                    if let Err(mpsc::error::TrySendError::Full(_)) = tx.try_send(envelope) {
                        tracing::debug!("upstream is not reading; sensor configuration ack skipped");
                    }
                    None
                }
                Some(result) = next_result(&mut self.commands) => {
                    let envelope = self.envelope(Payload::CommandResult(result));
                    // Waits for room: results matter more than samples.
                    let _ = tx.send(envelope).await;
                    None
                }
                Some(report) = next_report(&mut self.reports) => {
                    let envelope = self.envelope(Payload::PackageReport(report));
                    let _ = tx.send(envelope).await;
                    None
                }
                message = inbound.message() => match message {
                    Ok(Some(envelope)) => {
                        self.receive(envelope);
                        None
                    }
                    Ok(None) => return Err(Failure::Transient("the upstream closed the stream".into())),
                    Err(status) => return Err(classify_status(&status)),
                },
            };
            if let Some(result) = close {
                drop(tx);
                let _ = timeout(CLOSE_GRACE, drain(&mut inbound)).await;
                if result.is_ok() {
                    tracing::info!("stream closed");
                }
                return result;
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
            agent_id: self.identity.peer_id().to_owned(),
            protocol_version: PROTOCOL_VERSION.into(),
            agent_version: monitorflow_common::VERSION.into(),
            hostname: self.host.hostname.clone(),
            os_family: self.host.os_family.as_str().into(),
            os_version: self.host.os_version.clone(),
        }
    }

    /// Logs what came down, never the payload itself.
    fn receive(&self, envelope: Envelope) {
        let sequence = envelope.sequence;
        match envelope.payload {
            Some(Payload::SensorConfig(config)) => match &self.sensors {
                Some(sensors) => {
                    tracing::debug!(
                        sequence,
                        revision = config.revision,
                        "sensor configuration received"
                    );
                    sensors.configs.send_replace(Some(config));
                }
                None => tracing::debug!(sequence, "ignoring sensor configuration"),
            },
            Some(Payload::Command(command)) => match &self.commands {
                Some(commands) => {
                    tracing::debug!(sequence, command_id = %command.command_id.escape_debug(), "command received");
                    if commands.commands.try_send(command).is_err() {
                        tracing::warn!(
                            sequence,
                            "command queue full; the server will send it again"
                        );
                    }
                }
                None => tracing::warn!(sequence, "ignoring command: no command dispatcher"),
            },
            Some(other) => tracing::debug!(sequence, kind = kind(&other), "ignoring envelope"),
            None => tracing::debug!(sequence, "ignoring envelope without payload"),
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

fn kind(payload: &Payload) -> &'static str {
    match payload {
        Payload::Hello(_) => "hello",
        Payload::Heartbeat(_) => "heartbeat",
        Payload::Samples(_) => "samples",
        Payload::SensorConfig(_) => "sensor_config",
        Payload::SensorConfigAck(_) => "sensor_config_ack",
        Payload::Command(_) => "command",
        Payload::CommandResult(_) => "command_result",
        Payload::PackageReport(_) => "package_report",
        Payload::RelayMirrors(_) => "relay_mirrors",
    }
}

/// Resolves once a full batch waits; never without an uplink.
async fn filled(uplink: &Option<Uplink>) {
    match uplink {
        Some(uplink) => uplink.filled().await,
        None => std::future::pending().await,
    }
}

/// The next acknowledgement of the sensors task; never without one.
async fn next_ack(sensors: &mut Option<StreamEnd>) -> Option<SensorConfigAck> {
    match sensors {
        Some(sensors) => sensors.acks.recv().await,
        None => std::future::pending().await,
    }
}

/// The next result of the command dispatcher; never without one.
async fn next_result(commands: &mut Option<commands::StreamEnd>) -> Option<CommandResult> {
    match commands {
        Some(commands) => commands.results.recv().await,
        None => std::future::pending().await,
    }
}

/// The next package report not sent yet; never without a publisher.
async fn next_report(
    reports: &mut Option<watch::Receiver<Option<PackageReport>>>,
) -> Option<PackageReport> {
    let Some(reports) = reports else {
        return std::future::pending().await;
    };
    loop {
        if reports.changed().await.is_err() {
            return std::future::pending().await;
        }
        if let Some(report) = reports.borrow_and_update().clone() {
            return Some(report);
        }
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
