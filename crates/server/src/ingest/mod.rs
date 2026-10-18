//! Ingest of agent data into VictoriaMetrics and VictoriaLogs (ADR-0013):
//! validation and identity labels, then background writers with bounded
//! queues so storage never slows the gRPC streams down.

mod http;
pub mod metrics;
pub mod validate;
pub mod writer;

pub use metrics::RemoteWrite;
pub use validate::{check_batch, Checked, Identity, Invalid, Series};
pub use writer::{Queue, RetrySettings, Sink, Stats, WriteError, Writer, WriterSettings};
