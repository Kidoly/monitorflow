use std::sync::Mutex;

use monitorflow_common::proto::v1::DeviceSamples;

use super::*;
use crate::ingest::{Series, Sink, WriteError, Writer, WriterSettings};

/// Keeps what it is given.
#[derive(Clone, Default)]
struct Kept(Arc<Mutex<Vec<Series>>>);

impl Sink for Kept {
    type Item = Series;
    fn name(&self) -> &'static str {
        "kept"
    }
    async fn write(&self, batch: &[Series]) -> Result<(), WriteError> {
        self.0.lock().unwrap().extend_from_slice(batch);
        Ok(())
    }
}

fn probes(kept: &Kept) -> (Probes, Writer<Series>) {
    let settings = WriterSettings {
        flush_interval: Duration::from_millis(10),
        ..WriterSettings::default()
    };
    let writer = Writer::spawn(kept.clone(), settings);
    let ingest = Ingest {
        metrics: Some(writer.queue()),
        events: None,
        virt: None,
    };
    let pool = sqlx::PgPool::connect_lazy("postgres://localhost/unused").unwrap();
    (Probes::new(ProbeStore::new(pool), ingest, None), writer)
}

fn sample(sensor: &str, value: f64) -> ProtoSample {
    ProtoSample {
        sensor: sensor.into(),
        instance: "relay-1".into(),
        timestamp_unix_ms: Utc::now().timestamp_millis(),
        value,
        labels: [("target".to_string(), "192.0.2.5".to_string())].into(),
    }
}

fn results(device: &str, samples: Vec<ProtoSample>) -> ProbeResults {
    ProbeResults {
        devices: vec![DeviceSamples {
            device: device.into(),
            samples,
        }],
    }
}

#[tokio::test]
async fn relays_are_believed_for_their_devices_only() {
    let kept = Kept::default();
    let (probes, writer) = probes(&kept);
    // Nothing sent to the relay yet.
    probes.ingest("relay-1", results("nas", vec![sample("ping_up", 1.0)]));
    assert_eq!(probes.stats().unknown_device, 1);
    probes.shared.write().unwrap().relays.insert(
        "relay-1".into(),
        [("nas".to_string(), "paris".to_string())].into(),
    );
    probes.ingest(
        "relay-1",
        results("nas", vec![sample("ping_up", 1.0), sample("Bad", 1.0)]),
    );
    probes.ingest("relay-1", results("web-01", vec![sample("ping_up", 0.0)]));
    probes.ingest("relay-2", results("nas", vec![sample("ping_up", 0.0)]));
    let stats = probes.stats();
    assert_eq!(
        (stats.samples, stats.invalid, stats.unknown_device),
        (1, 1, 3)
    );
    writer.shutdown(Duration::from_secs(1)).await;
    let kept = kept.0.lock().unwrap();
    assert_eq!(kept.len(), 1);
    let labels: BTreeMap<_, _> = kept[0].labels.iter().cloned().collect();
    assert_eq!(labels["__name__"], "monitorflow_ping_up");
    assert_eq!(labels["peer"], "nas");
    assert_eq!(labels["site"], "paris");
    assert_eq!(labels["probe"], "relay-1");
    assert_eq!(labels["instance"], "relay-1");
    assert_eq!(labels["target"], "192.0.2.5");
    let mut sorted = kept[0].labels.clone();
    sorted.sort();
    assert_eq!(sorted, kept[0].labels);
    probes.forget("relay-1");
    assert!(probes.shared.read().unwrap().relays.is_empty());
}

#[test]
fn a_probe_label_from_a_sensor_is_refused() {
    let identity = Identity {
        peer: "nas".into(),
        site: "paris".into(),
    };
    let mut forged = sample("ping_up", 1.0);
    forged.labels.insert(PROBE_LABEL.into(), "central".into());
    let checked = ingest::check_batch(
        &identity,
        SampleBatch {
            samples: vec![forged],
        },
        Utc::now().timestamp_millis(),
    );
    assert_eq!(checked.invalid, 1);
}

#[tokio::test]
async fn flow_reports_are_believed_for_given_devices_and_alert_per_address() {
    let kept = Kept::default();
    let (probes, writer) = probes(&kept);
    let (tx, mut written) = tokio::sync::mpsc::channel(8);
    let probes = probes.with_flows(
        FlowWriter::from_sender(tx),
        monitorflow_flow::collector::Settings::default(),
    );
    probes.shared.write().unwrap().relays.insert(
        "relay-1".into(),
        [("router".to_string(), "paris".to_string())].into(),
    );
    let report = |device: &str| FlowReport {
        device: device.into(),
        start_unix_ms: Utc::now().timestamp_millis() - 60_000,
        seconds: 60,
        bytes: 7_500,
        packets: 5,
        tops: vec![monitorflow_common::proto::v1::FlowTop {
            dimension: "src".into(),
            key: "192.0.2.15".into(),
            bytes: 7_500,
            packets: 5,
        }],
        conversations: vec![],
    };
    probes.flow_reports(
        "relay-1",
        FlowReports {
            reports: vec![report("router"), report("elsewhere")],
        },
    );
    let stats = probes.stats();
    assert_eq!((stats.flow_reports, stats.unknown_device), (1, 1));
    let batch = written.recv().await.unwrap();
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0].device, "router");
    writer.shutdown(Duration::from_secs(1)).await;
    let kept = kept.0.lock().unwrap();
    let address = kept
        .iter()
        .find(|s| {
            s.labels
                .iter()
                .any(|(k, v)| k == "instance" && v == "192.0.2.15")
        })
        .expect("the address's series");
    assert_eq!(address.value, 1_000.0, "7.5 kB in a minute");
    assert!(address
        .labels
        .iter()
        .any(|(k, v)| k == "__name__" && v == "monitorflow_flow_address_bits_per_second"));
    assert!(address
        .labels
        .iter()
        .any(|(k, v)| k == "probe" && v == "relay-1"));
}
