//! Samples waiting to go upstream in `SampleBatch` envelopes.
//!
//! The sensors task pushes every sample into one bounded [`Uplink`]; the
//! stream takes a batch when [`UploadSettings::batch_size`] samples are
//! waiting, or every [`UploadSettings::flush_interval`] whatever is there.
//! While the stream is down samples accumulate; once
//! [`UploadSettings::capacity`] is reached the oldest are dropped and
//! counted, so a long outage keeps the latest data. A batch the stream
//! took is lost if the connection breaks before it was written: the server
//! does not acknowledge batches.
//!
//! TODO(phase2-buffer): spill to a disk buffer in `data_dir` before
//! dropping, with the stream sequence persisted next to it.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use monitorflow_common::proto::v1 as proto;
use monitorflow_sensors::Sample;
use tokio::sync::{mpsc, Notify};
use tokio::time::Instant;

/// Most samples in one batch the server accepts (ADR-0013).
pub const MAX_BATCH: usize = 10_000;
/// Least time between two warnings about dropped samples.
const DROP_LOG_EVERY: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct UploadSettings {
    /// A batch goes as soon as this many samples wait; at most
    /// [`MAX_BATCH`].
    pub batch_size: usize,
    /// Samples waiting this long go even in a smaller batch.
    pub flush_interval: Duration,
    /// Most samples kept while the stream is down.
    pub capacity: usize,
}

impl Default for UploadSettings {
    fn default() -> Self {
        Self {
            batch_size: 500,
            flush_interval: Duration::from_secs(10),
            // About 10 minutes of 80 samples a second.
            capacity: 50_000,
        }
    }
}

/// Counters since the agent started.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UploadStats {
    pub queued: u64,
    /// Samples handed to the stream.
    pub sent: u64,
    /// Oldest samples dropped because the buffer was full.
    pub dropped: u64,
}

#[derive(Debug, Default)]
struct State {
    samples: VecDeque<proto::Sample>,
    stats: UploadStats,
    warned: Option<Instant>,
}

/// Shared buffer between the sensors task and the stream.
#[derive(Debug, Clone)]
pub struct Uplink {
    state: Arc<Mutex<State>>,
    filled: Arc<Notify>,
    settings: UploadSettings,
}

impl Uplink {
    /// `batch_size` is capped to [`MAX_BATCH`] and `capacity` raised to at
    /// least one batch.
    pub fn new(mut settings: UploadSettings) -> Self {
        settings.batch_size = settings.batch_size.clamp(1, MAX_BATCH);
        settings.capacity = settings.capacity.max(settings.batch_size);
        Self {
            state: Arc::default(),
            filled: Arc::default(),
            settings,
        }
    }

    pub fn settings(&self) -> &UploadSettings {
        &self.settings
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // Every critical section leaves the state consistent.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Queues a sample, dropping the oldest one when full.
    pub fn push(&self, sample: proto::Sample) {
        let mut state = self.lock();
        if state.samples.len() >= self.settings.capacity {
            state.samples.pop_front();
            state.stats.dropped += 1;
            let now = Instant::now();
            if state
                .warned
                .is_none_or(|at| now.duration_since(at) >= DROP_LOG_EVERY)
            {
                state.warned = Some(now);
                tracing::warn!(
                    capacity = self.settings.capacity,
                    dropped = state.stats.dropped,
                    "upload buffer full, dropping the oldest samples"
                );
            }
        }
        state.samples.push_back(sample);
        state.stats.queued += 1;
        if state.samples.len() >= self.settings.batch_size {
            self.filled.notify_waiters();
        }
    }

    pub fn len(&self) -> usize {
        self.lock().samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether a full batch waits.
    pub fn is_full_batch(&self) -> bool {
        self.len() >= self.settings.batch_size
    }

    /// The oldest samples, at most one batch.
    pub fn take(&self) -> Vec<proto::Sample> {
        let mut state = self.lock();
        let n = state.samples.len().min(self.settings.batch_size);
        state.stats.sent += n as u64;
        state.samples.drain(..n).collect()
    }

    /// Resolves once a full batch waits.
    pub async fn filled(&self) {
        loop {
            // Created before the check, so a push in between wakes it.
            let notified = self.filled.notified();
            if self.is_full_batch() {
                return;
            }
            notified.await;
        }
    }

    pub fn stats(&self) -> UploadStats {
        self.lock().stats
    }
}

pub fn to_proto(sample: Sample) -> proto::Sample {
    proto::Sample {
        sensor: sample.sensor,
        instance: sample.instance,
        timestamp_unix_ms: sample.timestamp_unix_ms,
        value: sample.value,
        labels: sample.labels.into_iter().collect(),
    }
}

/// Moves samples from a scheduler into `uplink` until the channel closes;
/// returns how many.
pub async fn collect(mut samples: mpsc::Receiver<Sample>, uplink: Uplink) -> u64 {
    let mut count = 0;
    while let Some(sample) = samples.recv().await {
        count += 1;
        uplink.push(to_proto(sample));
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(value: f64) -> proto::Sample {
        proto::Sample {
            sensor: "cpu".into(),
            value,
            timestamp_unix_ms: 1,
            ..proto::Sample::default()
        }
    }

    fn uplink(batch_size: usize, capacity: usize) -> Uplink {
        Uplink::new(UploadSettings {
            batch_size,
            capacity,
            ..UploadSettings::default()
        })
    }

    #[test]
    fn settings_are_bounded() {
        let u = uplink(20_000, 5);
        assert_eq!(u.settings().batch_size, MAX_BATCH);
        assert_eq!(u.settings().capacity, MAX_BATCH);
        assert_eq!(uplink(0, 0).settings().batch_size, 1);
    }

    #[test]
    fn batches_are_taken_oldest_first() {
        let u = uplink(3, 10);
        for v in 0..7 {
            u.push(sample(v.into()));
        }
        assert!(u.is_full_batch());
        let values = |b: Vec<proto::Sample>| b.iter().map(|s| s.value).collect::<Vec<_>>();
        assert_eq!(values(u.take()), [0.0, 1.0, 2.0]);
        assert_eq!(values(u.take()), [3.0, 4.0, 5.0]);
        assert!(!u.is_full_batch());
        assert_eq!(values(u.take()), [6.0]);
        assert!(u.take().is_empty());
        assert_eq!(
            u.stats(),
            UploadStats {
                queued: 7,
                sent: 7,
                dropped: 0
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_buffer_drops_the_oldest_samples() {
        let u = uplink(2, 4);
        for v in 0..10 {
            u.push(sample(v.into()));
        }
        assert_eq!(u.len(), 4);
        let kept: Vec<_> = [u.take(), u.take()]
            .concat()
            .iter()
            .map(|s| s.value)
            .collect();
        assert_eq!(kept, [6.0, 7.0, 8.0, 9.0]);
        assert_eq!(u.stats().dropped, 6);
    }

    #[tokio::test(start_paused = true)]
    async fn filled_waits_for_a_full_batch() {
        let u = uplink(2, 10);
        let waiter = tokio::spawn({
            let u = u.clone();
            async move { u.filled().await }
        });
        u.push(sample(1.0));
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        u.push(sample(2.0));
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap();
        // Already full: returns at once.
        u.filled().await;
    }

    #[tokio::test]
    async fn collect_converts_and_counts() {
        let (tx, rx) = mpsc::channel(4);
        let u = uplink(10, 10);
        let task = tokio::spawn(collect(rx, u.clone()));
        let s = Sample::now(2.5).with_instance("/").with_label("fs", "ext4");
        tx.send(Sample {
            sensor: "disk".into(),
            ..s
        })
        .await
        .unwrap();
        drop(tx);
        assert_eq!(task.await.unwrap(), 1);
        let batch = u.take();
        assert_eq!(
            (batch[0].sensor.as_str(), batch[0].instance.as_str()),
            ("disk", "/")
        );
        assert_eq!(
            (batch[0].value, batch[0].labels["fs"].as_str()),
            (2.5, "ext4")
        );
    }
}
