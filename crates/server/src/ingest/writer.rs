//! A background task that batches items from a bounded queue and writes
//! them to a [`Sink`], with retries (ADR-0013).
//!
//! Producers call [`Queue::push`], which never waits: when the queue is
//! full the item is dropped and counted, so a slow or unreachable backend
//! never stalls an agent stream.

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use monitorflow_common::config::IngestConfig;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{interval, sleep, timeout, MissedTickBehavior};

/// Why a write failed.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    /// Network errors, 429 and 5xx: worth another attempt.
    #[error("{0}")]
    Retryable(String),
    /// The backend refused the data; retrying would not help.
    #[error("{0}")]
    Permanent(String),
}

/// A backend that stores batches of items.
pub trait Sink: Send + Sync + 'static {
    type Item: Send + 'static;

    /// Short name for logs, like `victoriametrics`.
    fn name(&self) -> &'static str;

    fn write(&self, batch: &[Self::Item]) -> impl Future<Output = Result<(), WriteError>> + Send;
}

/// Retry schedule of one batch: exponential backoff between attempts.
#[derive(Debug, Clone)]
pub struct RetrySettings {
    pub attempts: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
}

impl Default for RetrySettings {
    fn default() -> Self {
        Self {
            attempts: 5,
            initial_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, Clone)]
pub struct WriterSettings {
    pub batch_size: usize,
    pub flush_interval: Duration,
    pub queue_capacity: usize,
    pub retry: RetrySettings,
    /// How often the counters are logged, when they changed.
    pub report_interval: Duration,
}

impl WriterSettings {
    pub fn from_config(config: &IngestConfig) -> Self {
        Self {
            batch_size: config.batch_size,
            flush_interval: Duration::from_millis(config.flush_interval_ms),
            queue_capacity: config.queue_capacity,
            retry: RetrySettings::default(),
            report_interval: Duration::from_secs(60),
        }
    }
}

impl Default for WriterSettings {
    fn default() -> Self {
        Self::from_config(&IngestConfig::default())
    }
}

#[derive(Debug, Default)]
struct Counters {
    accepted: AtomicU64,
    rejected: AtomicU64,
    dropped: AtomicU64,
    written: AtomicU64,
    failed: AtomicU64,
}

impl Counters {
    fn add(counter: &AtomicU64, n: u64) {
        counter.fetch_add(n, Ordering::Relaxed);
    }

    fn snapshot(&self) -> Stats {
        Stats {
            accepted: self.accepted.load(Ordering::Relaxed),
            rejected: self.rejected.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            written: self.written.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
        }
    }
}

/// Totals since the writer started.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Queued for writing.
    pub accepted: u64,
    /// Refused by validation before reaching the queue.
    pub rejected: u64,
    /// Dropped because the queue was full or closed.
    pub dropped: u64,
    /// Stored by the backend.
    pub written: u64,
    /// Given up on after a permanent error or the last retry.
    pub failed: u64,
}

impl Stats {
    /// Accepted items not yet written or failed.
    pub fn pending(&self) -> u64 {
        self.accepted.saturating_sub(self.written + self.failed)
    }
}

/// Producer handle of a writer; clones share the queue and the counters.
pub struct Queue<T> {
    tx: mpsc::Sender<T>,
    counters: Arc<Counters>,
}

impl<T> Clone for Queue<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            counters: self.counters.clone(),
        }
    }
}

impl<T> Queue<T> {
    /// Queues `item` without waiting. Returns false, and counts the item as
    /// dropped, when the queue is full or the writer has stopped.
    pub fn push(&self, item: T) -> bool {
        match self.tx.try_send(item) {
            Ok(()) => {
                Counters::add(&self.counters.accepted, 1);
                true
            }
            Err(_) => {
                Counters::add(&self.counters.dropped, 1);
                false
            }
        }
    }

    /// Counts `n` items that failed validation, so they show in [`Stats`].
    pub fn reject(&self, n: u64) {
        Counters::add(&self.counters.rejected, n);
    }

    pub fn stats(&self) -> Stats {
        self.counters.snapshot()
    }
}

/// A running writer. Dropping it without [`Writer::shutdown`] lets the task
/// flush what is queued in the background, with no deadline.
pub struct Writer<T> {
    queue: Queue<T>,
    stop: oneshot::Sender<()>,
    task: JoinHandle<()>,
    name: &'static str,
}

impl<T: Send + 'static> Writer<T> {
    /// Starts the background task of `sink`.
    pub fn spawn<S: Sink<Item = T>>(sink: S, settings: WriterSettings) -> Self {
        let (tx, rx) = mpsc::channel(settings.queue_capacity.max(1));
        let counters = Arc::new(Counters::default());
        let (stop, stopped) = oneshot::channel();
        let name = sink.name();
        let task = tokio::spawn(run(sink, rx, settings, counters.clone(), stopped));
        Self {
            queue: Queue { tx, counters },
            stop,
            task,
            name,
        }
    }

    pub fn queue(&self) -> Queue<T> {
        self.queue.clone()
    }

    /// Stops accepting items, flushes what is queued and waits at most
    /// `deadline`. Items still pending after it are lost and logged.
    pub async fn shutdown(self, deadline: Duration) -> Stats {
        let _ = self.stop.send(());
        let mut task = self.task;
        if timeout(deadline, &mut task).await.is_err() {
            task.abort();
        }
        let stats = self.queue.stats();
        if stats.pending() > 0 {
            tracing::warn!(
                sink = self.name,
                lost = stats.pending(),
                ?deadline,
                "items not flushed before the shutdown deadline"
            );
        }
        tracing::info!(sink = self.name, ?stats, "writer stopped");
        stats
    }
}

async fn run<S: Sink>(
    sink: S,
    mut rx: mpsc::Receiver<S::Item>,
    settings: WriterSettings,
    counters: Arc<Counters>,
    mut stop: oneshot::Receiver<()>,
) {
    let mut batch = Vec::with_capacity(settings.batch_size);
    let mut flush = interval(settings.flush_interval);
    flush.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut report = interval(settings.report_interval);
    report.reset();
    let mut reported = Stats::default();
    loop {
        tokio::select! {
            biased;
            _ = &mut stop => break,
            item = rx.recv() => {
                let Some(item) = item else { break };
                batch.push(item);
                if batch.len() >= settings.batch_size {
                    write(&sink, &mut batch, &settings.retry, &counters).await;
                    flush.reset();
                }
            }
            _ = flush.tick() => write(&sink, &mut batch, &settings.retry, &counters).await,
            _ = report.tick() => reported = log_stats(sink.name(), reported, counters.snapshot()),
        }
    }
    // Shutdown: refuse new items, then flush everything already queued.
    rx.close();
    while let Some(item) = rx.recv().await {
        batch.push(item);
        if batch.len() >= settings.batch_size {
            write(&sink, &mut batch, &settings.retry, &counters).await;
        }
    }
    write(&sink, &mut batch, &settings.retry, &counters).await;
}

/// Writes and clears `batch`, retrying retryable errors with backoff.
async fn write<S: Sink>(
    sink: &S,
    batch: &mut Vec<S::Item>,
    retry: &RetrySettings,
    counters: &Counters,
) {
    if batch.is_empty() {
        return;
    }
    let count = batch.len() as u64;
    let mut backoff = retry.initial_backoff;
    let mut attempt = 1;
    let result = loop {
        match sink.write(batch).await {
            Err(WriteError::Retryable(error)) if attempt < retry.attempts => {
                tracing::warn!(sink = sink.name(), attempt, %error, ?backoff, "write failed; retrying");
                sleep(backoff).await;
                backoff = (backoff * 2).min(retry.max_backoff);
                attempt += 1;
            }
            result => break result,
        }
    };
    match result {
        Ok(()) => Counters::add(&counters.written, count),
        Err(error) => {
            tracing::error!(sink = sink.name(), items = count, attempt, %error, "write failed; dropping the batch");
            Counters::add(&counters.failed, count);
        }
    }
    batch.clear();
}

/// Logs the counters when they changed, louder when items were lost.
fn log_stats(sink: &'static str, last: Stats, now: Stats) -> Stats {
    if now == last {
        return now;
    }
    let lost = (now.dropped + now.failed) - (last.dropped + last.failed);
    if lost > 0 {
        tracing::warn!(sink, ?now, lost_since_last_report = lost, "writer counters");
    } else {
        tracing::info!(sink, ?now, "writer counters");
    }
    now
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Records batches; fails the first `failures` writes with `error`.
    #[derive(Clone, Default)]
    struct Fake {
        batches: Arc<Mutex<Vec<Vec<u32>>>>,
        failures: Arc<Mutex<Vec<WriteError>>>,
        gate: Option<Arc<tokio::sync::Semaphore>>,
    }

    impl Sink for Fake {
        type Item = u32;

        fn name(&self) -> &'static str {
            "fake"
        }

        async fn write(&self, batch: &[u32]) -> Result<(), WriteError> {
            if let Some(gate) = &self.gate {
                let _ = gate.acquire().await;
            }
            if let Some(error) = self.failures.lock().unwrap().pop() {
                return Err(error);
            }
            self.batches.lock().unwrap().push(batch.to_vec());
            Ok(())
        }
    }

    fn settings(batch_size: usize, queue_capacity: usize) -> WriterSettings {
        WriterSettings {
            batch_size,
            flush_interval: Duration::from_secs(3600),
            queue_capacity,
            retry: RetrySettings {
                attempts: 3,
                initial_backoff: Duration::from_millis(1),
                max_backoff: Duration::from_millis(2),
            },
            report_interval: Duration::from_secs(3600),
        }
    }

    #[tokio::test]
    async fn full_batches_are_written_and_the_rest_at_shutdown() {
        let fake = Fake::default();
        let writer = Writer::spawn(fake.clone(), settings(2, 10));
        let queue = writer.queue();
        for i in 0..5 {
            assert!(queue.push(i));
        }
        let stats = writer.shutdown(Duration::from_secs(5)).await;
        assert_eq!(
            *fake.batches.lock().unwrap(),
            [vec![0, 1], vec![2, 3], vec![4]]
        );
        assert_eq!((stats.accepted, stats.written, stats.pending()), (5, 5, 0));
        assert!(!queue.push(9), "a stopped writer accepts nothing");
        assert_eq!(queue.stats().dropped, 1);
    }

    #[tokio::test]
    async fn partial_batches_are_flushed_on_the_interval() {
        let fake = Fake::default();
        let mut s = settings(100, 100);
        s.flush_interval = Duration::from_millis(20);
        let writer = Writer::spawn(fake.clone(), s);
        writer.queue().push(7);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while fake.batches.lock().unwrap().is_empty() {
            assert!(tokio::time::Instant::now() < deadline, "no flush");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(*fake.batches.lock().unwrap(), [vec![7]]);
        writer.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn retryable_errors_are_retried_permanent_ones_are_not() {
        let fake = Fake::default();
        *fake.failures.lock().unwrap() = vec![
            WriteError::Retryable("503".into()),
            WriteError::Retryable("timeout".into()),
        ];
        let writer = Writer::spawn(fake.clone(), settings(1, 10));
        writer.queue().push(1);
        let stats = writer.shutdown(Duration::from_secs(5)).await;
        assert_eq!(*fake.batches.lock().unwrap(), [vec![1]]);
        assert_eq!((stats.written, stats.failed), (1, 0));

        let fake = Fake::default();
        *fake.failures.lock().unwrap() = vec![WriteError::Permanent("400".into())];
        let writer = Writer::spawn(fake.clone(), settings(1, 10));
        writer.queue().push(1);
        let stats = writer.shutdown(Duration::from_secs(5)).await;
        assert!(fake.batches.lock().unwrap().is_empty());
        assert_eq!((stats.written, stats.failed), (0, 1));
    }

    #[tokio::test]
    async fn retries_stop_after_the_last_attempt() {
        let fake = Fake::default();
        *fake.failures.lock().unwrap() = (0..3)
            .map(|_| WriteError::Retryable("503".into()))
            .collect();
        let writer = Writer::spawn(fake.clone(), settings(1, 10));
        writer.queue().push(1);
        writer.queue().push(2);
        let stats = writer.shutdown(Duration::from_secs(5)).await;
        assert_eq!(*fake.batches.lock().unwrap(), [vec![2]]);
        assert_eq!((stats.written, stats.failed), (1, 1));
    }

    #[tokio::test]
    async fn a_full_queue_drops_new_items_without_waiting() {
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let fake = Fake {
            gate: Some(gate.clone()),
            ..Fake::default()
        };
        let writer = Writer::spawn(fake.clone(), settings(1, 2));
        let queue = writer.queue();
        // The task holds one item in a blocked write; two more fill the queue.
        assert!(queue.push(0));
        while queue.tx.capacity() < 2 {
            tokio::task::yield_now().await;
        }
        assert!(queue.push(1) && queue.push(2));
        assert!(!queue.push(3));
        assert_eq!(queue.stats().dropped, 1);
        gate.add_permits(10);
        let stats = writer.shutdown(Duration::from_secs(5)).await;
        assert_eq!(stats.written, 3);
        assert_eq!(fake.batches.lock().unwrap().concat(), [0, 1, 2]);
    }

    #[tokio::test]
    async fn shutdown_gives_up_after_the_deadline() {
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let fake = Fake {
            gate: Some(gate),
            ..Fake::default()
        };
        let writer = Writer::spawn(fake, settings(1, 10));
        writer.queue().push(1);
        writer.queue().push(2);
        let stats = writer.shutdown(Duration::from_millis(50)).await;
        assert_eq!(stats.pending(), 2);
    }

    #[test]
    fn rejected_items_are_counted() {
        let (tx, _rx) = mpsc::channel(1);
        let queue: Queue<u32> = Queue {
            tx,
            counters: Arc::default(),
        };
        queue.reject(3);
        assert_eq!(queue.stats().rejected, 3);
    }
}
