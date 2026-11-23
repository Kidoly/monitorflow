//! Traffic flows (ADR-0184). Probes add up the NetFlow, sFlow and IPFIX
//! of the devices set as exporters and send one report per device and
//! minute; this module keeps them in PostgreSQL ([`store`]), sums minutes
//! older than [`MINUTES_KEPT`] into hours and drops hours older than
//! [`HOURS_KEPT`] ([`maintain`]), and turns each report into the series
//! thresholds watch: the traffic of each address in the window, and the
//! device's whole traffic ([`alert_samples`]).

pub mod query;
pub mod store;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use monitorflow_common::proto::v1::{FlowReport, Sample as ProtoSample};
use sqlx::PgPool;
use tokio::sync::mpsc;
use tokio::time::{interval, MissedTickBehavior};

/// Series of the traffic of one address, sent or received, in bits per
/// second, with the address as instance; and of the whole device.
pub const ADDRESS_METRIC: &str = "flow_address_bits_per_second";
pub const TOTAL_METRIC: &str = "flow_bits_per_second";

/// How long minutes are kept before they become hours, and hours kept.
pub const MINUTES_KEPT: Duration = Duration::from_secs(48 * 3600);
pub const HOURS_KEPT: Duration = Duration::from_secs(31 * 24 * 3600);
/// Keys of each dimension, and conversations, kept per hour.
pub const HOURLY_TOP: i64 = 20;
pub const HOURLY_CONVERSATIONS: i64 = 100;
/// Time between two runs of [`maintain`].
pub const MAINTAIN_EVERY: Duration = Duration::from_secs(600);
/// Batches of reports waiting to be written; more are dropped and logged.
const WRITE_QUEUE: usize = 256;

/// Queues reports for writing; clones share the queue.
#[derive(Debug, Clone)]
pub struct FlowWriter {
    tx: mpsc::Sender<Vec<FlowReport>>,
}

impl FlowWriter {
    /// Starts the task that writes reports into `pool`.
    pub fn start(pool: PgPool) -> Self {
        let (tx, mut rx) = mpsc::channel::<Vec<FlowReport>>(WRITE_QUEUE);
        tokio::spawn(async move {
            while let Some(reports) = rx.recv().await {
                if let Err(error) = store::insert(&pool, &reports, chrono::Utc::now()).await {
                    tracing::warn!(%error, reports = reports.len(), "could not write traffic flows");
                }
            }
        });
        Self { tx }
    }

    /// A writer whose reports go to `tx`, for tests.
    pub fn from_sender(tx: mpsc::Sender<Vec<FlowReport>>) -> Self {
        Self { tx }
    }

    pub fn submit(&self, reports: Vec<FlowReport>) {
        if reports.is_empty() {
            return;
        }
        if self.tx.try_send(reports).is_err() {
            tracing::warn!("traffic flows dropped: the database is behind");
        }
    }
}

/// The addresses each device's last report named, so an address that
/// drops out of the largest gets a 0 and its incident can resolve.
#[derive(Debug, Default)]
pub struct Seen(HashMap<String, HashSet<String>>);

/// The samples of `report`: one per address among its largest sources
/// and destinations, the bytes it sent and received over the window in
/// bits per second; 0 for the addresses of the device's previous report
/// that are not in this one; and the device's whole traffic.
pub fn alert_samples(report: &FlowReport, seen: &mut Seen) -> Vec<ProtoSample> {
    let seconds = f64::from(report.seconds.max(1));
    let at = report
        .start_unix_ms
        .saturating_add(i64::from(report.seconds) * 1000);
    let mut addresses: BTreeMap<&str, u64> = BTreeMap::new();
    for top in &report.tops {
        if top.dimension == "src" || top.dimension == "dst" {
            let bytes = addresses.entry(top.key.as_str()).or_default();
            *bytes = bytes.saturating_add(top.bytes);
        }
    }
    let sample = |sensor: &str, instance: &str, value: f64| ProtoSample {
        sensor: sensor.into(),
        instance: instance.into(),
        timestamp_unix_ms: at,
        value,
        labels: Default::default(),
    };
    let mut samples: Vec<ProtoSample> = addresses
        .iter()
        .map(|(address, bytes)| sample(ADDRESS_METRIC, address, *bytes as f64 * 8.0 / seconds))
        .collect();
    let now: HashSet<String> = addresses.keys().map(|a| (*a).to_owned()).collect();
    let before = seen
        .0
        .insert(report.device.clone(), now.clone())
        .unwrap_or_default();
    samples.extend(
        before
            .difference(&now)
            .map(|gone| sample(ADDRESS_METRIC, gone, 0.0)),
    );
    samples.push(sample(
        TOTAL_METRIC,
        "",
        report.bytes as f64 * 8.0 / seconds,
    ));
    samples
}

/// Sums old minutes into hours and drops what is past [`HOURS_KEPT`],
/// every [`MAINTAIN_EVERY`] until `until`. Runs on the leader.
pub async fn maintain(pool: PgPool, until: impl std::future::Future<Output = ()>) {
    tokio::pin!(until);
    let mut tick = interval(MAINTAIN_EVERY);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = &mut until => return,
            _ = tick.tick() => {}
        }
        match store::maintain(&pool, chrono::Utc::now()).await {
            Ok(done) if done.any() => tracing::info!(?done, "traffic flows summed into hours"),
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(%error, "could not sum or drop old traffic flows; will retry")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use monitorflow_common::proto::v1::FlowTop;

    use super::*;

    fn top(dimension: &str, key: &str, bytes: u64) -> FlowTop {
        FlowTop {
            dimension: dimension.into(),
            key: key.into(),
            bytes,
            packets: 1,
        }
    }

    #[test]
    fn addresses_add_both_ways_and_go_to_zero() {
        let mut seen = Seen::default();
        let report = FlowReport {
            device: "sw".into(),
            start_unix_ms: 60_000,
            seconds: 60,
            bytes: 15_000,
            packets: 3,
            tops: vec![
                top("src", "192.0.2.1", 7_500),
                top("dst", "192.0.2.1", 7_500),
                top("dst", "198.51.100.1", 7_500),
                top("app", "tcp/443", 15_000),
            ],
            conversations: vec![],
        };
        let samples = alert_samples(&report, &mut seen);
        let value =
            |s: &[ProtoSample], i: &str| s.iter().find(|s| s.instance == i).map(|s| s.value);
        assert_eq!(
            value(&samples, "192.0.2.1"),
            Some(2_000.0),
            "15 kB in a minute"
        );
        assert_eq!(value(&samples, "198.51.100.1"), Some(1_000.0));
        assert_eq!(value(&samples, ""), Some(2_000.0));
        assert!(samples.iter().all(|s| s.timestamp_unix_ms == 120_000));
        let next = FlowReport {
            start_unix_ms: 120_000,
            tops: vec![top("src", "192.0.2.1", 60)],
            ..report
        };
        let samples = alert_samples(&next, &mut seen);
        assert_eq!(
            value(&samples, "198.51.100.1"),
            Some(0.0),
            "gone: back to 0"
        );
        assert_eq!(value(&samples, "192.0.2.1"), Some(8.0));
    }
}
