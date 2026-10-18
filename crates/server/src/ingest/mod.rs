//! Ingest of agent data into VictoriaMetrics and VictoriaLogs (ADR-0013):
//! validation and identity labels, then background writers with bounded
//! queues so storage never slows the gRPC streams down.

pub mod validate;

pub use validate::{check_batch, Checked, Identity, Invalid, Series};
