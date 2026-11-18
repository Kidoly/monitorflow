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

mod store;

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use chrono::Utc;
use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::{
    ProbeConfig, ProbeConfigAck, ProbeResults, Sample as ProtoSample, SampleBatch,
};
use monitorflow_common::tls::PeerRole;
use monitorflow_probe::{Check, ProbeFactory, Runner, CENTRAL};
use tokio::time::{interval, timeout, MissedTickBehavior};

pub use store::ProbeStore;

use crate::alerting::Alerts;
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
        // Never 0, which reads as "nothing sent".
        hasher.finish().max(1)
    }

    pub fn to_proto(&self) -> ProbeConfig {
        ProbeConfig {
            revision: self.revision(),
            checks: self.checks.iter().map(Check::to_proto).collect(),
        }
    }
}

/// Counters since the start, for tests and logs.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ProbeStats {
    pub samples: u64,
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
        loop {
            tokio::select! {
                () = &mut until => break,
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
                    let errors = runner.apply(set.checks.clone()).await;
                    for error in &errors {
                        tracing::warn!(device = %error.device, check = %error.name, error = %error.message, "probe check could not start");
                    }
                    tracing::info!(checks = set.checks.len(), devices = runner.devices(), "central probe checks applied");
                    current = Some(set);
                }
            }
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
