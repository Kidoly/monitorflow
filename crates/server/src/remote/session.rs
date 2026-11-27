//! One remote session, run on the instance holding the browser's
//! WebSocket (ADR-0190). The WebSocket itself is the API's: here the
//! browser is two channels, so the session can be driven without one.

use std::time::Duration;

use monitorflow_common::command::{REMOTE_DESKTOP, REMOTE_SESSION};
use monitorflow_common::proto::v1::{
    remote_data, remote_frame, RemoteClosed, RemoteData, RemoteFrame, RemoteOpened, RemoteResize,
    RemoteSessionOpen,
};
use monitorflow_common::remote::{
    self as sealing, Channel, ServerHandshake, DESKTOP, DESKTOP_WINDOW, MAX_CHUNK, TERMINAL,
};
use monitorflow_common::tls::PeerRole;
use prost::Message;
use serde_json::json;
use tokio::sync::mpsc;
use tokio::time::{interval, sleep_until, Instant, MissedTickBehavior};
use uuid::Uuid;

use super::{Inbound, RemoteHub, COMMAND_TTL, INBOX};
use crate::api::audit::{self, Event};
use crate::ca::Serial;
use crate::commands::{CommandStore, NewCommand, State};

/// Output kept before it is written to the recording.
const RECORD_BUFFER: usize = 64 * 1024;
/// Time between two writes of the recording, and of `seen_at`.
const FLUSH_EVERY: Duration = Duration::from_secs(2);
const SEEN_EVERY: Duration = Duration::from_secs(20);
/// The agent closes an idle session itself; the server waits this much
/// longer, for its word.
const IDLE_SLACK: Duration = Duration::from_secs(10);
/// A session whose agent has no stream anywhere for this long ends.
const AGENT_GONE: Duration = Duration::from_secs(30);
/// A desktop's output frames passed on between two `delivered`: a
/// quarter of the agent's window, so it never waits on a busy link.
const DELIVERED_EVERY: u64 = DESKTOP_WINDOW / 4;

/// From the browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FromBrowser {
    Input(Vec<u8>),
    Resize {
        cols: u32,
        rows: u32,
    },
    Close,
    /// The proxy of a desktop gives up, with why (ADR-0190).
    Abort(String),
}

/// To the browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToBrowser {
    /// The command is on its way to the agent.
    Waiting,
    /// The agent opened the terminal, or reached the RDP service.
    Opened,
    Output(Vec<u8>),
    /// The session ended, with why.
    Closed(String),
}

/// Why a browser cannot attach.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AttachError {
    #[error("this session's token is wrong, expired or already used")]
    Refused,
    #[error("the terminal size is not valid")]
    BadSize,
    #[error("the database is unavailable")]
    Database,
}

/// A session a browser attached to, ready to run.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Attached {
    pub id: Uuid,
    /// `terminal` or `desktop`.
    pub kind: String,
    pub server_id: Uuid,
    pub peer_id: String,
    pub hostname: String,
    pub username: String,
    pub requested_by: Option<Uuid>,
    pub recorded: bool,
    pub idle_timeout_seconds: i32,
    #[sqlx(skip)]
    pub cols: u32,
    #[sqlx(skip)]
    pub rows: u32,
}

impl Attached {
    fn is_terminal(&self) -> bool {
        self.kind == TERMINAL
    }
}

/// SHA-256 of a session token, as stored.
pub fn token_hash(token: &str) -> Vec<u8> {
    ring::digest::digest(&ring::digest::SHA256, token.as_bytes())
        .as_ref()
        .to_vec()
}

/// How a session went, for the row and the audit log.
struct Totals {
    bytes_in: i64,
    bytes_out: i64,
}

/// The output kept for the recording.
struct Recording {
    on: bool,
    seq: i32,
    pending: Vec<u8>,
    pending_at_ms: i64,
}

impl RemoteHub {
    /// Uses the one-time token of a terminal: the session is then this
    /// browser's, and no other can take it.
    pub async fn attach(
        &self,
        session: Uuid,
        token: &str,
        cols: u32,
        rows: u32,
    ) -> Result<Attached, AttachError> {
        if !sealing::valid_size(cols, rows) {
            return Err(AttachError::BadSize);
        }
        let mut attached = self.attach_kind(session, token, TERMINAL).await?;
        attached.cols = cols;
        attached.rows = rows;
        Ok(attached)
    }

    /// The same for a desktop: its token never opens a terminal, nor a
    /// terminal's a desktop.
    pub async fn attach_desktop(
        &self,
        session: Uuid,
        token: &str,
    ) -> Result<Attached, AttachError> {
        self.attach_kind(session, token, DESKTOP).await
    }

    async fn attach_kind(
        &self,
        session: Uuid,
        token: &str,
        kind: &str,
    ) -> Result<Attached, AttachError> {
        let attached: Option<Attached> = sqlx::query_as(
            "UPDATE remote_sessions r SET connected_at = now(), seen_at = now()
             FROM servers s
             WHERE r.id = $1 AND s.id = r.server_id AND r.token_sha256 = $2 AND r.kind = $3
               AND r.connected_at IS NULL AND r.closed_at IS NULL AND r.token_expires_at > now()
             RETURNING r.id, r.kind, r.server_id, s.peer_id, s.hostname, r.username,
                       r.requested_by, r.recorded, r.idle_timeout_seconds",
        )
        .bind(session)
        .bind(token_hash(token))
        .bind(kind)
        .fetch_optional(&self.inner.pool)
        .await
        .map_err(|_| AttachError::Database)?;
        attached.ok_or(AttachError::Refused)
    }

    /// Runs the session until it ends, and returns why it ended.
    pub async fn run(
        &self,
        attached: Attached,
        mut from_browser: mpsc::Receiver<FromBrowser>,
        to_browser: mpsc::Sender<ToBrowser>,
    ) -> String {
        let (inbox_tx, mut inbox) = mpsc::channel(INBOX);
        self.local().insert(attached.id, inbox_tx);
        let started = Instant::now();
        let mut totals = Totals {
            bytes_in: 0,
            bytes_out: 0,
        };
        // A desktop's output is TLS: its proxy records the screen
        // (`super::rdp::recorder`).
        let mut recording = Recording {
            on: attached.recorded && attached.is_terminal(),
            seq: 0,
            pending: Vec::new(),
            pending_at_ms: 0,
        };
        let mut channel = None;
        let reason = match self
            .open(&attached, &mut inbox, &mut from_browser, &to_browser)
            .await
        {
            Ok(opened) => {
                channel = Some(opened);
                let _ = to_browser.send(ToBrowser::Opened).await;
                self.serve(
                    &attached,
                    channel.as_mut().expect("just set"),
                    &mut inbox,
                    &mut from_browser,
                    &to_browser,
                    &mut totals,
                    &mut recording,
                )
                .await
            }
            Err(reason) => reason,
        };
        self.local().remove(&attached.id);
        // The agent hears it, sealed when the session opened, so a
        // session it no longer runs is not left open there.
        let frame = match channel.as_mut() {
            Some(channel) => sealed(&attached, channel, &sealing::close(reason.clone())),
            None => RemoteFrame {
                session_id: attached.id.to_string(),
                body: Some(remote_frame::Body::Closed(RemoteClosed {
                    reason: reason.clone(),
                })),
                counter: 0,
            },
        };
        self.to_agent(&attached.peer_id, frame).await;
        if let Err(error) = self
            .finish(
                &attached,
                &reason,
                started.elapsed(),
                &totals,
                &mut recording,
            )
            .await
        {
            tracing::error!(session = %attached.id, %error, "cannot record the end of a remote session");
        }
        let _ = to_browser.send(ToBrowser::Closed(reason.clone())).await;
        reason
    }

    /// Queues the command and waits for the agent's proven answer.
    async fn open(
        &self,
        attached: &Attached,
        inbox: &mut mpsc::Receiver<Inbound>,
        from_browser: &mut mpsc::Receiver<FromBrowser>,
        to_browser: &mpsc::Sender<ToBrowser>,
    ) -> Result<Channel, String> {
        let handshake =
            ServerHandshake::new().map_err(|e| format!("cannot start the session: {e}"))?;
        let payload = RemoteSessionOpen {
            session_id: attached.id.to_string(),
            kind: attached.kind.clone(),
            server_key: handshake.public_key().to_vec(),
            cols: attached.cols,
            rows: attached.rows,
            requested_by: attached.username.clone(),
            idle_timeout_seconds: attached.idle_timeout_seconds as u32,
            recorded: attached.recorded,
        };
        let command_id = self
            .queue(attached, payload.encode_to_vec())
            .await
            .map_err(|e| format!("cannot queue the session's command: {e}"))?;
        let _ = to_browser.send(ToBrowser::Waiting).await;
        let deadline = Instant::now() + COMMAND_TTL + Duration::from_secs(10);
        let mut check = interval(Duration::from_secs(2));
        check.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut size = (attached.cols, attached.rows);
        loop {
            tokio::select! {
                inbound = inbox.recv() => match inbound {
                    Some(Inbound::Frame { agent, frame }) if agent == attached.peer_id => {
                        match frame.body {
                            Some(remote_frame::Body::Opened(opened)) => {
                                let mut channel = self.prove(attached, handshake, &opened).await?;
                                self.opened(attached).await;
                                if attached.is_terminal() && size != (attached.cols, attached.rows) {
                                    let resize = resize(size.0, size.1);
                                    self.to_agent(&attached.peer_id, sealed(attached, &mut channel, &resize)).await;
                                }
                                return Ok(channel);
                            }
                            Some(remote_frame::Body::Closed(closed)) => {
                                return Err(bounded(&closed.reason, "the agent closed the session"));
                            }
                            _ => {}
                        }
                    }
                    Some(Inbound::Frame { .. }) => {}
                    Some(Inbound::Close { by }) => return Err(format!("closed by {by}")),
                    None => return Err("the central server is stopping".into()),
                },
                message = from_browser.recv() => match message {
                    Some(FromBrowser::Resize { cols, rows }) if sealing::valid_size(cols, rows) => size = (cols, rows),
                    Some(FromBrowser::Input(_) | FromBrowser::Resize { .. }) => {}
                    Some(FromBrowser::Abort(reason)) => return Err(reason),
                    Some(FromBrowser::Close) | None => {
                        return Err(format!("closed by {} before it opened", attached.username));
                    }
                },
                _ = check.tick() => {
                    if let Some(reason) = self.refused(command_id).await {
                        return Err(reason);
                    }
                }
                () = sleep_until(deadline) => {
                    return Err("the agent did not answer; is it connected?".into());
                }
            }
        }
    }

    /// Queues the signed command, cancelling nothing: each session has its
    /// own.
    async fn queue(&self, attached: &Attached, payload: Vec<u8>) -> Result<Uuid, sqlx::Error> {
        let mut tx = self.inner.pool.begin().await?;
        let row = CommandStore::enqueue_in(
            &mut *tx,
            &NewCommand {
                server_id: attached.server_id,
                kind: if attached.is_terminal() {
                    REMOTE_SESSION
                } else {
                    REMOTE_DESKTOP
                }
                .to_owned(),
                payload,
                ttl: COMMAND_TTL,
                requested_by: attached.requested_by,
                campaign_id: None,
            },
        )
        .await?;
        sqlx::query("UPDATE remote_sessions SET command_id = $2 WHERE id = $1")
            .bind(attached.id)
            .bind(row.id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        if let Some(commands) = &self.inner.commands {
            commands.dispatch(&attached.peer_id);
        }
        Ok(row.id)
    }

    /// Why the agent refused the command, once it did.
    async fn refused(&self, command_id: Uuid) -> Option<String> {
        let row: Option<(State, Option<String>)> =
            sqlx::query_as("SELECT state, error FROM commands WHERE id = $1")
                .bind(command_id)
                .fetch_optional(&self.inner.pool)
                .await
                .ok()?;
        match row? {
            (State::Failed | State::Cancelled, error) => {
                Some(error.unwrap_or_else(|| "the agent could not open the session".into()))
            }
            (State::Expired, _) => Some("the agent did not answer in time".into()),
            _ => None,
        }
    }

    /// The channel, once the agent's answer proves to come from it: its
    /// CA issued the certificate to this agent, which is not revoked, and
    /// its key signed both halves of the keys.
    async fn prove(
        &self,
        attached: &Attached,
        handshake: ServerHandshake,
        opened: &RemoteOpened,
    ) -> Result<Channel, String> {
        let refused = |why: &str| {
            tracing::warn!(session = %attached.id, agent_id = %attached.peer_id, why, "refused a remote session's keys");
            format!("the agent's answer could not be trusted: {why}")
        };
        let serial = x509_parser::parse_x509_certificate(&opened.certificate)
            .map(|(_, cert)| Serial::from_bytes(cert.raw_serial()))
            .map_err(|_| refused("its certificate does not parse"))?;
        let unrevoked: Option<bool> = sqlx::query_scalar(
            "SELECT revoked_at IS NULL FROM certificates
             WHERE serial = $1 AND peer_role = 'agent' AND peer_id = $2",
        )
        .bind(serial.as_bytes())
        .bind(&attached.peer_id)
        .fetch_optional(&self.inner.pool)
        .await
        .map_err(|_| "the database is unavailable".to_owned())?;
        if unrevoked != Some(true) {
            return Err(refused("its certificate is unknown or revoked"));
        }
        let ca = self.inner.ca.clone();
        handshake
            .finish(
                &attached.id.to_string(),
                &attached.peer_id,
                opened,
                |cert| ca.verify_issued(cert),
            )
            .map_err(|e| refused(&e.to_string()))
    }

    async fn opened(&self, attached: &Attached) {
        let result = async {
            let mut tx = self.inner.pool.begin().await?;
            sqlx::query(
                "UPDATE remote_sessions SET opened_at = now(), seen_at = now() WHERE id = $1",
            )
            .bind(attached.id)
            .execute(&mut *tx)
            .await?;
            audit(&mut tx, attached, "remote_session.open", json!({})).await?;
            tx.commit().await
        }
        .await;
        if let Err(error) = result {
            tracing::error!(session = %attached.id, %error, "cannot record that a remote session opened");
        }
        tracing::info!(session = %attached.id, agent_id = %attached.peer_id, user = %attached.username, kind = %attached.kind, "remote session opened");
    }

    /// Carries the open session until it ends; returns why.
    #[allow(clippy::too_many_arguments)]
    async fn serve(
        &self,
        attached: &Attached,
        channel: &mut Channel,
        inbox: &mut mpsc::Receiver<Inbound>,
        from_browser: &mut mpsc::Receiver<FromBrowser>,
        to_browser: &mpsc::Sender<ToBrowser>,
        totals: &mut Totals,
        recording: &mut Recording,
    ) -> String {
        let opened_at = Instant::now();
        let idle = Duration::from_secs(attached.idle_timeout_seconds as u64);
        let mut idle_at = opened_at + idle + IDLE_SLACK;
        let mut flush = interval(FLUSH_EVERY);
        flush.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut seen = interval(SEEN_EVERY);
        seen.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut agent_gone_since: Option<Instant> = None;
        // Desktops: output frames passed on, and the last count the agent
        // heard.
        let (mut delivered, mut told) = (0u64, 0u64);
        loop {
            tokio::select! {
                message = from_browser.recv() => match message {
                    Some(FromBrowser::Input(bytes)) => {
                        idle_at = Instant::now() + idle + IDLE_SLACK;
                        totals.bytes_in += bytes.len() as i64;
                        for chunk in bytes.chunks(MAX_CHUNK) {
                            let frame = sealed(attached, channel, &sealing::input(chunk.to_vec()));
                            if !self.to_agent(&attached.peer_id, frame).await {
                                return "the agent is not connected to the central server".into();
                            }
                        }
                    }
                    Some(FromBrowser::Resize { cols, rows }) => {
                        if sealing::valid_size(cols, rows) {
                            let frame = sealed(attached, channel, &resize(cols, rows));
                            self.to_agent(&attached.peer_id, frame).await;
                        }
                    }
                    Some(FromBrowser::Abort(reason)) => return reason,
                    Some(FromBrowser::Close) | None => return format!("closed by {}", attached.username),
                },
                inbound = inbox.recv() => match inbound {
                    Some(Inbound::Frame { agent, frame }) if agent == attached.peer_id => {
                        match frame.body {
                            Some(remote_frame::Body::Sealed(bytes)) => {
                                match channel.open(frame.counter, &bytes) {
                                    Ok(RemoteData { body: Some(remote_data::Body::Output(output)) }) => {
                                        totals.bytes_out += output.len() as i64;
                                        self.record(attached, recording, opened_at, &output).await;
                                        if to_browser.send(ToBrowser::Output(output)).await.is_err() {
                                            return format!("closed by {}", attached.username);
                                        }
                                        if !attached.is_terminal() {
                                            delivered += 1;
                                            if delivered - told >= DELIVERED_EVERY {
                                                told = delivered;
                                                let frame = sealed(attached, channel, &sealing::delivered(delivered));
                                                self.to_agent(&attached.peer_id, frame).await;
                                            }
                                        }
                                    }
                                    Ok(RemoteData { body: Some(remote_data::Body::Close(reason)) }) => {
                                        return bounded(&reason, "the agent closed the session");
                                    }
                                    Ok(_) => {}
                                    Err(error) => {
                                        tracing::warn!(session = %attached.id, %error, "refused a remote session frame");
                                        return format!("closed: {error}");
                                    }
                                }
                            }
                            Some(remote_frame::Body::Closed(closed)) => {
                                return bounded(&closed.reason, "the agent closed the session");
                            }
                            _ => {}
                        }
                    }
                    Some(Inbound::Frame { .. }) => {}
                    Some(Inbound::Close { by }) => return format!("closed by {by}"),
                    None => return "the central server is stopping".into(),
                },
                _ = flush.tick() => self.flush(attached, recording).await,
                _ = seen.tick() => {
                    self.seen(attached, totals).await;
                    if self.inner.registry.live(PeerRole::Agent, &attached.peer_id).is_some() {
                        agent_gone_since = None;
                    } else {
                        let since = *agent_gone_since.get_or_insert_with(Instant::now);
                        if since.elapsed() >= AGENT_GONE {
                            return "the agent disconnected from the central server".into();
                        }
                    }
                }
                () = sleep_until(idle_at) => {
                    return format!("closed after {} minutes without input", idle.as_secs() / 60);
                }
            }
        }
    }

    async fn record(
        &self,
        attached: &Attached,
        recording: &mut Recording,
        opened_at: Instant,
        output: &[u8],
    ) {
        if !recording.on {
            return;
        }
        if recording.pending.is_empty() {
            recording.pending_at_ms = opened_at.elapsed().as_millis() as i64;
        }
        recording.pending.extend_from_slice(output);
        if recording.pending.len() >= RECORD_BUFFER {
            self.flush(attached, recording).await;
        }
    }

    async fn flush(&self, attached: &Attached, recording: &mut Recording) {
        if recording.pending.is_empty() {
            return;
        }
        let output = std::mem::take(&mut recording.pending);
        let result = sqlx::query(
            "INSERT INTO remote_session_recordings (session_id, seq, at_ms, output)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(attached.id)
        .bind(recording.seq)
        .bind(recording.pending_at_ms)
        .bind(output)
        .execute(&self.inner.pool)
        .await;
        recording.seq += 1;
        if let Err(error) = result {
            tracing::error!(session = %attached.id, %error, "cannot write a remote session's recording");
        }
    }

    async fn seen(&self, attached: &Attached, totals: &Totals) {
        let result = sqlx::query(
            "UPDATE remote_sessions SET seen_at = now(), bytes_in = $2, bytes_out = $3
             WHERE id = $1 AND closed_at IS NULL",
        )
        .bind(attached.id)
        .bind(totals.bytes_in)
        .bind(totals.bytes_out)
        .execute(&self.inner.pool)
        .await;
        if let Err(error) = result {
            tracing::warn!(session = %attached.id, %error, "cannot keep a remote session marked live");
        }
    }

    async fn finish(
        &self,
        attached: &Attached,
        reason: &str,
        lasted: Duration,
        totals: &Totals,
        recording: &mut Recording,
    ) -> Result<(), sqlx::Error> {
        self.flush(attached, recording).await;
        let mut tx = self.inner.pool.begin().await?;
        let closed = sqlx::query(
            "UPDATE remote_sessions SET closed_at = now(), close_reason = $2, seen_at = now(),
                    bytes_in = $3, bytes_out = $4
             WHERE id = $1 AND closed_at IS NULL",
        )
        .bind(attached.id)
        .bind(reason)
        .bind(totals.bytes_in)
        .bind(totals.bytes_out)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if closed > 0 {
            audit(
                &mut tx,
                attached,
                "remote_session.close",
                json!({
                    "reason": reason,
                    "duration_seconds": lasted.as_secs(),
                    "bytes_in": totals.bytes_in,
                    "bytes_out": totals.bytes_out,
                }),
            )
            .await?;
        }
        tx.commit().await?;
        tracing::info!(session = %attached.id, agent_id = %attached.peer_id, kind = %attached.kind, %reason, seconds = lasted.as_secs(), "remote session closed");
        Ok(())
    }
}

/// An audit entry of `attached`, as its user.
async fn audit(
    tx: &mut sqlx::PgConnection,
    attached: &Attached,
    action: &str,
    mut details: serde_json::Value,
) -> Result<(), sqlx::Error> {
    if let Some(details) = details.as_object_mut() {
        details.insert("session_id".into(), json!(attached.id));
        details.insert("hostname".into(), json!(attached.hostname));
        details.insert("peer_id".into(), json!(attached.peer_id));
        details.insert("kind".into(), json!(attached.kind));
    }
    let actor = attached
        .requested_by
        .map(|id| id.to_string())
        .unwrap_or_else(|| attached.username.clone());
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action,
            target: Some(("server", &attached.server_id.to_string())),
            details,
        },
    )
    .await
}

fn sealed(attached: &Attached, channel: &mut Channel, data: &RemoteData) -> RemoteFrame {
    let (counter, sealed) = channel.seal(data);
    RemoteFrame {
        session_id: attached.id.to_string(),
        body: Some(remote_frame::Body::Sealed(sealed)),
        counter,
    }
}

fn resize(cols: u32, rows: u32) -> RemoteData {
    RemoteData {
        body: Some(remote_data::Body::Resize(RemoteResize { cols, rows })),
    }
}

/// A reason from the agent, bounded and without control characters.
fn bounded(reason: &str, fallback: &str) -> String {
    let reason: String = reason
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect();
    if reason.is_empty() {
        fallback.to_owned()
    } else {
        reason
    }
}
