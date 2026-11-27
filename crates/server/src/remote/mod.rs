//! Remote terminals and desktops (ADR-0190): the central server's half.
//!
//! A person's browser opens a WebSocket to whichever instance its request
//! reaches, and the agent's stream is held by whichever instance it
//! reached (ADR-0089). The instance holding the WebSocket runs the session
//! ([`RemoteHub::run`]): it makes the session's key, queues the signed
//! `remote_session` command, checks the agent's answer, and seals and opens
//! every frame. Frames between it and the agent's stream go straight
//! through the registry when one instance holds both, and otherwise as
//! PostgreSQL notifications on [`CHANNEL`]: sealed frames only, so a
//! notification carries nothing a relay could not already see.
//!
//! The instance running a session moves `remote_sessions.seen_at` on
//! while it lives; any instance closes a session whose instance stopped,
//! and one whose browser never came ([`RemoteHub::tidy`]).

pub mod desktop;
pub mod rdp;
mod session;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use base64::Engine;
use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::RemoteFrame;
use monitorflow_common::tls::PeerRole;
use prost::Message;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgListener;
use sqlx::PgPool;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::ca::CertificateAuthority;
use crate::commands::Commands;
use crate::grpc::Registry;

pub use session::{token_hash, AttachError, Attached, FromBrowser, ToBrowser};

/// Notifications between instances; payload: a [`Bridged`] in JSON.
pub const CHANNEL: &str = "monitorflow_remote";
/// How long the one-time token of a new session lasts.
pub const TOKEN_TTL: Duration = Duration::from_secs(60);
/// How long the agent has to open a session once the browser connected:
/// the command's lifetime, and a little more for its answer.
pub const COMMAND_TTL: Duration = Duration::from_secs(120);
/// Frames waiting for one session on this instance.
const INBOX: usize = 256;
/// How long a session-to-agent check is believed.
const KNOWN_FOR: Duration = Duration::from_secs(30);
/// A session whose instance did not move `seen_at` for this long is closed.
const STALE: Duration = Duration::from_secs(120);
/// Time between two passes of [`RemoteHub::tidy`].
const TIDY_EVERY: Duration = Duration::from_secs(60);

/// What reaches a session run on this instance.
#[derive(Debug)]
enum Inbound {
    /// From the stream of agent `agent`, as its stream's holder knows it.
    Frame { agent: String, frame: RemoteFrame },
    /// Someone closed the session from the API.
    Close { by: String },
}

/// A message between instances.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "lowercase")]
enum Bridged {
    /// From an agent's stream to the session's instance.
    Up { s: Uuid, a: String, f: String },
    /// From the session's instance to the agent's stream.
    Down { a: String, f: String },
    /// Close the session, asked on another instance.
    Close { s: Uuid, by: String },
}

fn encode(frame: &RemoteFrame) -> String {
    base64::engine::general_purpose::STANDARD.encode(frame.encode_to_vec())
}

fn decode(text: &str) -> Option<RemoteFrame> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(text)
        .ok()?;
    RemoteFrame::decode(bytes.as_slice()).ok()
}

struct Inner {
    pool: PgPool,
    registry: Registry,
    ca: Arc<CertificateAuthority>,
    commands: Option<Commands>,
    /// Sessions run on this instance.
    local: Mutex<HashMap<Uuid, mpsc::Sender<Inbound>>>,
    /// Sessions known to belong to an agent and be open, with when it was
    /// checked: frames for them from that agent are passed on.
    known: Mutex<HashMap<Uuid, (String, Instant)>>,
}

/// The remote sessions of this instance; clones share them.
#[derive(Clone)]
pub struct RemoteHub {
    inner: Arc<Inner>,
}

impl RemoteHub {
    /// A hub without its background tasks, for tests.
    pub fn new(
        pool: PgPool,
        registry: Registry,
        ca: Arc<CertificateAuthority>,
        commands: Option<Commands>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                pool,
                registry,
                ca,
                commands,
                local: Mutex::new(HashMap::new()),
                known: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// A hub listening to the other instances and tidying up, until the
    /// last clone is dropped and the process stops.
    pub fn spawn(
        pool: PgPool,
        registry: Registry,
        ca: Arc<CertificateAuthority>,
        commands: Option<Commands>,
    ) -> Self {
        let hub = Self::new(pool, registry, ca, commands);
        tokio::spawn(hub.clone().listen());
        let tidy = hub.clone();
        tokio::spawn(async move {
            let mut every = tokio::time::interval(TIDY_EVERY);
            loop {
                every.tick().await;
                if let Err(error) = tidy.tidy().await {
                    tracing::warn!(%error, "cannot tidy remote sessions");
                }
            }
        });
        hub
    }

    /// The database, for what a session writes besides its row.
    pub(crate) fn pool(&self) -> &PgPool {
        &self.inner.pool
    }

    fn local(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, mpsc::Sender<Inbound>>> {
        self.inner
            .local
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Sessions run on this instance now.
    pub fn local_sessions(&self) -> usize {
        self.local().len()
    }

    fn deliver(&self, session: Uuid, inbound: Inbound) -> bool {
        let inbox = self.local().get(&session).cloned();
        match inbox {
            Some(inbox) => {
                if inbox.try_send(inbound).is_err() {
                    tracing::warn!(%session, "a remote session is not keeping up; dropping a frame");
                }
                true
            }
            None => false,
        }
    }

    /// A frame from the stream of agent `agent`, held here. Passed to its
    /// session here, or to another instance when the session belongs to
    /// that agent and is open; dropped otherwise.
    pub async fn from_agent(&self, agent: &str, frame: RemoteFrame) {
        let Ok(session) = frame.session_id.parse::<Uuid>() else {
            return;
        };
        let inbox = self.local().get(&session).cloned();
        if let Some(inbox) = inbox {
            let inbound = Inbound::Frame {
                agent: agent.to_owned(),
                frame,
            };
            if inbox.try_send(inbound).is_err() {
                tracing::warn!(%session, "a remote session is not keeping up; dropping a frame");
            }
            return;
        }
        match self.belongs(session, agent).await {
            Ok(true) => {}
            Ok(false) => {
                tracing::debug!(%session, agent_id = %agent, "dropping a frame of no open session of this agent");
                return;
            }
            Err(error) => {
                tracing::warn!(%session, %error, "cannot check a remote session; dropping a frame");
                return;
            }
        }
        let message = Bridged::Up {
            s: session,
            a: agent.to_owned(),
            f: encode(&frame),
        };
        if let Err(error) = self.notify(&message).await {
            tracing::warn!(%session, %error, "cannot pass a remote session frame on");
        }
    }

    /// Whether `session` is open and on a server of agent `agent`.
    async fn belongs(&self, session: Uuid, agent: &str) -> Result<bool, sqlx::Error> {
        {
            let mut known = self
                .inner
                .known
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            known.retain(|_, (_, at)| at.elapsed() < KNOWN_FOR);
            if let Some((owner, _)) = known.get(&session) {
                return Ok(owner == agent);
            }
        }
        let owner: Option<String> = sqlx::query_scalar(
            "SELECT s.peer_id FROM remote_sessions r JOIN servers s ON s.id = r.server_id
             WHERE r.id = $1 AND r.closed_at IS NULL",
        )
        .bind(session)
        .fetch_optional(&self.inner.pool)
        .await?;
        let Some(owner) = owner else {
            return Ok(false);
        };
        let belongs = owner == agent;
        self.inner
            .known
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(session, (owner, Instant::now()));
        Ok(belongs)
    }

    /// Sends `frame` to agent `agent`: on its stream when held here, else
    /// to the instance that holds it.
    async fn to_agent(&self, agent: &str, frame: RemoteFrame) -> bool {
        let registry = &self.inner.registry;
        if registry.get(PeerRole::Agent, agent).is_some() {
            let envelope = registry.envelope(Payload::Remote(frame));
            return registry
                .send(PeerRole::Agent, agent, envelope)
                .await
                .is_ok();
        }
        let message = Bridged::Down {
            a: agent.to_owned(),
            f: encode(&frame),
        };
        match self.notify(&message).await {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(agent_id = %agent, %error, "cannot pass a remote session frame on");
                false
            }
        }
    }

    /// Asks the session to close, wherever it runs.
    pub async fn close(&self, session: Uuid, by: &str) -> Result<(), sqlx::Error> {
        if self.deliver(session, Inbound::Close { by: by.to_owned() }) {
            return Ok(());
        }
        self.notify(&Bridged::Close {
            s: session,
            by: by.to_owned(),
        })
        .await
    }

    async fn notify(&self, message: &Bridged) -> Result<(), sqlx::Error> {
        let payload = serde_json::to_string(message).expect("plain JSON");
        sqlx::query("SELECT pg_notify($1, $2)")
            .bind(CHANNEL)
            .bind(payload)
            .execute(&self.inner.pool)
            .await
            .map(|_| ())
    }

    /// Applies what other instances send, reconnecting when the listening
    /// connection drops.
    async fn listen(self) {
        loop {
            let mut listener = match PgListener::connect_with(&self.inner.pool).await {
                Ok(listener) => listener,
                Err(error) => {
                    tracing::warn!(%error, "cannot listen for remote session frames; retrying");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };
            if let Err(error) = listener.listen(CHANNEL).await {
                tracing::warn!(%error, "cannot listen for remote session frames; retrying");
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
            loop {
                match listener.recv().await {
                    Ok(notification) => self.bridged(notification.payload()).await,
                    Err(error) => {
                        tracing::warn!(%error, "lost the remote session listener; reconnecting");
                        break;
                    }
                }
            }
        }
    }

    async fn bridged(&self, payload: &str) {
        let Ok(message) = serde_json::from_str::<Bridged>(payload) else {
            tracing::debug!("ignoring a remote session notification that does not parse");
            return;
        };
        match message {
            Bridged::Up { s, a, f } => {
                if let Some(frame) = decode(&f) {
                    self.deliver(s, Inbound::Frame { agent: a, frame });
                }
            }
            Bridged::Down { a, f } => {
                let registry = &self.inner.registry;
                if registry.get(PeerRole::Agent, &a).is_none() {
                    return;
                }
                if let Some(frame) = decode(&f) {
                    let envelope = registry.envelope(Payload::Remote(frame));
                    let _ = registry.send(PeerRole::Agent, &a, envelope).await;
                }
            }
            Bridged::Close { s, by } => {
                self.deliver(s, Inbound::Close { by });
            }
        }
    }

    /// Closes sessions nobody runs any more: a browser that never came
    /// with its token, an instance that stopped while running one. Deletes
    /// recordings past their retention. Safe on every instance at once.
    pub async fn tidy(&self) -> Result<(), sqlx::Error> {
        let mut tx = self.inner.pool.begin().await?;
        let closed: Vec<(Uuid, Uuid, String, Option<Uuid>, String)> = sqlx::query_as(
            "UPDATE remote_sessions r SET closed_at = now(),
                    close_reason = CASE WHEN r.connected_at IS NULL
                        THEN 'the browser never connected'
                        ELSE 'the central server instance running it stopped' END
             FROM servers s
             WHERE s.id = r.server_id AND r.closed_at IS NULL
               AND ((r.connected_at IS NULL AND r.token_expires_at < now())
                    OR r.seen_at < now() - make_interval(secs => $1))
             RETURNING r.id, r.server_id, s.hostname, r.requested_by, r.close_reason",
        )
        .bind(STALE.as_secs_f64())
        .fetch_all(&mut *tx)
        .await?;
        for (id, server_id, hostname, requested_by, reason) in &closed {
            let actor = requested_by.map(|u| u.to_string());
            crate::api::audit::record(
                &mut *tx,
                crate::api::audit::Event {
                    actor_kind: "system",
                    actor_id: "remote-sessions",
                    action: "remote_session.close",
                    target: Some(("server", &server_id.to_string())),
                    details: serde_json::json!({
                        "session_id": id,
                        "hostname": hostname,
                        "requested_by": actor,
                        "reason": reason,
                    }),
                },
            )
            .await?;
        }
        sqlx::query(
            "DELETE FROM remote_session_recordings rec USING remote_sessions r
             WHERE rec.session_id = r.id
               AND r.created_at < now() - make_interval(days => (
                   SELECT retention_days FROM remote_access_settings))",
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "DELETE FROM remote_desktop_frames f USING remote_sessions r
             WHERE f.session_id = r.id
               AND r.created_at < now() - make_interval(days => (
                   SELECT retention_days FROM remote_access_settings))",
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        if !closed.is_empty() {
            tracing::info!(closed = closed.len(), "closed remote sessions nobody runs");
        }
        Ok(())
    }
}

#[cfg(test)]
mod bridge_tests {
    use super::*;

    #[test]
    fn bridged_messages_round_trip_and_stay_small() {
        let frame = RemoteFrame {
            session_id: Uuid::nil().to_string(),
            body: Some(monitorflow_common::proto::v1::remote_frame::Body::Sealed(
                vec![7; monitorflow_common::remote::MAX_CHUNK + 16],
            )),
            counter: u64::MAX,
        };
        let message = Bridged::Up {
            s: Uuid::nil(),
            a: "a".repeat(64),
            f: encode(&frame),
        };
        let text = serde_json::to_string(&message).unwrap();
        // PostgreSQL refuses notifications of 8000 bytes or more.
        assert!(text.len() < 8000, "{}", text.len());
        let back: Bridged = serde_json::from_str(&text).unwrap();
        assert_eq!(back, message);
        let Bridged::Up { f, .. } = back else {
            unreachable!()
        };
        assert_eq!(decode(&f).unwrap(), frame);
        assert!(decode("not base64").is_none());
    }
}
