//! Ingest of agent data into VictoriaMetrics and VictoriaLogs (ADR-0013):
//! validation and identity labels, then background writers with bounded
//! queues so storage never slows the gRPC streams down.

mod http;
pub mod logs;
pub mod metrics;
pub mod validate;
pub mod writer;

pub use logs::{JsonLines, LogRecord};
pub use metrics::RemoteWrite;
pub use validate::{check_batch, Checked, Identity, Invalid, Series};
pub use writer::{Queue, RetrySettings, Sink, Stats, WriteError, Writer, WriterSettings};

/// Where agent streams send series and events. A missing queue discards
/// what would go to it; tests and tools run without storage that way.
#[derive(Clone, Default)]
pub struct Ingest {
    pub metrics: Option<Queue<Series>>,
    pub events: Option<Queue<LogRecord>>,
}

impl Ingest {
    /// Queues the valid series of a batch and counts the invalid ones.
    /// Returns how many series were dropped because the queue was full.
    pub fn samples(&self, checked: Checked) -> u64 {
        let Some(queue) = &self.metrics else {
            return 0;
        };
        if checked.invalid > 0 {
            queue.reject(checked.invalid);
        }
        checked
            .series
            .into_iter()
            .map(|series| queue.push(series))
            .filter(|&queued| !queued)
            .count() as u64
    }

    pub fn event(&self, record: LogRecord) {
        if let Some(queue) = &self.events {
            queue.push(record);
        }
    }
}
