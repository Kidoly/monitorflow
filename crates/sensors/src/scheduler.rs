//! Runs each configured sensor on its own interval and sends the samples
//! to one bounded channel.
//!
//! Per sensor, one task:
//! - the first tick comes after a random delay below the interval (unless
//!   [`SchedulerOptions::jitter`] is off), so a fleet started together
//!   does not collect in lockstep;
//! - each collection runs in its own task, bounded by
//!   [`SensorConfig::timeout`], which is never longer than the interval;
//!   the next one starts only after it returned, timed out or panicked, so
//!   runs of one sensor never overlap;
//! - ticks missed while a collection ran late are skipped, not bursted;
//! - errors, timeouts and panics are counted and logged at most once per
//!   [`LOG_EVERY`] per sensor and kind, then the sensor waits for its next
//!   tick; a [`SensorError::Permanent`] stops that sensor only;
//! - samples get the configured sensor name and default instance, are
//!   validated (invalid ones are dropped and counted) and sent with
//!   `try_send`: when the channel is full the new sample is dropped and
//!   counted, sensors are never slowed down by a slow consumer.
//!
//! [`SchedulerHandle::shutdown`] cancels running collections and waits for
//! every task. Dropping the handle stops the tasks too, without waiting.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio::task::{AbortHandle, JoinSet};
use tokio::time::{interval_at, timeout_at, Instant, MissedTickBehavior};

use crate::validate::{self, ValidationError};
use crate::{Sample, Sensor, SensorConfig, SensorError};

/// Least time between two log lines about the same problem of one sensor.
pub const LOG_EVERY: Duration = Duration::from_secs(5 * 60);
/// Samples buffered between the sensors and their consumer by default.
pub const DEFAULT_CAPACITY: usize = 1024;

#[derive(Debug, Clone)]
pub struct SchedulerOptions {
    /// Capacity of the sample channel.
    pub capacity: usize,
    /// Delay each sensor's first tick by a random time below its interval.
    pub jitter: bool,
}

impl Default for SchedulerOptions {
    fn default() -> Self {
        Self {
            capacity: DEFAULT_CAPACITY,
            jitter: true,
        }
    }
}

/// Counters since the scheduler started, over all sensors.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Collections that returned samples.
    pub collected: u64,
    /// Collections that returned an error.
    pub failed: u64,
    pub timed_out: u64,
    pub panicked: u64,
    /// Samples refused by [`validate::sample`].
    pub invalid: u64,
    /// Samples dropped because the channel was full.
    pub dropped: u64,
}

#[derive(Debug, Default)]
struct Counters {
    collected: AtomicU64,
    failed: AtomicU64,
    timed_out: AtomicU64,
    panicked: AtomicU64,
    invalid: AtomicU64,
    dropped: AtomicU64,
}

impl Counters {
    fn bump(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> Stats {
        let get = |c: &AtomicU64| c.load(Ordering::Relaxed);
        Stats {
            collected: get(&self.collected),
            failed: get(&self.failed),
            timed_out: get(&self.timed_out),
            panicked: get(&self.panicked),
            invalid: get(&self.invalid),
            dropped: get(&self.dropped),
        }
    }
}

/// Sensors to run, built before [`Scheduler::start`].
pub struct Scheduler {
    options: SchedulerOptions,
    sensors: Vec<(SensorConfig, Arc<dyn Sensor>)>,
    keys: BTreeSet<(String, String)>,
}

impl Scheduler {
    pub fn new(options: SchedulerOptions) -> Self {
        Self {
            options,
            sensors: Vec::new(),
            keys: BTreeSet::new(),
        }
    }

    /// Adds a sensor after validating its configuration; `(name, instance)`
    /// must be unique.
    pub fn add(
        &mut self,
        config: SensorConfig,
        sensor: Arc<dyn Sensor>,
    ) -> Result<(), ValidationError> {
        config.validate()?;
        if !self
            .keys
            .insert((config.name.clone(), config.instance.clone()))
        {
            return Err(ValidationError::Duplicate {
                name: config.name,
                instance: config.instance,
            });
        }
        self.sensors.push((config, sensor));
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.sensors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sensors.is_empty()
    }

    /// Spawns one task per sensor on the current runtime. The receiver ends
    /// once every sensor task has stopped.
    pub fn start(self) -> (SchedulerHandle, mpsc::Receiver<Sample>) {
        let (tx, rx) = mpsc::channel(self.options.capacity.max(1));
        let (stop_tx, stop_rx) = watch::channel(false);
        let counters = Arc::new(Counters::default());
        let mut tasks = JoinSet::new();
        for (config, sensor) in self.sensors {
            let first = if self.options.jitter {
                jitter(config.interval)
            } else {
                Duration::ZERO
            };
            let task = SensorTask {
                config,
                sensor,
                tx: tx.clone(),
                counters: counters.clone(),
                stop: stop_rx.clone(),
            };
            tasks.spawn(task.run(first));
        }
        let handle = SchedulerHandle {
            stop: stop_tx,
            tasks,
            counters,
        };
        (handle, rx)
    }
}

/// Controls a started [`Scheduler`].
pub struct SchedulerHandle {
    stop: watch::Sender<bool>,
    tasks: JoinSet<()>,
    counters: Arc<Counters>,
}

impl SchedulerHandle {
    pub fn stats(&self) -> Stats {
        self.counters.snapshot()
    }

    /// Cancels running collections and waits until every sensor task ended.
    pub async fn shutdown(mut self) {
        self.stop.send_replace(true);
        while self.tasks.join_next().await.is_some() {}
    }
}

/// A random delay below `interval`. Not security sensitive: std's
/// randomly keyed hasher is enough to spread a fleet.
fn jitter(interval: Duration) -> Duration {
    use std::hash::BuildHasher;
    let random = std::collections::hash_map::RandomState::new().hash_one(());
    // MAX_INTERVAL in nanoseconds fits a u64 by far.
    let nanos = u64::try_from(interval.as_nanos())
        .unwrap_or(u64::MAX)
        .max(1);
    Duration::from_nanos(random % nanos)
}

/// Aborts the collection task if the sensor task itself is dropped.
struct AbortOnDrop(AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Allows one log line per [`LOG_EVERY`], counting the ones held back.
#[derive(Debug, Default)]
struct Limiter {
    last: Option<Instant>,
    suppressed: u64,
}

impl Limiter {
    /// `Some(suppressed since the last line)` if a line may be logged now.
    fn allow(&mut self) -> Option<u64> {
        let now = Instant::now();
        match self.last {
            Some(last) if now.duration_since(last) < LOG_EVERY => {
                self.suppressed += 1;
                None
            }
            _ => {
                self.last = Some(now);
                Some(std::mem::take(&mut self.suppressed))
            }
        }
    }
}

struct SensorTask {
    config: SensorConfig,
    sensor: Arc<dyn Sensor>,
    tx: mpsc::Sender<Sample>,
    counters: Arc<Counters>,
    stop: watch::Receiver<bool>,
}

/// How one collection ended, when it did not return samples.
enum Problem {
    Failed(SensorError),
    TimedOut(Duration),
    Panicked,
    /// Not reported: the scheduler is stopping.
    Shutdown,
}

impl SensorTask {
    async fn run(mut self, first: Duration) {
        let mut ticker = interval_at(Instant::now() + first, self.config.interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let (mut failing, mut failures, mut invalid, mut dropped) = (
            false,
            Limiter::default(),
            Limiter::default(),
            Limiter::default(),
        );
        loop {
            tokio::select! {
                biased;
                _ = self.stop.changed() => return,
                _ = ticker.tick() => {}
            }
            let samples = match self.collect().await {
                None => return,
                Some(Ok(samples)) => samples,
                Some(Err(problem)) => {
                    failing = true;
                    if self.report(problem, &mut failures) {
                        return;
                    }
                    continue;
                }
            };
            Counters::bump(&self.counters.collected);
            if std::mem::take(&mut failing) {
                tracing::info!(sensor = %self.config.name, instance = %self.config.instance, "sensor recovered");
                failures = Limiter::default();
            }
            for mut sample in samples {
                sample.sensor.clone_from(&self.config.name);
                if sample.instance.is_empty() {
                    sample.instance.clone_from(&self.config.instance);
                }
                if let Err(error) = validate::sample(&sample) {
                    Counters::bump(&self.counters.invalid);
                    if let Some(suppressed) = invalid.allow() {
                        tracing::warn!(sensor = %self.config.name, instance = %self.config.instance, %error, suppressed, "dropping invalid sample");
                    }
                    continue;
                }
                match self.tx.try_send(sample) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        Counters::bump(&self.counters.dropped);
                        if let Some(suppressed) = dropped.allow() {
                            tracing::warn!(sensor = %self.config.name, instance = %self.config.instance, suppressed, "sample channel full, dropping samples");
                        }
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => return,
                }
            }
        }
    }

    /// One collection in its own task, so a panic stays in it. `None` on
    /// shutdown.
    async fn collect(&mut self) -> Option<Result<Vec<Sample>, Problem>> {
        let timeout = self.config.timeout();
        let deadline = Instant::now() + timeout;
        let sensor = self.sensor.clone();
        let mut task = tokio::spawn(async move { sensor.collect(deadline).await });
        let _guard = AbortOnDrop(task.abort_handle());
        let outcome = tokio::select! {
            biased;
            _ = self.stop.changed() => None,
            outcome = timeout_at(deadline, &mut task) => Some(outcome),
        };
        let result = match outcome {
            Some(Ok(Ok(Ok(samples)))) => Ok(samples),
            Some(Ok(Ok(Err(error)))) => Err(Problem::Failed(error)),
            Some(Ok(Err(_))) => Err(Problem::Panicked),
            Some(Err(_)) => Err(Problem::TimedOut(timeout)),
            None => Err(Problem::Shutdown),
        };
        if matches!(result, Err(Problem::TimedOut(_) | Problem::Shutdown)) {
            // Cancel and wait, so runs of one sensor never overlap.
            task.abort();
            let _ = task.await;
        }
        match result {
            Err(Problem::Shutdown) => None,
            result => Some(result),
        }
    }

    /// Counts and logs a problem; true if the sensor must stop.
    fn report(&self, problem: Problem, limiter: &mut Limiter) -> bool {
        let (name, instance) = (&self.config.name, &self.config.instance);
        match problem {
            Problem::Failed(error) if error.is_permanent() => {
                Counters::bump(&self.counters.failed);
                tracing::error!(sensor = %name, instance = %instance, %error, "sensor failed permanently, stopping it");
                return true;
            }
            Problem::Failed(error) => {
                Counters::bump(&self.counters.failed);
                if let Some(suppressed) = limiter.allow() {
                    tracing::warn!(sensor = %name, instance = %instance, %error, suppressed, "sensor collection failed");
                }
            }
            Problem::TimedOut(timeout) => {
                Counters::bump(&self.counters.timed_out);
                if let Some(suppressed) = limiter.allow() {
                    tracing::warn!(sensor = %name, instance = %instance, ?timeout, suppressed, "sensor collection timed out");
                }
            }
            Problem::Panicked => {
                Counters::bump(&self.counters.panicked);
                if let Some(suppressed) = limiter.allow() {
                    tracing::error!(sensor = %name, instance = %instance, suppressed, "sensor collection panicked");
                }
            }
            Problem::Shutdown => return true,
        }
        false
    }
}

#[cfg(test)]
mod tests;
