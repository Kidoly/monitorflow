//! Network checks this relay runs as a probe (ADR-0148). The server sends
//! a `ProbeConfig` on the relay's own stream; the relay runs its checks
//! with [`monitorflow_probe::Runner`], acknowledges it, and sends what
//! they measure back up as `ProbeResults` on the same stream.
//!
//! While the upstream is down, results wait in the runner's bounded queue
//! and newer ones are dropped once it is full; the checks keep the last
//! configuration until a new one comes.
//!
//! The same configuration names the devices whose traffic flows the relay
//! receives (ADR-0184): the collector opens its UDP ports while there are
//! some, and each window's sums go up as `FlowReports`.

use std::sync::Arc;

use monitorflow_common::client::shutdown::Shutdown;
use monitorflow_common::config::FlowConfig;
use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::{
    FlowExporter, ProbeConfig, ProbeConfigAck, ProbeTest, ProbeTestResult,
};
use monitorflow_flow::collector::{self, Exporter, Settings, Stats};
use monitorflow_probe::{Check, ProbeFactory, Runner};
use tokio::sync::{mpsc, watch};

/// Configurations queued towards the probe task; a newer one replaces
/// the whole list, so a small queue is enough.
const CONFIG_QUEUE: usize = 4;
/// Tests waiting to run; more are refused, and the person tries again.
const TEST_QUEUE: usize = 8;
/// Tests running at once.
const TESTS_AT_ONCE: usize = 4;
/// Messages queued towards the upstream.
const UP_QUEUE: usize = 16;
/// Flow reports waiting for the upstream: an hour of one-minute windows.
/// Newer ones are dropped once it is full: flows are about now.
const FLOW_QUEUE: usize = 60;

/// The control stream's side.
pub struct ProbeEnd {
    pub configs: mpsc::Sender<ProbeConfig>,
    /// One-off tests (ADR-0149), run as they come.
    pub tests: mpsc::Sender<ProbeTest>,
    /// Acknowledgements and results, to send upstream.
    pub up: mpsc::Receiver<Payload>,
}

/// Starts the probe task with sensors from `factory`, receiving flows
/// as `flows` says.
pub fn start(factory: Arc<dyn ProbeFactory>, flows: &FlowConfig, shutdown: Shutdown) -> ProbeEnd {
    let settings = Settings {
        bind: flows.listen,
        ports: flows.ports(),
        ..Settings::default()
    };
    start_with(factory, settings, shutdown)
}

fn start_with(factory: Arc<dyn ProbeFactory>, settings: Settings, shutdown: Shutdown) -> ProbeEnd {
    let (configs, configs_rx) = mpsc::channel(CONFIG_QUEUE);
    let (tests, tests_rx) = mpsc::channel(TEST_QUEUE);
    let (up_tx, up) = mpsc::channel(UP_QUEUE);
    let (exporters_tx, exporters) = watch::channel(Vec::new());
    let (reports_tx, reports) = mpsc::channel(FLOW_QUEUE);
    tokio::spawn(collector::run(
        settings,
        exporters,
        reports_tx,
        Arc::new(Stats::default()),
    ));
    tokio::spawn(run_tests(factory.clone(), tests_rx, up_tx.clone()));
    tokio::spawn(run(
        factory,
        configs_rx,
        Flows {
            exporters: exporters_tx,
            reports,
        },
        up_tx,
        shutdown,
    ));
    ProbeEnd { configs, tests, up }
}

/// The probe task's side of the flow collector.
struct Flows {
    exporters: watch::Sender<Vec<Exporter>>,
    reports: mpsc::Receiver<monitorflow_common::proto::v1::FlowReports>,
}

/// The exporters of a configuration, as the collector takes them.
pub fn exporters(list: &[FlowExporter]) -> Vec<Exporter> {
    list.iter()
        .map(|e| Exporter {
            device: e.device.clone(),
            addresses: e.addresses.clone(),
        })
        .collect()
}

async fn run(
    factory: Arc<dyn ProbeFactory>,
    mut configs: mpsc::Receiver<ProbeConfig>,
    mut flows: Flows,
    up: mpsc::Sender<Payload>,
    mut shutdown: Shutdown,
) {
    let (mut runner, mut samples) = Runner::new(factory);
    loop {
        tokio::select! {
            () = shutdown.wait() => break,
            config = configs.recv() => {
                let Some(mut config) = config else { break };
                // Only the newest counts.
                while let Ok(newer) = configs.try_recv() {
                    config = newer;
                }
                let wanted = exporters(&config.flow_exporters);
                flows.exporters.send_if_modified(|current| {
                    let changed = *current != wanted;
                    if changed {
                        tracing::info!(exporters = wanted.len(), "flow exporters applied");
                        *current = wanted;
                    }
                    changed
                });
                let ack = apply(&mut runner, config).await;
                if up.send(Payload::ProbeConfigAck(ack)).await.is_err() {
                    break;
                }
            }
            batch = monitorflow_probe::next_batch(&mut samples) => {
                let Some(batch) = batch else { break };
                let results = monitorflow_probe::to_results(batch);
                // Waits while the upstream is down: the runner's queue
                // holds what comes meanwhile.
                if up.send(Payload::ProbeResults(results)).await.is_err() {
                    break;
                }
            }
            Some(reports) = flows.reports.recv() => {
                // Waits while the upstream is down; the collector drops
                // newer windows once its queue is full.
                if up.send(Payload::FlowReports(reports)).await.is_err() {
                    break;
                }
            }
        }
    }
    runner.shutdown().await;
}

/// Runs each test once, a few at a time, and sends its result up.
async fn run_tests(
    factory: Arc<dyn ProbeFactory>,
    mut tests: mpsc::Receiver<ProbeTest>,
    up: mpsc::Sender<Payload>,
) {
    let limit = Arc::new(tokio::sync::Semaphore::new(TESTS_AT_ONCE));
    while let Some(test) = tests.recv().await {
        let Ok(permit) = limit.clone().acquire_owned().await else {
            return;
        };
        let (factory, up) = (factory.clone(), up.clone());
        tokio::spawn(async move {
            let _permit = permit;
            let result = match test.check.and_then(Check::from_proto) {
                None => Err("a test without its check".to_string()),
                Some(check) => monitorflow_probe::test_once(&*factory, &check).await,
            };
            let result = match result {
                Ok(samples) => ProbeTestResult {
                    id: test.id,
                    samples: samples
                        .into_iter()
                        .map(monitorflow_probe::sample_to_proto)
                        .collect(),
                    error: String::new(),
                },
                Err(error) => ProbeTestResult {
                    id: test.id,
                    samples: Vec::new(),
                    error,
                },
            };
            let _ = up.send(Payload::ProbeTestResult(result)).await;
        });
    }
}

async fn apply(runner: &mut Runner, config: ProbeConfig) -> ProbeConfigAck {
    let mut errors = Vec::new();
    let mut checks = Vec::with_capacity(config.checks.len());
    for check in config.checks {
        let device = check.device.clone();
        match Check::from_proto(check) {
            Some(check) => checks.push(check),
            None => errors.push(monitorflow_common::proto::v1::ProbeCheckError {
                device,
                message: "a check without its specification".into(),
                ..Default::default()
            }),
        }
    }
    let count = checks.len();
    errors.extend(runner.apply(checks).await.iter().map(|e| e.to_proto()));
    tracing::info!(
        revision = config.revision,
        checks = count,
        refused = errors.len(),
        "probe checks applied"
    );
    ProbeConfigAck {
        revision: config.revision,
        errors,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use monitorflow_common::client::shutdown;
    use monitorflow_common::proto::v1::{ProbeCheck, SensorSpec};

    use super::*;

    /// A TCP port check: unlike ping, it needs no ICMP socket, which CI
    /// runners may not allow.
    fn port(device: &str, port: u16) -> ProbeCheck {
        ProbeCheck {
            device: device.into(),
            spec: Some(SensorSpec {
                kind: "port".into(),
                name: "port".into(),
                instance: "relay-1".into(),
                interval_ms: 1_000,
                timeout_ms: 900,
                params: BTreeMap::from([
                    ("host".to_string(), "127.0.0.1".to_string()),
                    ("port".to_string(), port.to_string()),
                ])
                .into_iter()
                .collect(),
            }),
        }
    }

    #[tokio::test]
    async fn tests_run_once_and_answer() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let open = listener.local_addr().unwrap().port();
        let (_trigger, stop) = shutdown::channel();
        let mut end = start(
            Arc::new(monitorflow_probe::Builtin),
            &FlowConfig::default(),
            stop,
        );
        end.tests
            .send(ProbeTest {
                id: "t1".into(),
                check: Some(port("lo", open)),
            })
            .await
            .unwrap();
        end.tests
            .send(ProbeTest {
                id: "t2".into(),
                check: None,
            })
            .await
            .unwrap();
        let mut results = Vec::new();
        while results.len() < 2 {
            let next = tokio::time::timeout(Duration::from_secs(10), end.up.recv());
            if let Some(Payload::ProbeTestResult(r)) = next.await.unwrap() {
                results.push(r);
            }
        }
        results.sort_by(|a, b| a.id.cmp(&b.id));
        assert!(results[0].error.is_empty(), "{:?}", results[0]);
        assert!(results[0]
            .samples
            .iter()
            .any(|s| s.sensor == "port_up" && s.value == 1.0));
        assert!(results[1].error.contains("without its check"));
    }

    #[tokio::test]
    async fn flows_of_the_configured_exporters_go_up() {
        let port = std::net::UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let (_trigger, stop) = shutdown::channel();
        let settings = Settings {
            bind: Some([127, 0, 0, 1].into()),
            ports: vec![port],
            window: Duration::from_secs(1),
        };
        let mut end = start_with(Arc::new(monitorflow_probe::Builtin), settings, stop);
        end.configs
            .send(ProbeConfig {
                revision: 3,
                flow_exporters: vec![FlowExporter {
                    device: "router".into(),
                    addresses: vec!["127.0.0.1".into()],
                }],
                ..Default::default()
            })
            .await
            .unwrap();
        // NetFlow v5: one record of 1500 bytes, 192.0.2.1 to 198.51.100.1.
        let mut d = vec![0, 5, 0, 1];
        d.extend([0u8; 20]);
        d.extend([192, 0, 2, 1, 198, 51, 100, 1, 0, 0, 0, 0, 0, 1, 0, 2]);
        d.extend(1u32.to_be_bytes());
        d.extend(1500u32.to_be_bytes());
        d.extend([0u8; 8]);
        d.extend([0xc3, 0x50, 0x01, 0xbb, 0, 0, 6, 0]);
        d.extend([0u8; 8]);
        let sender = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let report = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let _ = sender.send_to(&d, ("127.0.0.1", port)).await;
                match tokio::time::timeout(Duration::from_millis(300), end.up.recv()).await {
                    Ok(Some(Payload::FlowReports(r))) => break r,
                    Ok(None) => panic!("the probe task stopped"),
                    _ => {}
                }
            }
        })
        .await
        .expect("flow reports");
        let r = &report.reports[0];
        assert_eq!(r.device, "router");
        assert!(r
            .tops
            .iter()
            .any(|t| t.dimension == "app" && t.key == "tcp/443"));
    }

    #[tokio::test]
    async fn checks_run_and_are_acknowledged() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let open = listener.local_addr().unwrap().port();
        let (trigger, stop) = shutdown::channel();
        let mut end = start(
            Arc::new(monitorflow_probe::Builtin),
            &FlowConfig::default(),
            stop,
        );
        let config = ProbeConfig {
            revision: 7,
            checks: vec![
                port("lo", open),
                ProbeCheck {
                    device: "bad".into(),
                    spec: None,
                },
                ProbeCheck {
                    spec: Some(SensorSpec {
                        kind: "cpu".into(),
                        name: "cpu".into(),
                        interval_ms: 1_000,
                        ..Default::default()
                    }),
                    device: "x".into(),
                },
            ],
            ..Default::default()
        };
        end.configs.send(config).await.unwrap();
        let Some(Payload::ProbeConfigAck(ack)) = end.up.recv().await else {
            panic!("no ack")
        };
        assert_eq!(ack.revision, 7);
        assert_eq!(ack.errors.len(), 2, "{:?}", ack.errors);
        assert!(ack
            .errors
            .iter()
            .any(|e| e.device == "x" && e.message.contains("does not run")));
        let results = tokio::time::timeout(Duration::from_secs(10), end.up.recv())
            .await
            .unwrap();
        let Some(Payload::ProbeResults(results)) = results else {
            panic!("no results")
        };
        let lo = &results.devices[0];
        assert_eq!(lo.device, "lo");
        assert!(lo
            .samples
            .iter()
            .any(|s| s.sensor == "port_up" && s.instance == "relay-1" && s.value == 1.0));
        trigger.fire();
    }
}
