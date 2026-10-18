//! Runs the sensor [`Scheduler`] until shutdown and consumes its samples.
//!
//! For now the scheduler has no sensors (the cpu, memory and disk sensors
//! and the configuration pushed by the server are later tasks) and samples
//! are only logged at debug level by [`log_samples`]. The upload task
//! replaces [`log_samples`] with a consumer that batches samples into
//! `SampleBatch` envelopes on the stream; nothing else here changes.

use monitorflow_sensors::{Sample, Scheduler, SchedulerOptions, Stats};
use tokio::sync::mpsc;

use crate::shutdown::Shutdown;

/// The sensors this agent runs.
pub fn scheduler() -> Scheduler {
    Scheduler::new(SchedulerOptions::default())
}

/// Runs `scheduler` until `stop` fires, then stops the sensors and lets the
/// consumer finish the queued samples. Returns the scheduler's counters and
/// the number of samples consumed.
pub async fn run(scheduler: Scheduler, mut stop: Shutdown) -> (Stats, u64) {
    let count = scheduler.len();
    let (handle, samples) = scheduler.start();
    tracing::info!(sensors = count, "sensors started");
    let consumer = tokio::spawn(log_samples(samples));
    stop.wait().await;
    let stats = handle.stats();
    handle.shutdown().await;
    // The channel closed with the last sensor task, so this ends.
    let consumed = consumer.await.unwrap_or_default();
    tracing::info!(?stats, "sensors stopped");
    (stats, consumed)
}

/// Placeholder consumer: logs every sample at debug level until the
/// channel closes, and returns how many it saw.
pub async fn log_samples(mut samples: mpsc::Receiver<Sample>) -> u64 {
    let mut count = 0;
    while let Some(sample) = samples.recv().await {
        count += 1;
        tracing::debug!(
            sensor = %sample.sensor,
            instance = %sample.instance,
            value = sample.value,
            timestamp_unix_ms = sample.timestamp_unix_ms,
            labels = ?sample.labels,
            "sample"
        );
    }
    count
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use monitorflow_sensors::testing::FakeSensor;
    use monitorflow_sensors::SensorConfig;

    use super::*;
    use crate::shutdown;

    #[tokio::test(start_paused = true)]
    async fn the_default_scheduler_is_empty_and_stops() {
        assert!(scheduler().is_empty());
        let (trigger, stop) = shutdown::channel();
        let task = tokio::spawn(run(scheduler(), stop));
        trigger.fire();
        let (stats, consumed) = task.await.unwrap();
        assert_eq!((stats, consumed), (Stats::default(), 0));
    }

    #[tokio::test(start_paused = true)]
    async fn samples_are_consumed_until_shutdown() {
        let mut scheduler = Scheduler::new(SchedulerOptions {
            jitter: false,
            ..SchedulerOptions::default()
        });
        let sensor = Arc::new(FakeSensor::new());
        let config = SensorConfig::new("fake", Duration::from_secs(10));
        scheduler.add(config, sensor.clone()).unwrap();
        let (trigger, stop) = shutdown::channel();
        let task = tokio::spawn(run(scheduler, stop));
        tokio::time::sleep(Duration::from_secs(25)).await;
        trigger.fire();
        let (stats, consumed) = task.await.unwrap();
        assert_eq!(sensor.calls(), 3);
        assert_eq!((stats.collected, consumed), (3, 3));
    }
}
