//! The engine task (ADR-0017): evaluates the series of each ingested batch
//! and writes incidents, their events and alert states.
//!
//! Agent streams call [`Alerts::submit`], which never waits: when the
//! queue is full the batch is dropped and counted. One task evaluates
//! batches in order, so no two writes for a series ever race. Thresholds
//! are cached per agent and reloaded when their revision moves.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio::time::{interval, MissedTickBehavior};
use uuid::Uuid;

use super::rules::Rule;
use super::state::{SeriesState, Transition};
use super::store::{AlertStore, IncidentStep, SeriesKey, ServerRules, Step};
use crate::ingest::Series;

#[derive(Debug, Clone)]
pub struct EngineSettings {
    /// Batches queued towards the engine; more are dropped.
    pub queue_capacity: usize,
    /// A longer silence between two samples restarts pending timers.
    pub max_gap: Duration,
    /// Time between two threshold revision checks.
    pub poll_interval: Duration,
    /// How often the counters are logged, when they changed.
    pub report_interval: Duration,
}

impl Default for EngineSettings {
    fn default() -> Self {
        Self {
            queue_capacity: 10_000,
            max_gap: Duration::from_secs(10 * 60),
            poll_interval: Duration::from_secs(10),
            report_interval: Duration::from_secs(60),
        }
    }
}

/// A change of an incident, for whoever must hear about it.
#[derive(Debug, Clone, Copy)]
pub struct Notification<'a> {
    /// The agent ID of the server.
    pub peer: &'a str,
    pub key: &'a SeriesKey,
    pub transition: Transition,
    pub value: Option<f64>,
    /// Sample time, Unix milliseconds.
    pub at: i64,
    /// `None` when the incident could not be written.
    pub incident: Option<Uuid>,
}

/// Where incident changes go besides the database. Called on the engine
/// task: an implementation that does I/O must queue and return.
// TODO(notifications): mail, webhook and chat notifiers.
pub trait Notifier: Send + Sync + 'static {
    fn notify(&self, notification: &Notification<'_>);
}

/// Logs every change; the only notifier for now.
#[derive(Debug, Clone, Copy, Default)]
pub struct LogNotifier;

impl Notifier for LogNotifier {
    fn notify(&self, n: &Notification<'_>) {
        let severity = n.transition.severity().map(|s| s.as_str());
        tracing::warn!(
            agent_id = n.peer,
            metric = %n.key.metric,
            instance = %n.key.instance,
            change = n.transition.kind(),
            severity,
            value = n.value,
            incident = ?n.incident,
            "incident changed"
        );
    }
}

/// Totals since the engine started.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub batches: u64,
    /// Batches dropped because the queue was full or closed.
    pub dropped: u64,
    /// Samples not newer than the last one of their series.
    pub out_of_order: u64,
    pub transitions: u64,
    pub db_errors: u64,
}

#[derive(Debug, Default)]
struct Counters {
    batches: AtomicU64,
    dropped: AtomicU64,
    out_of_order: AtomicU64,
    transitions: AtomicU64,
    db_errors: AtomicU64,
}

impl Counters {
    fn add(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> Stats {
        let get = |c: &AtomicU64| c.load(Ordering::Relaxed);
        Stats {
            batches: get(&self.batches),
            dropped: get(&self.dropped),
            out_of_order: get(&self.out_of_order),
            transitions: get(&self.transitions),
            db_errors: get(&self.db_errors),
        }
    }
}

struct Point {
    metric: String,
    instance: String,
    at: i64,
    value: f64,
}

enum Message {
    Batch { peer: String, points: Vec<Point> },
    Flush(oneshot::Sender<()>),
}

/// Producer handle of the engine; clones share the queue.
#[derive(Clone)]
pub struct Alerts {
    tx: mpsc::Sender<Message>,
    counters: Arc<Counters>,
}

impl Alerts {
    /// Reloads the stored state, then starts the engine task, which runs
    /// until every handle is dropped.
    pub async fn start(
        store: AlertStore,
        settings: EngineSettings,
        notifier: Arc<dyn Notifier>,
    ) -> Result<Self, sqlx::Error> {
        let series: HashMap<_, _> = store
            .load_states()
            .await?
            .into_iter()
            .map(|s| {
                let tracked = Tracked {
                    state: s.state,
                    incident: s.incident,
                };
                (s.key, tracked)
            })
            .collect();
        tracing::info!(series = series.len(), "threshold engine state loaded");
        let (tx, rx) = mpsc::channel(settings.queue_capacity);
        let counters = Arc::new(Counters::default());
        let engine = Engine {
            settings,
            rules: HashMap::new(),
            series,
            out: Output {
                store,
                notifier,
                counters: counters.clone(),
            },
        };
        tokio::spawn(engine.run(rx));
        Ok(Self { tx, counters })
    }

    /// Queues the series of one stream's batch for evaluation. Returns
    /// `false` when the batch was dropped.
    pub fn submit(&self, series: &[Series]) -> bool {
        let mut peer = None;
        let points: Vec<Point> = series
            .iter()
            .filter_map(|s| {
                let (mut metric, mut instance) = (None, "");
                for (name, value) in &s.labels {
                    match name.as_str() {
                        "__name__" => metric = Some(value),
                        "instance" => instance = value,
                        "peer" => peer = peer.or(Some(value)),
                        _ => {}
                    }
                }
                Some(Point {
                    metric: metric?.clone(),
                    instance: instance.to_owned(),
                    at: s.timestamp_ms,
                    value: s.value,
                })
            })
            .collect();
        let Some(peer) = peer.filter(|_| !points.is_empty()) else {
            return true;
        };
        let batch = Message::Batch {
            peer: peer.clone(),
            points,
        };
        if self.tx.try_send(batch).is_err() {
            Counters::add(&self.counters.dropped);
            return false;
        }
        true
    }

    /// Waits until every batch submitted before is evaluated and written.
    pub async fn flush(&self) {
        let (done, wait) = oneshot::channel();
        if self.tx.send(Message::Flush(done)).await.is_ok() {
            let _ = wait.await;
        }
    }

    pub fn stats(&self) -> Stats {
        self.counters.snapshot()
    }
}

#[derive(Debug, Default)]
struct Tracked {
    state: SeriesState,
    /// The open incident's ID and peak value.
    incident: Option<(Uuid, f64)>,
}

// TODO(host-down): an agent that stops reporting is not an incident: its
// series keep their open incidents (ADR-0017, no data). A "host down" rule
// on the registry's liveness will open one.
struct Engine {
    settings: EngineSettings,
    /// Thresholds per agent ID; `None` for peers without a `servers` row.
    rules: HashMap<String, Option<Arc<ServerRules>>>,
    series: HashMap<SeriesKey, Tracked>,
    out: Output,
}

impl Engine {
    async fn run(mut self, mut rx: mpsc::Receiver<Message>) {
        let mut poll = interval(self.settings.poll_interval);
        poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut report = interval(self.settings.report_interval);
        report.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut reported = Stats::default();
        loop {
            tokio::select! {
                message = rx.recv() => match message {
                    Some(Message::Batch { peer, points }) => self.batch(&peer, points).await,
                    Some(Message::Flush(done)) => {
                        let _ = done.send(());
                    }
                    None => return,
                },
                _ = poll.tick() => self.poll().await,
                _ = report.tick() => {
                    let stats = self.out.counters.snapshot();
                    if stats != reported {
                        if stats.dropped > reported.dropped || stats.db_errors > reported.db_errors {
                            tracing::warn!(?stats, "threshold engine lost batches or writes");
                        } else {
                            tracing::info!(?stats, "threshold engine");
                        }
                        reported = stats;
                    }
                }
            }
        }
    }

    async fn batch(&mut self, peer: &str, points: Vec<Point>) {
        Counters::add(&self.out.counters.batches);
        let Some(server) = self.server(peer).await else {
            return;
        };
        let max_gap = i64::try_from(self.settings.max_gap.as_millis()).unwrap_or(i64::MAX);
        for point in points {
            let Some(rule) = server.rules.lookup(&point.metric, &point.instance) else {
                continue;
            };
            let key = SeriesKey {
                server_id: server.server_id,
                metric: point.metric,
                instance: point.instance,
            };
            let tracked = self.series.entry(key.clone()).or_default();
            let Some(evaluation) = tracked.state.evaluate(rule, point.at, point.value, max_gap)
            else {
                Counters::add(&self.out.counters.out_of_order);
                continue;
            };
            let change = Change {
                peer,
                at: point.at,
                value: Some(point.value),
                transition: evaluation.transition,
                timers_changed: evaluation.timers_changed,
                reason: None,
            };
            self.out.write(&key, tracked, Some(rule), change).await;
        }
    }

    /// The cached thresholds of `peer`, loaded on first use.
    async fn server(&mut self, peer: &str) -> Option<Arc<ServerRules>> {
        if let Some(entry) = self.rules.get(peer) {
            return entry.clone();
        }
        match self.out.store.load_rules(peer).await {
            Ok(entry) => {
                let entry = entry.map(Arc::new);
                if let Some(server) = &entry {
                    // States reloaded at start may belong to removed thresholds.
                    self.drop_unmatched(peer, server).await;
                }
                self.rules.insert(peer.to_owned(), entry.clone());
                entry
            }
            Err(error) => {
                Counters::add(&self.out.counters.db_errors);
                tracing::warn!(agent_id = peer, %error, "could not load thresholds; batch skipped");
                None
            }
        }
    }

    /// Reloads the thresholds of servers whose revision moved, and forgets
    /// peers without a row so their next batch looks again.
    async fn poll(&mut self) {
        self.rules.retain(|_, entry| entry.is_some());
        let ids: Vec<Uuid> = self.rules.values().flatten().map(|s| s.server_id).collect();
        if ids.is_empty() {
            return;
        }
        let current: HashMap<Uuid, u64> = match self.out.store.revisions(&ids).await {
            Ok(revisions) => revisions.into_iter().collect(),
            Err(error) => {
                tracing::warn!(%error, "could not check threshold revisions; will retry");
                return;
            }
        };
        let changed: Vec<(String, Uuid)> = self
            .rules
            .iter()
            .filter_map(|(peer, entry)| {
                let server = entry.as_ref()?;
                (current.get(&server.server_id) != Some(&server.revision))
                    .then(|| (peer.clone(), server.server_id))
            })
            .collect();
        for (peer, server_id) in changed {
            match self.out.store.load_rules(&peer).await {
                Ok(Some(server)) => {
                    tracing::info!(agent_id = %peer, revision = server.revision, "thresholds reloaded");
                    self.drop_unmatched(&peer, &server).await;
                    self.rules.insert(peer, Some(Arc::new(server)));
                }
                Ok(None) => {
                    // The server row is gone, and its incidents with it.
                    self.rules.remove(&peer);
                    self.series.retain(|key, _| key.server_id != server_id);
                }
                Err(error) => {
                    tracing::warn!(agent_id = %peer, %error, "could not reload thresholds; will retry");
                }
            }
        }
    }

    /// Resolves and forgets the series of `server` no threshold watches
    /// any more.
    async fn drop_unmatched(&mut self, peer: &str, server: &ServerRules) {
        let gone: Vec<SeriesKey> = self
            .series
            .keys()
            .filter(|k| {
                k.server_id == server.server_id
                    && server.rules.lookup(&k.metric, &k.instance).is_none()
            })
            .cloned()
            .collect();
        let now = chrono::Utc::now().timestamp_millis();
        for key in gone {
            let Some(mut tracked) = self.series.remove(&key) else {
                continue;
            };
            let pending = !tracked.state.is_ok();
            let change = Change {
                peer,
                at: now,
                value: None,
                transition: tracked.state.clear(),
                timers_changed: pending,
                reason: Some("threshold_removed"),
            };
            self.out.write(&key, &mut tracked, None, change).await;
            if tracked.incident.is_some() {
                // Not resolved in the database: the next reload tries again.
                self.series.insert(key, tracked);
            }
        }
    }
}

/// What one evaluation changed.
struct Change<'a> {
    peer: &'a str,
    at: i64,
    value: Option<f64>,
    transition: Option<Transition>,
    timers_changed: bool,
    reason: Option<&'static str>,
}

/// The engine's writes, apart from its maps so both can be borrowed.
struct Output {
    store: AlertStore,
    notifier: Arc<dyn Notifier>,
    counters: Arc<Counters>,
}

impl Output {
    /// Brings the database in line with `tracked`. A failure is logged and
    /// left in memory: an incident that could not be opened or resolved is
    /// tried again at the next sample.
    async fn write(
        &self,
        key: &SeriesKey,
        tracked: &mut Tracked,
        rule: Option<&Rule>,
        change: Change<'_>,
    ) {
        let mut event = change.transition;
        let incident = match (tracked.state.firing, tracked.incident, rule) {
            (Some(severity), None, Some(rule)) => {
                event = event.or(Some(Transition::Opened(severity)));
                IncidentStep::Open { severity, rule }
            }
            (Some(severity), Some((id, peak)), Some(rule)) => {
                let peak = change
                    .value
                    .map_or(peak, |v| rule.comparison.worst(peak, v));
                tracked.incident = Some((id, peak));
                IncidentStep::Update { id, severity, peak }
            }
            (None, Some((id, _)), _) => IncidentStep::Resolve { id },
            _ => IncidentStep::None,
        };
        let save_state = change.transition.is_some() || change.timers_changed;
        if incident != IncidentStep::None || save_state {
            let step = Step {
                key,
                at: change.at,
                value: change.value,
                incident,
                transition: event,
                reason: change.reason,
                state: save_state.then_some(&tracked.state),
            };
            match self.store.apply(&step).await {
                Ok(opened) => match incident {
                    IncidentStep::Open { .. } => tracked.incident = opened,
                    IncidentStep::Resolve { .. } => tracked.incident = None,
                    _ => {}
                },
                Err(error) => {
                    Counters::add(&self.counters.db_errors);
                    tracing::warn!(agent_id = change.peer, metric = %key.metric, instance = %key.instance, %error, "could not record the alert state; will retry");
                }
            }
        }
        if let Some(transition) = change.transition {
            Counters::add(&self.counters.transitions);
            self.notifier.notify(&Notification {
                peer: change.peer,
                key,
                transition,
                value: change.value,
                at: change.at,
                incident: tracked.incident.map(|(id, _)| id).or(match incident {
                    IncidentStep::Resolve { id } => Some(id),
                    _ => None,
                }),
            });
        }
    }
}
