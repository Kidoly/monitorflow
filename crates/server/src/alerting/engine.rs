//! The engine task (ADR-0017): evaluates the series of each ingested batch
//! and writes incidents, their events and alert states.
//!
//! Agent streams call [`Alerts::submit`], which never waits: when the
//! queue is full the batch is dropped and counted. One task evaluates
//! batches in order, so no two writes for a series ever race. Thresholds
//! are cached per agent and reloaded when their revision moves.
//!
//! The state of an agent's series is read from the database when its first
//! batch arrives, and forgotten when its stream opens or closes here
//! ([`Alerts::forget`]): with several server instances (ADR-0089), the
//! instance holding the stream evaluates it from what the previous one
//! wrote, never from an old copy.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio::time::{interval, MissedTickBehavior};
use uuid::Uuid;

use super::by_server::ByServer;
use super::history::{Answer, Backend, History, PastKey, Points, Request};
use super::rules::{Kind, Rule};
use super::state::{Evaluation, SeriesState, Transition};
use super::store::{AlertStore, IncidentStep, SeriesKey, ServerRules, Step};
use super::summary::{bucket_width, derive, Need, Summary};
use crate::dependencies::Holds;
use crate::ingest::Series;

#[derive(Debug, Clone)]
pub struct EngineSettings {
    /// Batches queued towards the engine; more are dropped.
    pub queue_capacity: usize,
    /// A longer silence between two samples restarts pending timers; for
    /// a check whose interval is longer than half of it, two intervals
    /// (ADR-0097).
    pub max_gap: Duration,
    /// Time between two threshold revision checks.
    pub poll_interval: Duration,
    /// How often the counters are logged, when they changed.
    pub report_interval: Duration,
    /// Where richer rules read history (ADR-0153); without it windows
    /// fill from live samples and comparisons with the past judge nothing.
    pub history: Option<Arc<dyn Backend>>,
    /// Time between two reads of what dependencies hold (ADR-0152).
    pub holds_interval: Duration,
}

impl std::fmt::Debug for dyn Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Backend")
    }
}

impl Default for EngineSettings {
    fn default() -> Self {
        Self {
            queue_capacity: 10_000,
            max_gap: Duration::from_secs(10 * 60),
            poll_interval: Duration::from_secs(10),
            report_interval: Duration::from_secs(60),
            history: None,
            holds_interval: Duration::from_secs(5),
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
// Email and webhook notifications do not go through here: database
// triggers on incident events fill their outbox (ADR-0060).
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
    /// History requests dropped because their queue was full.
    pub history_dropped: u64,
}

#[derive(Debug, Default)]
struct Counters {
    batches: AtomicU64,
    dropped: AtomicU64,
    out_of_order: AtomicU64,
    transitions: AtomicU64,
    db_errors: AtomicU64,
    history_dropped: AtomicU64,
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
            history_dropped: get(&self.history_dropped),
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
    /// Agents whose state to read again; never dropped, and taken before
    /// any batch queued after them.
    forget: mpsc::UnboundedSender<String>,
    counters: Arc<Counters>,
}

impl Alerts {
    /// Starts the engine task, which runs until every handle is dropped.
    /// Stored states are read per agent, at its first batch.
    pub async fn start(
        store: AlertStore,
        settings: EngineSettings,
        notifier: Arc<dyn Notifier>,
    ) -> Result<Self, sqlx::Error> {
        let (tx, rx) = mpsc::channel(settings.queue_capacity);
        let (forget, forget_rx) = mpsc::unbounded_channel();
        let (answers, answers_rx) = mpsc::unbounded_channel();
        let counters = Arc::new(Counters::default());
        let history = settings
            .history
            .clone()
            .map(|backend| History::start(backend, answers));
        let engine = Engine {
            settings,
            rules: HashMap::new(),
            series: ByServer::default(),
            feeds: ByServer::default(),
            loaded_at: HashMap::new(),
            pasts: ByServer::default(),
            asked: ByServer::default(),
            history,
            out: Output {
                store,
                notifier,
                counters: counters.clone(),
                holds: Holds::default(),
            },
        };
        tokio::spawn(engine.run(rx, forget_rx, answers_rx));
        Ok(Self {
            tx,
            forget,
            counters,
        })
    }

    /// Drops what the engine holds of the agent `peer`, whose stream
    /// opened or closed here: its next batch reads the stored state, which
    /// another instance may have moved meanwhile.
    pub fn forget(&self, peer: &str) {
        let _ = self.forget.send(peer.to_owned());
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

/// What the engine keeps of one series a richer rule reads.
#[derive(Debug)]
struct Feed {
    /// One per window read, shortest first.
    summaries: Vec<Summary>,
    /// The latest sample.
    last: Option<(i64, f64)>,
    /// When the latest sample arrived, on the server clock.
    arrived: i64,
}

impl Feed {
    /// The summary that answers a window of `window_ms` most precisely.
    fn summary(&self, window_ms: i64) -> Option<&Summary> {
        self.summaries.iter().find(|s| s.span_ms() >= window_ms)
    }

    fn new(need: &Need, arrived: i64) -> Self {
        Self {
            summaries: need.spans.iter().map(|&span| Summary::new(span)).collect(),
            last: None,
            arrived,
        }
    }
}

/// One series, whatever the kind of rule: server, metric, instance.
type FeedKey = (Uuid, String, String);

/// An hour or two of a comparison's past.
#[derive(Debug)]
struct Past {
    from_ms: i64,
    to_ms: i64,
    step_ms: i64,
    series: HashMap<String, Points>,
}

/// The step of a comparison's past, and how much is asked at once.
const PAST_STEP_MS: i64 = 300_000;
const PAST_CHUNK_MS: i64 = 2 * 3_600_000;
/// How far into a chunk the next one is asked for.
const PAST_RENEW_MS: i64 = 3_600_000;

impl Past {
    /// The value of `instance` at `at`: the point at or just before it.
    fn at(&self, instance: &str, at: i64) -> Option<f64> {
        let points = self.series.get(instance)?;
        let i = points.partition_point(|p| p.0 <= at);
        let (t, v) = *points.get(i.checked_sub(1)?)?;
        (at - t < self.step_ms).then_some(v)
    }

    fn covers(&self, at: i64) -> bool {
        (self.from_ms..self.to_ms).contains(&at)
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

// An agent that stops reporting keeps its series' open incidents
// (ADR-0017, no data); `offline` opens one for the silence (ADR-0072).
struct Engine {
    settings: EngineSettings,
    /// Thresholds per agent ID; `None` for peers without a `servers` row.
    rules: HashMap<String, Option<Arc<ServerRules>>>,
    series: ByServer<SeriesKey, Tracked>,
    /// Series richer rules read, by server; `None` when nothing does.
    feeds: ByServer<FeedKey, Option<Feed>>,
    /// When each cached server was loaded, on the server clock: no data
    /// waits one rule's time from there.
    loaded_at: HashMap<Uuid, i64>,
    pasts: ByServer<PastKey, Past>,
    /// Pasts asked for and not answered yet.
    asked: ByServer<PastKey, ()>,
    history: Option<History>,
    out: Output,
}

impl Engine {
    async fn run(
        mut self,
        mut rx: mpsc::Receiver<Message>,
        mut forget: mpsc::UnboundedReceiver<String>,
        mut answers: mpsc::UnboundedReceiver<Answer>,
    ) {
        let mut poll = interval(self.settings.poll_interval);
        poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut report = interval(self.settings.report_interval);
        report.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut holds = interval(self.settings.holds_interval);
        holds.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut reported = Stats::default();
        loop {
            tokio::select! {
                biased;
                Some(peer) = forget.recv() => self.forget(&peer),
                Some(answer) = answers.recv() => self.answer(answer),
                message = rx.recv() => match message {
                    Some(Message::Batch { peer, points }) => self.batch(&peer, points).await,
                    Some(Message::Flush(done)) => {
                        let _ = done.send(());
                    }
                    None => return,
                },
                _ = poll.tick() => {
                    self.poll().await;
                    self.sweep(now_ms()).await;
                }
                _ = holds.tick() => match self.out.store.holds().await {
                    Ok(holds) => self.out.holds = holds,
                    Err(error) => tracing::warn!(%error, "could not read dependencies; will retry"),
                },
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
        let max_gap = u64::try_from(self.settings.max_gap.as_millis()).unwrap_or(u64::MAX);
        let arrived = now_ms();
        for point in points {
            let max_gap = series_max_gap(max_gap, server.intervals.of(&point.metric));
            self.feed(&server, &point, arrived);
            let rules: Vec<&Rule> = server
                .rules
                .matching(&point.metric, &point.instance)
                .collect();
            for rule in rules {
                let key = SeriesKey {
                    server_id: server.server_id,
                    metric: point.metric.clone(),
                    instance: point.instance.clone(),
                    kind: rule.kind,
                };
                let (evaluation, value) = if rule.kind == Kind::NoData {
                    let tracked = self.series.entry(key.clone()).or_default();
                    (
                        Some(tracked.state.evaluate_silence(rule, arrived, 0.0)),
                        Some(0.0),
                    )
                } else {
                    let value = self.judged(&server, rule, &point);
                    let gates = self.gates(&server, rule, &point, max_gap);
                    let tracked = self.series.entry(key.clone()).or_default();
                    let evaluation = tracked
                        .state
                        .evaluate_gated(rule, point.at, value, gates, max_gap);
                    (evaluation, value)
                };
                let Some(evaluation) = evaluation else {
                    Counters::add(&self.out.counters.out_of_order);
                    continue;
                };
                let at = if rule.kind == Kind::NoData {
                    arrived
                } else {
                    point.at
                };
                self.record(peer, &key, rule, evaluation, at, value).await;
            }
        }
    }

    async fn record(
        &mut self,
        peer: &str,
        key: &SeriesKey,
        rule: &Rule,
        evaluation: Evaluation,
        at: i64,
        value: Option<f64>,
    ) {
        let Some(tracked) = self.series.get_mut(key) else {
            return;
        };
        // A sample that could not be judged writes nothing but a timer
        // change.
        if value.is_none() && !evaluation.timers_changed {
            return;
        }
        let change = Change {
            peer,
            at,
            value,
            transition: evaluation.transition,
            timers_changed: evaluation.timers_changed,
            reason: None,
        };
        self.out.write(key, tracked, Some(rule), change).await;
    }

    /// Records `point` in its feed, when a rule reads it. Nothing is kept
    /// of the others: an agent chooses its instances, and one sending new
    /// names would otherwise grow the map without end.
    fn feed(&mut self, server: &ServerRules, point: &Point, arrived: i64) {
        let Some(need) = server.needs.get(&point.metric) else {
            return;
        };
        let key = (
            server.server_id,
            point.metric.clone(),
            point.instance.clone(),
        );
        let feed = self
            .feeds
            .entry(key)
            .or_insert_with(|| Some(Feed::new(need, arrived)));
        let Some(feed) = feed else {
            return;
        };
        feed.arrived = arrived;
        if feed.last.is_none_or(|(at, _)| point.at > at) {
            feed.last = Some((point.at, point.value));
        }
        for summary in &mut feed.summaries {
            summary.push(point.at, point.value);
        }
    }

    /// The number `rule` judges at `point`, `None` when it cannot yet.
    fn judged(&mut self, server: &ServerRules, rule: &Rule, point: &Point) -> Option<f64> {
        if rule.kind == Kind::Value {
            return Some(point.value);
        }
        let past = if rule.kind == Kind::Compare {
            self.past(server, rule, &point.instance, point.at)
        } else {
            None
        };
        let key = (
            server.server_id,
            point.metric.clone(),
            point.instance.clone(),
        );
        let summary = self
            .feeds
            .get(&key)
            .and_then(|f| f.as_ref())
            .and_then(|f| f.summary(rule.window_ms()));
        derive(rule, summary, point.at, point.value, past)
    }

    /// The average over the window of a comparison, `offset_s` before
    /// `at`, asking history for it when it is not loaded.
    fn past(&mut self, server: &ServerRules, rule: &Rule, instance: &str, at: i64) -> Option<f64> {
        let key = PastKey {
            server_id: server.server_id,
            metric: rule.metric.clone(),
            window_s: rule.window_s,
            offset_s: rule.offset_s,
        };
        let then = at - i64::from(rule.offset_s) * 1000;
        let (value, renew) = match self.pasts.get(&key) {
            Some(past) if past.covers(then) => (
                past.at(instance, then),
                then >= past.from_ms + PAST_RENEW_MS,
            ),
            _ => (None, true),
        };
        if renew && !self.asked.contains_key(&key) {
            let from_ms = then.div_euclid(PAST_STEP_MS) * PAST_STEP_MS;
            let request = Request::Past {
                key: key.clone(),
                target: server.target(),
                from_ms,
                to_ms: from_ms + PAST_CHUNK_MS,
                step_ms: PAST_STEP_MS,
            };
            if self.ask(request) {
                self.asked.insert(key, ());
            }
        }
        value
    }

    /// Whether every condition joined to `rule` holds on the latest value
    /// of its series, no older than `max_gap` from `point`.
    fn gates(&self, server: &ServerRules, rule: &Rule, point: &Point, max_gap: i64) -> bool {
        rule.conditions.iter().all(|condition| {
            let feed = |instance: &str| {
                self.feeds
                    .get(&(
                        server.server_id,
                        condition.metric.clone(),
                        instance.to_owned(),
                    ))
                    .and_then(Option::as_ref)
                    .filter(|f| f.last.is_some())
            };
            let feed = match &condition.instance {
                Some(instance) => feed(instance),
                None => feed(&point.instance).or_else(|| feed("")),
            };
            let Some(feed) = feed else {
                return false;
            };
            let Some((at, value)) = feed.last else {
                return false;
            };
            if (point.at - at).abs() > max_gap {
                return false;
            }
            let value = match condition.function {
                None => Some(value),
                Some(function) => feed
                    .summary(i64::from(condition.window_s.unwrap_or(0)) * 1000)
                    .and_then(|s| {
                        s.function(
                            function,
                            at,
                            i64::from(condition.window_s.unwrap_or(0)) * 1000,
                        )
                    }),
            };
            value.is_some_and(|v| condition.comparison.holds(v, condition.value))
        })
    }

    fn ask(&mut self, request: Request) -> bool {
        let Some(history) = &self.history else {
            return false;
        };
        let asked = history.ask(request);
        if !asked {
            Counters::add(&self.out.counters.history_dropped);
        }
        asked
    }

    /// Takes an answer of the history task.
    fn answer(&mut self, answer: Answer) {
        match answer {
            Answer::Seed {
                server_id,
                metric,
                step_ms,
                from_ms,
                series,
            } => {
                let Some(need) = self.cached_need(server_id, &metric) else {
                    return;
                };
                for (instance, points) in series {
                    let feed = self
                        .feeds
                        .entry((server_id, metric.clone(), instance))
                        .or_insert_with(|| Some(Feed::new(&need, 0)));
                    let Some(feed) = feed else {
                        continue;
                    };
                    for summary in &mut feed.summaries {
                        if summary.width_ms() == step_ms {
                            summary.load(from_ms, &points);
                        }
                    }
                    if feed.last.is_none() {
                        feed.last = points.last().copied();
                    }
                }
            }
            Answer::Instances {
                server_id,
                metric,
                instances,
            } => {
                let Some(need) = self.cached_need(server_id, &metric) else {
                    return;
                };
                let loaded = self.loaded_at.get(&server_id).copied().unwrap_or_default();
                for instance in instances {
                    self.feeds
                        .entry((server_id, metric.clone(), instance))
                        .or_insert_with(|| Some(Feed::new(&need, loaded)));
                }
            }
            Answer::Past {
                key,
                from_ms,
                to_ms,
                step_ms,
                series,
                failed,
            } => {
                self.asked.remove(&key);
                if failed {
                    return;
                }
                self.pasts.insert(
                    key,
                    Past {
                        from_ms,
                        to_ms,
                        step_ms,
                        series: series.into_iter().collect(),
                    },
                );
            }
        }
    }

    /// The need of `metric` on a server still cached.
    fn cached_need(&self, server_id: Uuid, metric: &str) -> Option<Need> {
        self.rules
            .values()
            .flatten()
            .find(|s| s.server_id == server_id)?
            .needs
            .get(metric)
            .cloned()
    }

    /// Asks history for what the rules of `server` read, for the metrics
    /// in `metrics`, and starts watching the series no data rules name.
    fn prepare(&mut self, server: &ServerRules, metrics: &[String]) {
        let now = now_ms();
        let target = server.target();
        for metric in metrics {
            let Some(need) = server.needs.get(metric) else {
                continue;
            };
            for &span in &need.spans {
                self.ask(Request::Seed {
                    server_id: server.server_id,
                    target: target.clone(),
                    metric: metric.clone(),
                    function: need.seed,
                    from_ms: now - span,
                    to_ms: now,
                    step_ms: bucket_width(span),
                });
            }
            if need.arrival {
                let mut every_instance = false;
                for rule in server.rules.enabled() {
                    if rule.kind != Kind::NoData || rule.metric != *metric {
                        continue;
                    }
                    match &rule.instance {
                        Some(instance) => {
                            self.feeds
                                .entry((server.server_id, metric.clone(), instance.clone()))
                                .or_insert_with(|| Some(Feed::new(need, now)));
                        }
                        None => every_instance = true,
                    }
                }
                if every_instance {
                    self.ask(Request::Instances {
                        server_id: server.server_id,
                        target: target.clone(),
                        metric: metric.clone(),
                        from_ms: now - 86_400_000,
                    });
                }
            }
        }
    }

    /// Judges every `nodata` rule of the servers held here at `now`.
    async fn sweep(&mut self, now: i64) {
        let servers: Vec<(String, Arc<ServerRules>)> = self
            .rules
            .iter()
            .filter_map(|(peer, s)| Some((peer.clone(), s.clone()?)))
            .filter(|(_, s)| s.needs.values().any(|n| n.arrival))
            .collect();
        for (peer, server) in servers {
            let loaded = self
                .loaded_at
                .get(&server.server_id)
                .copied()
                .unwrap_or(now);
            let watched: Vec<(String, String, i64)> = self
                .feeds
                .of(server.server_id)
                .filter(|((_, metric, _), feed)| {
                    feed.is_some() && server.needs.get(metric).is_some_and(|n| n.arrival)
                })
                .filter_map(|((_, metric, instance), feed)| {
                    Some((metric.clone(), instance.clone(), feed.as_ref()?.arrived))
                })
                .collect();
            for (metric, instance, arrived) in watched {
                let Some(rule) = server.rules.lookup_kind(&metric, &instance, Kind::NoData) else {
                    continue;
                };
                let silence = (now - arrived.max(loaded)).max(0) as f64 / 1000.0;
                let key = SeriesKey {
                    server_id: server.server_id,
                    metric,
                    instance,
                    kind: Kind::NoData,
                };
                let tracked = self.series.entry(key.clone()).or_default();
                if tracked.state.is_ok()
                    && rule
                        .threshold(super::Severity::Warning)
                        .or(rule.critical)
                        .is_some_and(|limit| silence <= limit)
                {
                    continue;
                }
                let evaluation = tracked.state.evaluate_silence(rule, now, silence);
                // An open incident's value is the silence so far; write it
                // once a minute rather than at every sweep.
                if evaluation.transition.is_none()
                    && !evaluation.timers_changed
                    && (silence as i64) % 60 >= self.settings.poll_interval.as_secs().max(1) as i64
                {
                    continue;
                }
                self.record(&peer, &key, rule, evaluation, now, Some(silence))
                    .await;
            }
        }
    }

    fn forget(&mut self, peer: &str) {
        if let Some(Some(server)) = self.rules.remove(peer) {
            self.drop_server(server.server_id);
        }
    }

    /// Forgets everything held of a server but its cached rules.
    fn drop_server(&mut self, server_id: Uuid) {
        self.series.drop_server(server_id);
        self.feeds.drop_server(server_id);
        self.pasts.drop_server(server_id);
        self.asked.drop_server(server_id);
        self.loaded_at.remove(&server_id);
    }

    /// The cached thresholds of `peer`, loaded on first use with the
    /// stored state of its series.
    async fn server(&mut self, peer: &str) -> Option<Arc<ServerRules>> {
        if let Some(entry) = self.rules.get(peer) {
            return entry.clone();
        }
        let loaded = match self.out.store.load_rules(peer).await {
            Ok(Some(server)) => self
                .out
                .store
                .load_server_states(server.server_id)
                .await
                .map(|states| Some((server, states))),
            Ok(None) => Ok(None),
            Err(error) => Err(error),
        };
        match loaded {
            Ok(Some((server, states))) => {
                let server = Arc::new(server);
                self.drop_server(server.server_id);
                self.loaded_at.insert(server.server_id, now_ms());
                self.series.extend(states.into_iter().map(|s| {
                    let tracked = Tracked {
                        state: s.state,
                        incident: s.incident,
                    };
                    (s.key, tracked)
                }));
                // Stored states may belong to removed thresholds.
                self.drop_unmatched(peer, &server).await;
                let metrics: Vec<String> = server.needs.keys().cloned().collect();
                self.prepare(&server, &metrics);
                self.rules.insert(peer.to_owned(), Some(server.clone()));
                Some(server)
            }
            Ok(None) => {
                self.rules.insert(peer.to_owned(), None);
                None
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
        let changed: Vec<(String, Arc<ServerRules>)> = self
            .rules
            .iter()
            .filter_map(|(peer, entry)| {
                let server = entry.as_ref()?;
                (current.get(&server.server_id) != Some(&server.revision))
                    .then(|| (peer.clone(), server.clone()))
            })
            .collect();
        for (peer, old) in changed {
            match self.out.store.load_rules(&peer).await {
                Ok(Some(server)) => {
                    tracing::info!(agent_id = %peer, revision = server.revision, "thresholds reloaded");
                    self.drop_unmatched(&peer, &server).await;
                    // Feeds whose need changed start over, from history.
                    let moved: Vec<String> = server
                        .needs
                        .iter()
                        .filter(|(metric, need)| old.needs.get(*metric) != Some(need))
                        .map(|(metric, _)| metric.clone())
                        .collect();
                    let id = server.server_id;
                    self.feeds.retain_server(id, |(_, metric, _), _| {
                        server.needs.contains_key(metric) && !moved.contains(metric)
                    });
                    self.pasts
                        .retain_server(id, |key, _| !moved.contains(&key.metric));
                    self.prepare(&server, &moved);
                    self.rules.insert(peer, Some(Arc::new(server)));
                }
                Ok(None) => {
                    // The server is removed, and its incidents resolved.
                    self.rules.remove(&peer);
                    self.drop_server(old.server_id);
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
            .of(server.server_id)
            .map(|(k, _)| k)
            .filter(|k| {
                server
                    .rules
                    .lookup_kind(&k.metric, &k.instance, k.kind)
                    .is_none()
            })
            .cloned()
            .collect();
        let now = now_ms();
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
/// The longest silence between two samples of a series that keeps its
/// pending timers, in milliseconds: `max_gap`, or two intervals of the
/// check that writes it when longer, so a check that runs every hour can
/// still hold a condition for two hours and lose one sample (ADR-0097).
fn series_max_gap(max_gap_ms: u64, interval_ms: Option<u64>) -> i64 {
    let gap = interval_ms.map_or(max_gap_ms, |interval| {
        max_gap_ms.max(interval.saturating_mul(2))
    });
    i64::try_from(gap).unwrap_or(i64::MAX)
}

struct Output {
    store: AlertStore,
    notifier: Arc<dyn Notifier>,
    counters: Arc<Counters>,
    /// What dependencies hold (ADR-0152): a held series opens no incident,
    /// and the next sample that still fails tries again.
    holds: Holds,
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
        let held = || {
            self.holds
                .series(key.server_id, &key.metric, &key.instance)
                .is_some()
        };
        let incident = match (tracked.state.firing, tracked.incident, rule) {
            (Some(_), None, Some(_)) if held() => {
                // Behind a parent that is down, in doubt or recovering: the
                // state is kept, so the next sample opens it once free.
                event = None;
                tracing::debug!(agent_id = change.peer, metric = %key.metric, instance = %key.instance, "incident held by a dependency");
                IncidentStep::None
            }
            (Some(severity), None, Some(rule)) => {
                event = event.or(Some(Transition::Opened(severity)));
                IncidentStep::Open { severity, rule }
            }
            (Some(severity), Some((id, peak)), Some(rule)) => {
                let peak = change.value.map_or(peak, |v| rule.judged().worst(peak, v));
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::super::rules::Rules;
    use super::super::store::CheckIntervals;
    use super::*;

    const MINUTE: u64 = 60_000;

    #[test]
    fn the_gap_follows_long_intervals_only() {
        let base = 10 * MINUTE;
        assert_eq!(series_max_gap(base, None), 600_000);
        for interval in [10_000, MINUTE, 5 * MINUTE] {
            assert_eq!(series_max_gap(base, Some(interval)), 600_000, "{interval}");
        }
        assert_eq!(series_max_gap(base, Some(15 * MINUTE)), 1_800_000);
        assert_eq!(series_max_gap(base, Some(60 * MINUTE)), 7_200_000);
        assert_eq!(series_max_gap(u64::MAX, Some(MINUTE)), i64::MAX);
    }

    /// An engine without a database or history.
    fn bare_engine() -> Engine {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://localhost/unused")
            .unwrap();
        Engine {
            settings: EngineSettings::default(),
            rules: HashMap::new(),
            series: Default::default(),
            feeds: Default::default(),
            loaded_at: HashMap::new(),
            pasts: Default::default(),
            asked: Default::default(),
            history: None,
            out: Output {
                store: AlertStore::new(pool),
                notifier: Arc::new(LogNotifier),
                counters: Arc::new(Counters::default()),
                holds: Holds::default(),
            },
        }
    }

    fn server_rules(n: u128) -> ServerRules {
        ServerRules {
            server_id: Uuid::from_u128(n),
            peer: format!("web-{n}"),
            site: "paris".into(),
            revision: 1,
            rules: Rules::default(),
            intervals: CheckIntervals::default(),
            needs: BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn forgetting_a_server_touches_its_series_only() {
        // 8,000 servers reloading their thresholds at once stopped the
        // engine for half a minute in the load test, as forgetting or
        // reloading one server scanned every server's series. A fleet's
        // worth of reconnections must now cost about one pass.
        const SERVERS: u128 = 8_000;
        const SERIES: usize = 12;
        let mut engine = bare_engine();
        for n in 0..SERVERS {
            let server = server_rules(n);
            engine.series.extend((0..SERIES).map(|i| {
                let key = SeriesKey {
                    server_id: server.server_id,
                    metric: format!("monitorflow_check_{i}_up"),
                    instance: String::new(),
                    kind: Kind::Value,
                };
                (key, Tracked::default())
            }));
            engine
                .rules
                .insert(server.peer.clone(), Some(Arc::new(server)));
        }
        assert_eq!(engine.series.len(), SERVERS as usize * SERIES);
        let start = std::time::Instant::now();
        for n in 0..SERVERS {
            engine.forget(&format!("web-{n}"));
        }
        let took = start.elapsed();
        assert_eq!(engine.series.len(), 0);
        // 12 ms unoptimized on the load test's host; the flat map took
        // 3.3 s.
        assert!(took < Duration::from_secs(1), "{took:?}");
    }

    #[tokio::test]
    async fn only_series_a_rule_reads_are_fed() {
        let mut engine = bare_engine();
        let server = server_rules(1);
        // An agent naming a new instance in every sample.
        for n in 0..1000 {
            let point = Point {
                metric: "monitorflow_disk_used_percent".into(),
                instance: format!("/mnt/{n}"),
                at: n,
                value: 1.0,
            };
            engine.feed(&server, &point, n);
        }
        assert!(engine.feeds.is_empty());
    }

    #[test]
    fn a_comparison_reads_the_point_at_or_just_before_its_time() {
        let past = Past {
            from_ms: 0,
            to_ms: 3_600_000,
            step_ms: 300_000,
            series: [("/".to_owned(), vec![(0, 1.0), (300_000, 2.0)])].into(),
        };
        assert_eq!(past.at("/", 299_999), Some(1.0));
        assert_eq!(past.at("/", 300_000), Some(2.0));
        // Past the last point by a step: missing, not stale.
        assert_eq!(past.at("/", 600_000), None);
        assert_eq!(past.at("/x", 0), None);
        assert!(past.covers(0) && !past.covers(3_600_000));
    }
}
