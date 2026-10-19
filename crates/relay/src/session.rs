//! One task per agent owns everything the relay sends upstream for it
//! (ADR-0027): live envelopes while its upstream stream works, its
//! `SampleBatch` envelopes to the buffer while it does not, and the
//! buffered ones first, oldest first, whenever a new stream opens.
//!
//! A session runs for a connected agent, or without one to replay the
//! records of an agent that left or of a previous run of the relay; such a
//! session closes its upstream stream once the records are sent.
//! [`Lanes`] keeps one session per agent: a new one stops the current one
//! and waits for it, so two never touch the same records.

// `Ended` carries the `Status` tonic returns by value.
#![allow(clippy::result_large_err)]

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::Envelope;
use monitorflow_common::via::Via;
use tokio::sync::mpsc;
use tokio::sync::{oneshot, watch, OwnedMutexGuard};
use tokio::time::{sleep_until, timeout, Instant};
use tonic::{Code, Status, Streaming};

use crate::forward::{downstream_status, Forwarder};

/// Time a replay-only stream gets to end cleanly once everything was
/// handed to it, so its last batches are not cut off.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) type Down = mpsc::Sender<Result<Envelope, Status>>;

/// How an upstream stream ended, or why it could not open.
#[derive(Debug)]
pub(crate) enum Ended {
    /// The relay's link failed or the server went away: buffer and retry.
    Link,
    /// A status the agent gets, which ends its session.
    Status(Status),
}

/// Link errors and `UNAVAILABLE` are worth buffering through; any other
/// status goes to the agent as ADR-0025 says.
pub(crate) fn classify(agent_id: &str, status: Status) -> Ended {
    let status = downstream_status(agent_id, status);
    if status.code() == Code::Unavailable {
        Ended::Link
    } else {
        Ended::Status(status)
    }
}

/// Refusals that retrying will not change: the agent's records are
/// deleted, since the server would refuse them again.
pub(crate) fn is_permanent(status: &Status) -> bool {
    matches!(
        status.code(),
        Code::PermissionDenied
            | Code::Unauthenticated
            | Code::InvalidArgument
            | Code::FailedPrecondition
    )
}

/// An open upstream stream for one agent.
pub(crate) struct Upstream {
    pub tx: mpsc::Sender<Envelope>,
    pub ended: oneshot::Receiver<Ended>,
}

impl Upstream {
    /// Passes what the upstream sends to `down`, if an agent is connected,
    /// and reports how the stream ended.
    pub fn start(
        agent_id: String,
        tx: mpsc::Sender<Envelope>,
        mut inbound: Streaming<Envelope>,
        down: Option<Down>,
    ) -> Self {
        let (end, ended) = oneshot::channel();
        tokio::spawn(async move {
            let how = loop {
                match inbound.message().await {
                    Ok(Some(envelope)) => match &down {
                        Some(down) if down.send(Ok(envelope)).await.is_err() => return,
                        _ => {}
                    },
                    Ok(None) => break Ended::Link,
                    Err(status) => break classify(&agent_id, status),
                }
            };
            let _ = end.send(how);
        });
        Self { tx, ended }
    }
}

struct Lane {
    lock: Arc<tokio::sync::Mutex<()>>,
    cancel: watch::Sender<bool>,
}

/// The session slot of each agent.
#[derive(Default)]
pub(crate) struct Lanes(Mutex<HashMap<String, Lane>>);

pub(crate) type Held = (OwnedMutexGuard<()>, watch::Receiver<bool>);

impl Lanes {
    /// Replaces the receiver the current holder watches with a new one,
    /// after telling the holder to stop.
    fn claim(&self, agent_id: &str) -> (Arc<tokio::sync::Mutex<()>>, watch::Receiver<bool>) {
        let mut lanes = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let lane = lanes.entry(agent_id.to_owned()).or_insert_with(|| Lane {
            lock: Arc::default(),
            cancel: watch::channel(false).0,
        });
        let (cancel, cancelled) = watch::channel(false);
        std::mem::replace(&mut lane.cancel, cancel).send_replace(true);
        (lane.lock.clone(), cancelled)
    }

    /// Stops the agent's current session, if any, and waits for it.
    pub async fn take(&self, agent_id: &str) -> Held {
        let (lock, cancelled) = self.claim(agent_id);
        (lock.lock_owned().await, cancelled)
    }

    /// The agent's slot, only if no session holds it.
    pub fn try_take(&self, agent_id: &str) -> Option<Held> {
        let mut lanes = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let lane = lanes.entry(agent_id.to_owned()).or_insert_with(|| Lane {
            lock: Arc::default(),
            cancel: watch::channel(false).0,
        });
        let guard = lane.lock.clone().try_lock_owned().ok()?;
        let (cancel, cancelled) = watch::channel(false);
        lane.cancel = cancel;
        Some((guard, cancelled))
    }
}

/// The connected agent's half of a session.
pub(crate) struct AgentSide {
    pub inbound: Streaming<Envelope>,
    pub down: Down,
    /// Envelopes not above this agent sequence are dropped, as the server
    /// would.
    pub last_sequence: u64,
}

enum Event {
    Cancelled,
    Agent(FromAgent),
    Lost(Ended),
    /// Room for one envelope on the upstream stream; false if it closed.
    Room(bool),
    Opened(Result<Upstream, Ended>),
    Retry,
}

enum FromAgent {
    Envelope(Envelope),
    Gone,
}

#[derive(Debug, Default)]
struct Counts {
    forwarded: u64,
    buffered: u64,
    replayed: u64,
    /// Envelopes other than batches while there was no upstream, stale
    /// sequences and batches the buffer could not keep.
    dropped: u64,
}

pub(crate) struct Session {
    pub forwarder: Forwarder,
    pub agent_id: String,
    pub hello: Envelope,
    /// The stream's path (ADR-0028), sent on every upstream stream opened
    /// for it, replays included.
    pub via: Via,
    pub held: Held,
    /// The relay's sequence on the current upstream stream; `Hello` is 1.
    pub sequence: u64,
}

impl Session {
    /// Runs until the agent leaves, the upstream ends it, a newer session
    /// takes over or, without an agent, the records are sent.
    pub async fn run(
        mut self,
        mut agent: Option<AgentSide>,
        upstream: Option<Upstream>,
        mut retry_at: Instant,
    ) {
        let buffer = self.forwarder.buffer().clone();
        let settings = self.forwarder.settings().clone();
        let id = self.agent_id.clone();
        let mut backoff = settings.backoff.clone();
        let mut up_flag = self.forwarder.upstream_up();
        let (mut up_tx, mut up_ended) = match upstream {
            Some(up) => (Some(up.tx), Some(up.ended)),
            None => (None, None),
        };
        let mut draining = up_tx.is_some() && buffer.has_pending(&id).await.unwrap_or(false);
        let mut replay = VecDeque::new();
        let mut opening: Option<tokio::task::JoinHandle<Result<Upstream, Ended>>> = None;
        let mut counts = Counts::default();
        let mut cancelled = self.held.1.clone();
        let reason = loop {
            // A lost stream goes first, so the batches after it are buffered
            // rather than sent into it.
            let event = tokio::select! {
                biased;
                _ = cancelled.wait_for(|cancelled| *cancelled) => Event::Cancelled,
                ended = until_ended(&mut up_ended) => Event::Lost(ended),
                event = from_agent(&mut agent) => Event::Agent(event),
                room = has_room(&up_tx), if draining => Event::Room(room),
                opened = join(&mut opening) => Event::Opened(opened),
                () = wait_retry(&mut up_flag, retry_at), if up_tx.is_none() && opening.is_none() => Event::Retry,
            };
            // What ended the upstream stream, or failed to open one.
            let ended = match event {
                Event::Cancelled => {
                    if let Some(side) = &agent {
                        let _ = side.down.try_send(Err(Status::aborted(
                            "replaced by a newer connection with the same ID",
                        )));
                    }
                    break "replaced by a newer session";
                }
                Event::Agent(FromAgent::Gone) => break "agent closed",
                Event::Agent(FromAgent::Envelope(envelope)) => {
                    let side = agent.as_mut().expect("an event came from it");
                    if envelope.sequence <= side.last_sequence {
                        counts.dropped += 1;
                        continue;
                    }
                    side.last_sequence = envelope.sequence;
                    let batch = match &envelope.payload {
                        Some(Payload::Samples(_)) => true,
                        Some(Payload::Hello(_)) => continue,
                        _ => false,
                    };
                    let envelope = match &up_tx {
                        Some(tx) if !(batch && draining) => {
                            self.sequence += 1;
                            let stamped = Envelope {
                                sequence: self.sequence,
                                ..envelope
                            };
                            match tx.send(stamped).await {
                                Ok(()) => {
                                    counts.forwarded += 1;
                                    continue;
                                }
                                // The stream is gone; `up_ended` says so next.
                                Err(failed) => failed.0,
                            }
                        }
                        _ => envelope,
                    };
                    if batch && self.append(&envelope).await {
                        counts.buffered += 1;
                    } else {
                        counts.dropped += 1;
                    }
                    continue;
                }
                Event::Lost(ended) => ended,
                // The stream closed; `up_ended` says why next.
                Event::Room(false) => continue,
                Event::Room(true) => {
                    if replay.is_empty() {
                        if let Err(error) = buffer.commit(&id).await {
                            tracing::error!(agent_id = %id, %error, "cannot delete sent buffer segments");
                        }
                        match buffer.read(&id, settings.replay_batch).await {
                            Ok(batch) => replay.extend(batch),
                            Err(error) => {
                                tracing::error!(agent_id = %id, %error, "cannot read the buffer; sending live data")
                            }
                        }
                    }
                    let Some(envelope) = replay.pop_front() else {
                        draining = false;
                        if counts.replayed > 0 {
                            tracing::info!(agent_id = %id, replayed = counts.replayed, "buffered batches sent");
                        }
                        if agent.is_none() {
                            self.close(up_tx.take(), up_ended.take()).await;
                            break "replayed";
                        }
                        continue;
                    };
                    self.sequence += 1;
                    let stamped = Envelope {
                        sequence: self.sequence,
                        ..envelope
                    };
                    // This task is the only sender and saw room.
                    let tx = up_tx.as_ref().expect("room on an open stream");
                    if tx.try_send(stamped).is_ok() {
                        counts.replayed += 1;
                    }
                    continue;
                }
                Event::Opened(Ok(up)) => {
                    opening = None;
                    tracing::info!(agent_id = %id, "upstream stream opened");
                    backoff.reset();
                    up_tx = Some(up.tx);
                    up_ended = Some(up.ended);
                    draining = buffer.has_pending(&id).await.unwrap_or(false);
                    continue;
                }
                Event::Opened(Err(ended)) => {
                    opening = None;
                    ended
                }
                Event::Retry => {
                    let (forwarder, hello) = (self.forwarder.clone(), self.hello.clone());
                    let via = self.via.clone();
                    let down = agent.as_ref().map(|side| side.down.clone());
                    self.sequence = 1;
                    opening = Some(tokio::spawn(async move {
                        forwarder.open(&hello, &via, down).await
                    }));
                    continue;
                }
            };
            up_tx = None;
            up_ended = None;
            replay.clear();
            if let Err(error) = buffer.rewind(&id).await {
                tracing::error!(agent_id = %id, %error, "cannot rewind the buffer");
            }
            match ended {
                Ended::Link if agent.is_none() => break "upstream unavailable",
                Ended::Link => {
                    let delay = backoff.next_delay();
                    tracing::warn!(agent_id = %id, retry_in = ?delay, "no upstream stream; buffering");
                    retry_at = Instant::now() + delay;
                }
                Ended::Status(status) => {
                    if is_permanent(&status) {
                        self.discard().await;
                    }
                    if let Some(side) = &agent {
                        let _ = side.down.try_send(Err(status));
                    }
                    break "ended by the upstream";
                }
            }
        };
        tracing::info!(
            agent_id = %id,
            forwarded = counts.forwarded,
            buffered = counts.buffered,
            replayed = counts.replayed,
            dropped = counts.dropped,
            reason,
            "stream closed"
        );
    }

    async fn append(&self, envelope: &Envelope) -> bool {
        match self
            .forwarder
            .buffer()
            .append(&self.agent_id, envelope)
            .await
        {
            Ok(kept) => kept,
            Err(error) => {
                tracing::error!(agent_id = %self.agent_id, %error, "cannot buffer a batch; dropping it");
                false
            }
        }
    }

    async fn discard(&self) {
        match self.forwarder.buffer().discard(&self.agent_id).await {
            Ok(0) => {}
            Ok(records) => {
                tracing::warn!(agent_id = %self.agent_id, records, "the upstream refuses the agent; its buffered batches are dropped");
            }
            Err(error) => {
                tracing::error!(agent_id = %self.agent_id, %error, "cannot drop refused records")
            }
        }
    }

    /// Ends a replay-only stream once the upstream read it all.
    async fn close(
        &self,
        tx: Option<mpsc::Sender<Envelope>>,
        ended: Option<oneshot::Receiver<Ended>>,
    ) {
        drop(tx);
        if let Some(ended) = ended {
            if timeout(CLOSE_TIMEOUT, ended).await.is_err() {
                tracing::warn!(agent_id = %self.agent_id, "the upstream did not close the replay stream in time");
            }
        }
    }
}

async fn from_agent(agent: &mut Option<AgentSide>) -> FromAgent {
    let Some(side) = agent else {
        return std::future::pending().await;
    };
    tokio::select! {
        message = side.inbound.message() => match message {
            Ok(Some(envelope)) => FromAgent::Envelope(envelope),
            Ok(None) => FromAgent::Gone,
            Err(status) => {
                tracing::debug!(%status, "agent stream failed");
                FromAgent::Gone
            }
        },
        () = side.down.closed() => FromAgent::Gone,
    }
}

async fn until_ended(ended: &mut Option<oneshot::Receiver<Ended>>) -> Ended {
    match ended {
        Some(ended) => ended.await.unwrap_or(Ended::Link),
        None => std::future::pending().await,
    }
}

/// Waits for room for one envelope on the upstream stream; false if it
/// closed.
async fn has_room(tx: &Option<mpsc::Sender<Envelope>>) -> bool {
    match tx {
        Some(tx) => tx.reserve().await.is_ok(),
        None => std::future::pending().await,
    }
}

async fn join(
    opening: &mut Option<tokio::task::JoinHandle<Result<Upstream, Ended>>>,
) -> Result<Upstream, Ended> {
    match opening {
        Some(task) => task.await.unwrap_or(Err(Ended::Link)),
        None => std::future::pending().await,
    }
}

/// Resolves at `at` once the relay's own stream is up.
async fn wait_retry(up: &mut watch::Receiver<bool>, at: Instant) {
    if up.wait_for(|up| *up).await.is_err() {
        // The relay is stopping.
        return std::future::pending().await;
    }
    sleep_until(at).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_failures_buffer_and_refusals_end_the_session() {
        assert!(matches!(
            classify("web-01", Status::unavailable("server shutting down")),
            Ended::Link
        ));
        let alert = std::io::Error::other("connection reset");
        assert!(matches!(
            classify("web-01", Status::from_error(Box::new(alert))),
            Ended::Link
        ));
        let Ended::Status(status) = classify("web-01", Status::aborted("replaced")) else {
            panic!("ABORTED goes to the agent");
        };
        assert_eq!(status.code(), Code::Aborted);
        assert!(!is_permanent(&status));
        assert!(is_permanent(&Status::permission_denied("not your site")));
    }

    #[tokio::test]
    async fn a_new_session_stops_the_current_one_and_waits_for_it() {
        let lanes = Arc::new(Lanes::default());
        let (guard, mut cancelled) = lanes.take("web-01").await;
        assert!(lanes.try_take("web-01").is_none());
        assert!(lanes.try_take("web-02").is_some());
        let next = tokio::spawn({
            let lanes = lanes.clone();
            async move { lanes.take("web-01").await }
        });
        cancelled.wait_for(|c| *c).await.unwrap();
        assert!(!next.is_finished());
        drop(guard);
        let (_guard, cancelled) = next.await.unwrap();
        assert!(!*cancelled.borrow());
    }
}
