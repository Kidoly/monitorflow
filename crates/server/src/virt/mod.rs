//! Virtual machines, hosts, clusters and datastores of vCenters, ESXi
//! and Hyper-V hosts (ADR-0156), kept from the samples their checks
//! write.
//!
//! [`Sink::observe`] sees every validated batch, agents' and probes'
//! alike, and passes on the series with a `virt` label without waiting.
//! A task gathers them per source and writes them to `virtual_sources`
//! and `virtual_objects` every [`FLUSH`], then matches each source's
//! virtual machines to the servers of its customer ([`matching`]).

pub mod matching;
mod store;

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use monitorflow_sensors::virt::{KEY, KINDS, LABEL, SOURCE};
use sqlx::PgPool;
use tokio::sync::mpsc;

use crate::ingest::Series;

/// How often gathered samples are written.
pub const FLUSH: Duration = Duration::from_secs(10);
/// Batches waiting for the task; more are dropped, the next collection
/// brings them again.
const QUEUE: usize = 512;
/// Labels that are not an object's description.
const IDENTITY: [&str; 7] = ["__name__", "instance", "peer", "site", "probe", LABEL, KEY];

/// Where batches go.
#[derive(Clone, Debug)]
pub struct Sink {
    tx: mpsc::Sender<Vec<Series>>,
}

impl Sink {
    /// A sink and the task that writes what it gets to `pool`.
    pub fn spawn(pool: PgPool) -> Self {
        let (tx, rx) = mpsc::channel(QUEUE);
        tokio::spawn(store::run(pool, rx));
        Self { tx }
    }

    /// A sink whose batches go to `tx`, for tests.
    pub fn with_sender(tx: mpsc::Sender<Vec<Series>>) -> Self {
        Self { tx }
    }

    /// Passes on the series of `batch` that describe virtual objects.
    pub fn observe(&self, batch: &[Series]) {
        let picked: Vec<Series> = batch
            .iter()
            .filter(|s| label(s, LABEL).is_some())
            .cloned()
            .collect();
        if !picked.is_empty() && self.tx.try_send(picked).is_err() {
            tracing::debug!("virtual inventory queue full; samples dropped");
        }
    }
}

fn label<'a>(series: &'a Series, name: &str) -> Option<&'a str> {
    series
        .labels
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

/// The source's own state.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceState {
    pub up: bool,
    pub reason: Option<String>,
    pub error: Option<String>,
    pub details: BTreeMap<String, String>,
}

/// What one collection said of one object.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Object {
    pub key: Option<String>,
    /// From its `_info` sample; `None` when only numbers came.
    pub details: Option<BTreeMap<String, String>>,
    pub metrics: BTreeMap<String, f64>,
    /// The collection's time, shared by its samples.
    pub collected_ms: i64,
}

/// What came from one source since the last write.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Gathered {
    pub source: Option<SourceState>,
    /// By kind and instance.
    pub objects: HashMap<(String, String), Object>,
}

impl Gathered {
    /// Adds one series. Samples of an older collection than the one
    /// already gathered for their object are dropped; a newer one
    /// replaces it.
    pub fn add(&mut self, series: &Series) {
        let (Some(kind), Some(name)) = (label(series, LABEL), label(series, "__name__")) else {
            return;
        };
        if kind == SOURCE {
            if name.ends_with("_up") {
                let details = series
                    .labels
                    .iter()
                    .filter(|(k, _)| {
                        !IDENTITY.contains(&k.as_str()) && k != "reason" && k != "error"
                    })
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                self.source = Some(SourceState {
                    up: series.value >= 1.0,
                    reason: label(series, "reason").map(str::to_owned),
                    error: label(series, "error").map(str::to_owned),
                    details,
                });
            }
            return;
        }
        let (Some(kind), Some(instance)) = (
            KINDS.iter().find(|k| **k == kind),
            label(series, "instance").filter(|i| !i.is_empty()),
        ) else {
            return;
        };
        let Some(measure) = measure(name, kind) else {
            return;
        };
        let object = self
            .objects
            .entry(((*kind).to_owned(), instance.to_owned()))
            .or_default();
        if series.timestamp_ms < object.collected_ms {
            return;
        }
        if series.timestamp_ms > object.collected_ms {
            *object = Object {
                collected_ms: series.timestamp_ms,
                ..Object::default()
            };
        }
        if measure == "info" {
            object.key = label(series, KEY).map(str::to_owned);
            object.details = Some(
                series
                    .labels
                    .iter()
                    .filter(|(k, _)| !IDENTITY.contains(&k.as_str()))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            );
        } else {
            object.metrics.insert(measure.to_owned(), series.value);
        }
    }
}

/// The measure of metric `name` about an object of `kind`:
/// `monitorflow_vmware_vm_snapshots` gives `snapshots`.
pub fn measure<'a>(name: &'a str, kind: &str) -> Option<&'a str> {
    let marker = format!("_{kind}_");
    let at = name.rfind(&marker)?;
    let measure = &name[at + marker.len()..];
    (!measure.is_empty()
        && measure
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'))
    .then_some(measure)
}

/// Writes `batches` and matches their sources at once, as the task does
/// over time; for tools and tests.
pub async fn apply(pool: &PgPool, batches: &[Vec<Series>]) -> Result<(), sqlx::Error> {
    let mut gathered = HashMap::new();
    gather(batches, &mut gathered);
    for (peer, source) in gathered {
        if let Some(id) = store::write(pool, &peer, &source).await? {
            store::match_source(pool, id).await?;
        }
    }
    Ok(())
}

/// The batches seen, by the peer they are about.
pub fn gather(batches: &[Vec<Series>], into: &mut HashMap<String, Gathered>) {
    for series in batches.iter().flatten() {
        if let Some(peer) = label(series, "peer") {
            into.entry(peer.to_owned()).or_default().add(series);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn series(name: &str, instance: &str, value: f64, at: i64, extra: &[(&str, &str)]) -> Series {
        let mut labels: Vec<(String, String)> = vec![
            ("__name__".into(), name.into()),
            ("instance".into(), instance.into()),
            ("peer".into(), "vcenter-1".into()),
            ("site".into(), "paris".into()),
            ("probe".into(), "central".into()),
        ];
        labels.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        labels.sort();
        Series {
            labels,
            timestamp_ms: at,
            value,
        }
    }

    #[test]
    fn measures_come_after_the_kind() {
        assert_eq!(
            measure("monitorflow_vmware_vm_snapshots", "vm"),
            Some("snapshots")
        );
        assert_eq!(
            measure("monitorflow_vm_lab_vm_powered_on", "vm"),
            Some("powered_on")
        );
        assert_eq!(
            measure("monitorflow_vmware_cluster_hosts_effective", "cluster"),
            Some("hosts_effective")
        );
        assert_eq!(measure("monitorflow_vmware_up", "vm"), None);
    }

    #[test]
    fn collections_are_gathered_by_source_and_object() {
        let batch = vec![
            series(
                "monitorflow_vmware_vm_info",
                "web-01",
                1.0,
                10,
                &[("virt", "vm"), ("key", "vm-42"), ("uuid", "u-1")],
            ),
            series(
                "monitorflow_vmware_vm_snapshots",
                "web-01",
                2.0,
                10,
                &[("virt", "vm")],
            ),
            series(
                "monitorflow_vmware_up",
                "",
                1.0,
                10,
                &[("virt", "source"), ("product", "vCenter 8")],
            ),
            series("monitorflow_cpu_usage_ratio", "total", 0.5, 10, &[]),
        ];
        let (tx, mut rx) = mpsc::channel(4);
        Sink::with_sender(tx).observe(&batch);
        let picked = rx.try_recv().unwrap();
        assert_eq!(picked.len(), 3, "series without `virt` stay out");

        let mut gathered = HashMap::new();
        gather(&[picked], &mut gathered);
        // A later batch of the same collection adds to it; an older one is
        // dropped; a newer one replaces it.
        gather(
            &[vec![
                series(
                    "monitorflow_vmware_vm_snapshot_age_seconds",
                    "web-01",
                    9.0,
                    10,
                    &[("virt", "vm")],
                ),
                series(
                    "monitorflow_vmware_vm_snapshots",
                    "web-01",
                    7.0,
                    5,
                    &[("virt", "vm")],
                ),
                series(
                    "monitorflow_vmware_datastore_free_ratio",
                    "ds1",
                    0.5,
                    10,
                    &[("virt", "datastore")],
                ),
            ]],
            &mut gathered,
        );
        let source = &gathered["vcenter-1"];
        assert_eq!(
            source.source.as_ref().unwrap().details["product"],
            "vCenter 8"
        );
        let vm = &source.objects[&("vm".to_string(), "web-01".to_string())];
        assert_eq!(vm.key.as_deref(), Some("vm-42"));
        assert_eq!(vm.details.as_ref().unwrap()["uuid"], "u-1");
        assert!(!vm.details.as_ref().unwrap().contains_key("probe"));
        assert_eq!(vm.metrics["snapshots"], 2.0);
        assert_eq!(vm.metrics["snapshot_age_seconds"], 9.0);
        gather(
            &[vec![series(
                "monitorflow_vmware_vm_snapshots",
                "web-01",
                0.0,
                20,
                &[("virt", "vm")],
            )]],
            &mut gathered,
        );
        let vm = &gathered["vcenter-1"].objects[&("vm".to_string(), "web-01".to_string())];
        assert_eq!(vm.metrics.len(), 1);
        assert!(vm.details.is_none());
    }
}
