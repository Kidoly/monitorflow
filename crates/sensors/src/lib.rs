//! Sensor trait and implementations. This crate has no network code so it can
//! be reused by future agents on other platforms.

use std::collections::BTreeMap;

/// One reading produced by a sensor.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub sensor: String,
    pub instance: String,
    pub timestamp_unix_ms: i64,
    pub value: f64,
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SensorError {
    #[error("collection failed: {0}")]
    Collection(String),
}

/// A source of samples, polled on an interval by the agent.
#[async_trait::async_trait]
pub trait Sensor: Send + Sync {
    /// Stable name, e.g. "cpu", "disk".
    fn name(&self) -> &str;
    /// Collect one round of samples.
    async fn collect(&self) -> Result<Vec<Sample>, SensorError>;
}
