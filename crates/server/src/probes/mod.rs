//! Network checks run from a probe (ADR-0148): rows of `sensor_configs`
//! whose `probe` is the central server or a relay, instead of the server's
//! agent.
//!
//! [`Probes::run_relays`] runs on every instance and sends each relay
//! whose stream it holds the checks it must run, as a `ProbeConfig` on the
//! relay's own stream, whenever they change and on every new connection.
//! [`Probes::run_central`] runs the central server's own checks, on the
//! leader only (ADR-0089), through the same [`monitorflow_probe::Runner`]
//! a relay uses. Results of both go through [`Probes::ingest`]: the series
//! of the device they are about, with a `probe` label naming who measured
//! them, into VictoriaMetrics and the threshold engine, as an agent's.
//!
//! A relay is believed only for the devices of the last configuration it
//! was sent: what it says about others is dropped and counted.
//!
//! The same configuration names the devices whose traffic flows each
//! probe receives (ADR-0184). Their reports go to [`crate::flows`], and
//! the series thresholds watch to the same place as checks' samples.

mod store;

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use chrono::Utc;
use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::{
    FlowExporter, FlowReport, FlowReports, ProbeConfig, ProbeConfigAck, ProbeResults, ProbeTest,
    ProbeTestResult, Sample as ProtoSample, SampleBatch,
};
use monitorflow_common::tls::PeerRole;
use monitorflow_flow::collector::{self, Exporter};
use monitorflow_probe::{Check, ProbeFactory, Runner, CENTRAL};
use tokio::time::{interval, timeout, MissedTickBehavior};

pub use store::ProbeStore;

use crate::alerting::Alerts;
use crate::flows::{FlowWriter, Seen};
use crate::grpc::Registry;
use crate::ingest::{self, Identity, Ingest};

/// Time between two reads of the probe checks.
pub const POLL: Duration = Duration::from_secs(5);
/// Time allowed to queue a configuration on a relay's stream.
const SEND_TIMEOUT: Duration = Duration::from_secs(1);
/// The label naming who measured a series: `central` or a relay's ID.
pub const PROBE_LABEL: &str = "probe";

/// What a probe runs: its checks, and the site of each device.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProbeSet {
    pub checks: Vec<Check>,
    /// Device ID to site name, for the series' `site` label.
    pub sites: BTreeMap<String, String>,
    /// Devices whose traffic flows the probe receives (ADR-0184).
    pub exporters: Vec<Exporter>,
}

impl ProbeSet {
    /// A revision that changes when the checks do.
    pub fn revision(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for check in &self.checks {
            check.device.hash(&mut hasher);
            check.kind.hash(&mut hasher);
            check.config.name.hash(&mut hasher);
            check.config.instance.hash(&mut hasher);
            check.config.interval.hash(&mut hasher);
            check.config.timeout.hash(&mut hasher);
            check.config.params.hash(&mut hasher);
        }
        for exporter in &self.exporters {
            exporter.device.hash(&mut hasher);
            exporter.addresses.hash(&mut hasher);
        }
        // Never 0, which reads as "nothing sent".
        hasher.finish().max(1)
    }

    pub fn to_proto(&self) -> ProbeConfig {
        ProbeConfig {
            revision: self.revision(),
            checks: self.checks.iter().map(Check::to_proto).collect(),
            flow_exporters: self
                .exporters
                .iter()
                .map(|e| FlowExporter {
                    device: e.device.clone(),
                    addresses: e.addresses.clone(),
                })
                .collect(),
        }
    }
}

/// Counters since the start, for tests and logs.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ProbeStats {
    pub samples: u64,
    /// Flow reports taken (ADR-0184).
    pub flow_reports: u64,
    /// About a device the probe was not given.
    pub unknown_device: u64,
    pub invalid: u64,
}

#[derive(Default)]
struct Shared {
    /// For each relay, the devices of the configuration it was last sent,
    /// and their sites.
    relays: HashMap<String, BTreeMap<String, String>>,
    stats: ProbeStats,
    /// Addresses of each exporter's last flow report.
    seen: Seen,
}

/// Shared handle; clones see the same state.
#[derive(Clone)]
pub struct Probes {
    store: ProbeStore,
    ingest: Ingest,
    alerts: Option<Alerts>,
    shared: Arc<RwLock<Shared>>,
    /// Wakes [`Probes::run_relays`] when a relay's stream opens.
    opened: Arc<tokio::sync::Notify>,
    /// Builds the sensors of the central server's one-off tests.
    factory: Arc<dyn ProbeFactory>,
    /// Tests sent to relays, by ID: the relay, and who waits.
    tests: Arc<std::sync::Mutex<HashMap<String, Waiting>>>,
    /// Where traffic flows go, and how the central server receives its
    /// own (ADR-0184); without it, flow reports are dropped.
    flows: Option<(FlowWriter, collector::Settings)>,
}

/// A test sent to a relay: the relay, and who waits for its answer.
type Waiting = (String, tokio::sync::oneshot::Sender<ProbeTestResult>);

/// Why a one-off test did not run.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TestError {
    /// The relay's stream is not held by this central server.
    #[error("the relay is not connected to this central server")]
    NotConnected,
    #[error("the relay did not answer in time")]
    NoAnswer,
    /// The check ran and failed, or could not run.
    #[error("{0}")]
    Failed(String),
}

impl std::fmt::Debug for Probes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Probes").finish_non_exhaustive()
    }
}

impl Probes {
    pub fn new(store: ProbeStore, ingest: Ingest, alerts: Option<Alerts>) -> Self {
        Self {
            store,
            ingest,
            alerts,
            shared: Arc::default(),
            opened: Arc::default(),
            factory: Arc::new(monitorflow_probe::Builtin),
            tests: Arc::default(),
            flows: None,
        }
    }

    /// Writes traffic flows with `writer`; the central server listens for
    /// its own exporters as `settings` say.
    pub fn with_flows(mut self, writer: FlowWriter, settings: collector::Settings) -> Self {
        self.flows = Some((writer, settings));
        self
    }

    /// Builds the central server's tests with `factory` instead of the
    /// built-in kinds.
    pub fn with_factory(mut self, factory: Arc<dyn ProbeFactory>) -> Self {
        self.factory = factory;
        self
    }

    /// Runs `check` once from `probe` ([`CENTRAL`] or a relay's ID) and
    /// returns what it measured (ADR-0149): on this central server, or on
    /// the relay through its stream, which `registry` must hold.
    pub async fn test(
        &self,
        registry: &Registry,
        probe: &str,
        check: Check,
    ) -> Result<Vec<ProtoSample>, TestError> {
        if probe == CENTRAL {
            return monitorflow_probe::test_once(&*self.factory, &check)
                .await
                .map(|samples| {
                    samples
                        .into_iter()
                        .map(monitorflow_probe::sample_to_proto)
                        .collect()
                })
                .map_err(TestError::Failed);
        }
        if registry.get(PeerRole::Relay, probe).is_none() {
            return Err(TestError::NotConnected);
        }
        let id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.tests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), (probe.to_owned(), tx));
        let wait = check.config.timeout().min(monitorflow_probe::MAX_TEST) + Duration::from_secs(5);
        let envelope = registry.envelope(Payload::ProbeTest(ProbeTest {
            id: id.clone(),
            check: Some(check.to_proto()),
        }));
        let outcome = match timeout(
            SEND_TIMEOUT,
            registry.send(PeerRole::Relay, probe, envelope),
        )
        .await
        {
            Ok(Ok(())) => match timeout(wait, rx).await {
                Ok(Ok(result)) if result.error.is_empty() => Ok(result.samples),
                Ok(Ok(result)) => Err(TestError::Failed(result.error.chars().take(512).collect())),
                Ok(Err(_)) | Err(_) => Err(TestError::NoAnswer),
            },
            Ok(Err(_)) | Err(_) => Err(TestError::NotConnected),
        };
        self.tests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
        outcome
    }

    /// The answer of `relay` to one of its tests; answers to tests it was
    /// not sent are dropped.
    pub fn test_result(&self, relay: &str, result: ProbeTestResult) {
        let mut tests = self.tests.lock().unwrap_or_else(|e| e.into_inner());
        if tests.get(&result.id).is_some_and(|(to, _)| to == relay) {
            if let Some((_, tx)) = tests.remove(&result.id) {
                let _ = tx.send(result);
            }
        }
    }

    /// A relay's stream opened: it gets its checks now, not at the next
    /// poll.
    pub fn relay_connected(&self) {
        self.opened.notify_one();
    }

    pub fn stats(&self) -> ProbeStats {
        self.shared.read().unwrap_or_else(|e| e.into_inner()).stats
    }

    /// Writes the `samples` the probe `probe` measured on `device` of site
    /// `site`: validated as an agent's, labelled with the probe, then
    /// queued for storage and the threshold engine.
    fn write(&self, probe: &str, device: &str, site: &str, samples: Vec<ProtoSample>) {
        let identity = Identity {
            peer: device.to_owned(),
            site: site.to_owned(),
        };
        let now = Utc::now().timestamp_millis();
        let mut checked = ingest::check_batch(&identity, SampleBatch { samples }, now);
        for series in &mut checked.series {
            series.labels.push((PROBE_LABEL.into(), probe.to_owned()));
            series.labels.sort_unstable();
        }
        {
            let mut shared = self.shared.write().unwrap_or_else(|e| e.into_inner());
            shared.stats.samples += checked.series.len() as u64;
            shared.stats.invalid += checked.invalid;
        }
        if let Some(alerts) = &self.alerts {
            alerts.submit(&checked.series);
        }
        self.ingest.samples(checked);
    }

    /// Results a relay sent on its own stream. Devices it was not given
    /// are dropped.
    pub fn ingest(&self, relay: &str, results: ProbeResults) {
        let sites = {
            let shared = self.shared.read().unwrap_or_else(|e| e.into_inner());
            shared.relays.get(relay).cloned().unwrap_or_default()
        };
        for device in results.devices {
            match sites.get(&device.device) {
                Some(site) => self.write(relay, &device.device, site, device.samples),
                None => {
                    let mut shared = self.shared.write().unwrap_or_else(|e| e.into_inner());
                    shared.stats.unknown_device += device.samples.len() as u64;
                    tracing::debug!(relay, device = %device.device.escape_debug(), "probe results about a device this relay was not given; dropped");
                }
            }
        }
    }

    /// Flow reports a relay sent on its own stream; devices it was not
    /// given are dropped.
    pub fn flow_reports(&self, relay: &str, reports: FlowReports) {
        let sites = {
            let shared = self.shared.read().unwrap_or_else(|e| e.into_inner());
            shared.relays.get(relay).cloned().unwrap_or_default()
        };
        self.take_flows(relay, &sites, reports.reports);
    }

    /// Writes the flow `reports` of `probe` about devices of `sites`, and
    /// the series thresholds watch.
    fn take_flows(&self, probe: &str, sites: &BTreeMap<String, String>, reports: Vec<FlowReport>) {
        let Some((writer, _)) = &self.flows else {
            return;
        };
        let mut kept = Vec::with_capacity(reports.len());
        for report in reports {
            let Some(site) = sites.get(&report.device) else {
                let mut shared = self.shared.write().unwrap_or_else(|e| e.into_inner());
                shared.stats.unknown_device += 1;
                tracing::debug!(probe, device = %report.device.escape_debug(), "flows about a device this probe was not given; dropped");
                continue;
            };
            let samples = {
                let mut shared = self.shared.write().unwrap_or_else(|e| e.into_inner());
                shared.stats.flow_reports += 1;
                crate::flows::alert_samples(&report, &mut shared.seen)
            };
            self.write(probe, &report.device, site, samples);
            kept.push(report);
        }
        writer.submit(kept);
    }

    /// Logs the checks a relay could not start.
    pub fn acknowledged(&self, relay: &str, ack: &ProbeConfigAck) {
        if let Some(first) = ack.errors.first() {
            tracing::warn!(
                relay,
                revision = ack.revision,
                refused = ack.errors.len(),
                device = %first.device.escape_debug(),
                check = %first.name.escape_debug(),
                error = %first.message.chars().take(256).collect::<String>().escape_debug(),
                "relay could not start probe checks"
            );
        }
    }

    /// The relay's stream closed: what it says next comes with a new
    /// configuration.
    pub fn forget(&self, relay: &str) {
        let mut shared = self.shared.write().unwrap_or_else(|e| e.into_inner());
        shared.relays.remove(relay);
    }

    /// Sends the relays whose streams this instance holds their checks,
    /// every [`POLL`], until `until`.
    pub async fn run_relays(
        self,
        registry: Registry,
        until: impl std::future::Future<Output = ()>,
    ) {
        tokio::pin!(until);
        let mut tick = interval(POLL);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        // Relay ID to (connection, revision) last sent.
        let mut sent: HashMap<String, (u64, u64)> = HashMap::new();
        loop {
            tokio::select! {
                () = &mut until => return,
                _ = tick.tick() => {}
                () = self.opened.notified() => {}
            }
            let relays: Vec<_> = registry
                .connected()
                .into_iter()
                .filter(|p| p.role == PeerRole::Relay)
                .collect();
            sent.retain(|id, _| relays.iter().any(|r| &r.id == id));
            if relays.is_empty() {
                continue;
            }
            let mut sets = match self.store.load().await {
                Ok(sets) => sets,
                Err(error) => {
                    tracing::warn!(%error, "could not read the probe checks; will retry");
                    continue;
                }
            };
            for relay in relays {
                let set = sets.remove(&relay.id).unwrap_or_default();
                let revision = set.revision();
                if sent.get(&relay.id) == Some(&(relay.connection, revision)) {
                    continue;
                }
                {
                    // Believed from now: results of the old set that are
                    // still on their way are of devices it had.
                    let mut shared = self.shared.write().unwrap_or_else(|e| e.into_inner());
                    let known = shared.relays.entry(relay.id.clone()).or_default();
                    known.extend(set.sites.clone());
                }
                let envelope = registry.envelope(Payload::ProbeConfig(set.to_proto()));
                match timeout(
                    SEND_TIMEOUT,
                    registry.send(PeerRole::Relay, &relay.id, envelope),
                )
                .await
                {
                    Ok(Ok(())) => {
                        tracing::info!(relay = %relay.id, checks = set.checks.len(), revision, "sent probe checks");
                        sent.insert(relay.id.clone(), (relay.connection, revision));
                        let mut shared = self.shared.write().unwrap_or_else(|e| e.into_inner());
                        shared.relays.insert(relay.id, set.sites);
                    }
                    Ok(Err(error)) => {
                        tracing::debug!(relay = %relay.id, %error, "probe checks not sent")
                    }
                    Err(_) => {
                        tracing::debug!(relay = %relay.id, "outbound queue full; probe checks not sent")
                    }
                }
            }
        }
    }

    /// Runs the central server's own checks with sensors from `factory`
    /// until `until`, reloading them every [`POLL`].
    pub async fn run_central(
        self,
        factory: Arc<dyn ProbeFactory>,
        until: impl std::future::Future<Output = ()>,
    ) {
        tokio::pin!(until);
        let (mut runner, mut samples) = Runner::new(factory);
        let mut tick = interval(POLL);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut current: Option<ProbeSet> = None;
        // The central server's own flow listeners (ADR-0184), open while
        // some device sends it flows.
        let (exporters, exporters_rx) = tokio::sync::watch::channel(Vec::new());
        let (reports_tx, mut reports) = tokio::sync::mpsc::channel::<FlowReports>(4);
        let collector = self.flows.as_ref().map(|(_, settings)| {
            tokio::spawn(collector::run(
                settings.clone(),
                exporters_rx,
                reports_tx,
                Arc::default(),
            ))
        });
        loop {
            tokio::select! {
                () = &mut until => break,
                Some(batch) = reports.recv() => {
                    let sites = current.as_ref().map(|s| s.sites.clone()).unwrap_or_default();
                    self.take_flows(CENTRAL, &sites, batch.reports);
                }
                batch = monitorflow_probe::next_batch(&mut samples) => {
                    let Some(batch) = batch else { break };
                    let sites = current.as_ref().map(|s| &s.sites);
                    let mut by_device: BTreeMap<String, Vec<ProtoSample>> = BTreeMap::new();
                    for sample in batch {
                        by_device.entry(sample.device).or_default().push(to_proto(sample.sample));
                    }
                    for (device, samples) in by_device {
                        if let Some(site) = sites.and_then(|s| s.get(&device)) {
                            self.write(CENTRAL, &device, site, samples);
                        }
                    }
                }
                _ = tick.tick() => {
                    let set = match self.store.load().await {
                        Ok(mut sets) => sets.remove(CENTRAL).unwrap_or_default(),
                        Err(error) => {
                            tracing::warn!(%error, "could not read the probe checks; will retry");
                            continue;
                        }
                    };
                    if current.as_ref() == Some(&set) {
                        continue;
                    }
                    exporters.send_if_modified(|now| {
                        let changed = *now != set.exporters;
                        if changed {
                            tracing::info!(exporters = set.exporters.len(), "central flow exporters applied");
                            now.clone_from(&set.exporters);
                        }
                        changed
                    });
                    let errors = runner.apply(set.checks.clone()).await;
                    for error in &errors {
                        tracing::warn!(device = %error.device, check = %error.name, error = %error.message, "probe check could not start");
                    }
                    tracing::info!(checks = set.checks.len(), devices = runner.devices(), "central probe checks applied");
                    current = Some(set);
                }
            }
        }
        if let Some(collector) = collector {
            collector.abort();
        }
        runner.shutdown().await;
    }
}

fn to_proto(sample: monitorflow_sensors::Sample) -> ProtoSample {
    ProtoSample {
        sensor: sample.sensor,
        instance: sample.instance,
        timestamp_unix_ms: sample.timestamp_unix_ms,
        value: sample.value,
        labels: sample.labels.into_iter().collect(),
    }
}

#[cfg(test)]
mod tests;
