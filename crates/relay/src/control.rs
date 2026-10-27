//! The relay's own stream to its upstream: `Hello` as itself, then a
//! heartbeat every interval, reconnecting with the policy of ADR-0009.
//!
//! While it is open, `upstream_up` is true and agent streams are forwarded;
//! otherwise they are accepted without dialing and their batches buffered,
//! instead of making the relay dial a dead upstream for each of them
//! (ADR-0027).
//! A renewed certificate closes the stream, which reconnects with it.
//!
//! After `Hello` and whenever it changes, the stream carries the report of
//! the mirrors the relay serves (ADR-0037); a relay without mirrors sends
//! an empty one, so the server forgets an earlier report.
//!
//! Commands addressed to the relay itself, only `self_update` (ADR-0055),
//! go to its dispatcher ([`crate::self_update`]); their results go up on
//! this stream, waiting for the next connection while it is down. Each
//! time the upstream accepts the stream, the relay records it for the
//! self-update watchdog.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use monitorflow_agent::commands::StreamEnd;
use monitorflow_common::client::backoff::Backoff;
use monitorflow_common::client::failure::{classify_error, classify_status, Failure};
use monitorflow_common::client::identity::StoredIdentity;
use monitorflow_common::client::shutdown::Shutdown;
use monitorflow_common::config::RelayConfig;
use monitorflow_common::proto::v1::agent_service_client::AgentServiceClient;
use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::{CommandResult, Envelope, Heartbeat, Hello, RelayMirrors};
use monitorflow_common::tls::{self, error_chain};
use monitorflow_common::PROTOCOL_VERSION;
use tokio::sync::{mpsc, watch};
use tokio::time::{interval_at, timeout, Instant, MissedTickBehavior};
use tokio_stream::wrappers::ReceiverStream;

const OPEN_TIMEOUT: Duration = Duration::from_secs(30);
const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone)]
pub struct ControlSettings {
    pub upstream_url: String,
    pub heartbeat_interval: Duration,
    pub connect_timeout: Duration,
    pub backoff: Backoff,
    /// A connection open this long resets the backoff.
    pub healthy_after: Duration,
    pub rejected_retry: Duration,
    pub revoked_retry: Duration,
}

impl ControlSettings {
    /// The agent's values (ADR-0009).
    pub fn from_config(config: &RelayConfig) -> Self {
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

pub struct Control {
    settings: ControlSettings,
    identity: watch::Receiver<StoredIdentity>,
    up: watch::Sender<bool>,
    mirrors: watch::Receiver<RelayMirrors>,
    commands: Option<StreamEnd>,
    /// Where the self-update watchdog looks for the connection.
    connected_dir: Option<PathBuf>,
    sequence: u64,
}

impl Control {
    /// Returns the task and the receiver of its "upstream is connected"
    /// flag, for [`crate::forward::Forwarder`].
    pub fn new(
        settings: ControlSettings,
        identity: watch::Receiver<StoredIdentity>,
    ) -> (Self, watch::Receiver<bool>) {
        let (up, rx) = watch::channel(false);
        let control = Self {
            settings,
            identity,
            up,
            // No mirrors until `with_mirrors`.
            mirrors: watch::channel(RelayMirrors::default()).1,
            commands: None,
            connected_dir: None,
            sequence: 0,
        };
        (control, rx)
    }

    /// Reports of the served mirrors, from the mirror task.
    pub fn with_mirrors(mut self, mirrors: watch::Receiver<RelayMirrors>) -> Self {
        self.mirrors = mirrors;
        self
    }

    /// The relay's own commands and their results (ADR-0055).
    pub fn with_commands(mut self, commands: StreamEnd) -> Self {
        self.commands = Some(commands);
        self
    }

    /// Records each accepted stream in `data_dir` for the self-update
    /// watchdog.
    pub fn with_connected_record(mut self, data_dir: PathBuf) -> Self {
        self.connected_dir = Some(data_dir);
        self
    }

    /// Keeps the stream open until `shutdown`.
    pub async fn run(mut self, mut shutdown: Shutdown) {
        let mut backoff = self.settings.backoff.clone();
        let mut commands = self.commands.take();
        loop {
            let mut opened = None;
            let result = self
                .session(&mut shutdown, &mut opened, &mut commands)
                .await;
            self.up.send_replace(false);
            let Err(failure) = result else { return };
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
                    tracing::warn!(error = %failure, retry_in = ?delay, "upstream connection lost; buffering agents' batches until it is back");
                    delay
                }
                Failure::Replaced => {
                    let delay = backoff.next_delay();
                    tracing::warn!(retry_in = ?delay, "another connection took over this relay's identity; is a copy of this relay running?");
                    delay
                }
                Failure::Rejected(_) => {
                    let delay = self.settings.rejected_retry;
                    tracing::error!(error = %failure, retry_in = ?delay, "fatal: the upstream refuses this relay; check its certificate and the server logs");
                    delay
                }
                Failure::Revoked => {
                    let delay = self.settings.revoked_retry;
                    tracing::error!(retry_in = ?delay, "fatal: this relay's certificate is revoked; remove the identity files and enroll again with a new token");
                    delay
                }
            };
            tokio::select! {
                () = tokio::time::sleep(delay) => {}
                () = shutdown.wait() => return,
            }
        }
    }

    async fn session(
        &mut self,
        shutdown: &mut Shutdown,
        opened: &mut Option<Instant>,
        commands: &mut Option<StreamEnd>,
    ) -> Result<(), Failure> {
        let identity = self.identity.borrow_and_update().clone();
        let url = self.settings.upstream_url.clone();
        let transient = |e: &dyn std::fmt::Display| Failure::Transient(e.to_string());
        let config = identity.tls.client_config().map_err(|e| transient(&e))?;
        let endpoint = tls::endpoint(&url)
            .map_err(|e| transient(&e))?
            .connect_timeout(self.settings.connect_timeout)
            .http2_keep_alive_interval(self.settings.heartbeat_interval)
            .keep_alive_timeout(KEEPALIVE_TIMEOUT)
            .keep_alive_while_idle(true);
        let connect =
            endpoint.connect_with_connector(tls::connector(config, self.settings.connect_timeout));
        let channel = tokio::select! {
            channel = connect => channel.map_err(|error| {
                let message = format!("cannot connect to {url}: {}", error_chain(&error));
                classify_error(&error, &message)
            })?,
            () = shutdown.wait() => return Ok(()),
        };
        let (tx, rx) = mpsc::channel(4);
        let hello = self.envelope(Payload::Hello(Hello {
            agent_id: identity.peer_id().to_owned(),
            protocol_version: PROTOCOL_VERSION.into(),
            agent_version: monitorflow_common::VERSION.into(),
            ..Hello::default()
        }));
        tx.try_send(hello)
            .expect("a new channel has room for Hello");
        let report = self.mirror_report();
        tx.try_send(report)
            .expect("a new channel has room for Hello and the mirror report");
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
        self.up.send_replace(true);
        if let Some(dir) = &self.connected_dir {
            crate::self_update::record_connected(dir.clone());
        }
        tracing::info!(upstream = %url, relay_id = identity.peer_id(), "upstream stream opened; forwarding agents");

        let every = self.settings.heartbeat_interval;
        let mut heartbeat = interval_at(Instant::now() + every, every);
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
        // Set when the renewal task is gone, so nothing will change.
        let mut fixed = false;
        // Set when the mirror task is gone, likewise.
        let mut mirrors_fixed = false;
        // A changed report the full queue did not take; retried with the
        // next heartbeat.
        let mut report_pending = false;
        loop {
            tokio::select! {
                () = shutdown.wait() => return Ok(()),
                changed = self.identity.changed(), if !fixed => match changed {
                    Ok(()) => return Err(Failure::Renewed),
                    Err(_) => fixed = true,
                },
                _ = heartbeat.tick() => {
                    let envelope = self.envelope(Payload::Heartbeat(Heartbeat {
                        sent_at_unix_ms: unix_ms(),
                    }));
                    if tx.try_send(envelope).is_err() {
                        tracing::debug!("upstream is not reading; heartbeat skipped");
                    }
                    if report_pending {
                        let report = self.mirror_report();
                        report_pending = tx.try_send(report).is_err();
                    }
                }
                changed = self.mirrors.changed(), if !mirrors_fixed => match changed {
                    Ok(()) => {
                        let report = self.mirror_report();
                        report_pending = tx.try_send(report).is_err();
                    }
                    Err(_) => mirrors_fixed = true,
                },
                result = next_result(commands) => {
                    let envelope = self.envelope(Payload::CommandResult(result));
                    // Lost on a full queue: the server sends the command
                    // again and the journal answers it.
                    if tx.try_send(envelope).is_err() {
                        tracing::warn!("upstream is not reading; command result dropped");
                    }
                }
                message = inbound.message() => match message {
                    Ok(Some(Envelope { payload: Some(Payload::Command(command)), .. })) => {
                        match commands {
                            Some(end) => {
                                if end.commands.try_send(command).is_err() {
                                    tracing::warn!("command queue full; the server will send it again");
                                }
                            }
                            None => tracing::debug!("ignoring a command: this relay runs none"),
                        }
                    }
                    Ok(Some(envelope)) => tracing::debug!(sequence = envelope.sequence, "ignoring envelope for the relay"),
                    Ok(None) => return Err(Failure::Transient("the upstream closed the stream".into())),
                    Err(status) => return Err(classify_status(&status)),
                },
            }
        }
    }

    /// The latest mirror report, marked as seen.
    fn mirror_report(&mut self) -> Envelope {
        let report = self.mirrors.borrow_and_update().clone();
        self.envelope(Payload::RelayMirrors(report))
    }

    fn envelope(&mut self, payload: Payload) -> Envelope {
        self.sequence += 1;
        Envelope {
            sequence: self.sequence,
            payload: Some(payload),
        }
    }
}

/// The next result of the relay's commands; never, without them.
async fn next_result(commands: &mut Option<StreamEnd>) -> CommandResult {
    match commands {
        Some(end) => match end.results.recv().await {
            Some(result) => result,
            None => std::future::pending().await,
        },
        None => std::future::pending().await,
    }
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
        let config = RelayConfig {
            upstream_url: "https://mf.example:7443".into(),
            heartbeat_interval: 10,
            ..RelayConfig::default()
        };
        let settings = ControlSettings::from_config(&config);
        assert_eq!(settings.heartbeat_interval, Duration::from_secs(10));
        assert_eq!(settings.healthy_after, Duration::from_secs(30));
        assert_eq!(settings.upstream_url, "https://mf.example:7443");
    }
}
