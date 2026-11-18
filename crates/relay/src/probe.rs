//! Network checks this relay runs as a probe (ADR-0148). The server sends
//! a `ProbeConfig` on the relay's own stream; the relay runs its checks
//! with [`monitorflow_probe::Runner`], acknowledges it, and sends what
//! they measure back up as `ProbeResults` on the same stream.
//!
//! While the upstream is down, results wait in the runner's bounded queue
//! and newer ones are dropped once it is full; the checks keep the last
//! configuration until a new one comes.

use std::sync::Arc;

use monitorflow_common::client::shutdown::Shutdown;
use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::{ProbeConfig, ProbeConfigAck};
use monitorflow_probe::{Check, ProbeFactory, Runner};
use tokio::sync::mpsc;

/// Configurations queued towards the probe task; a newer one replaces
/// the whole list, so a small queue is enough.
const CONFIG_QUEUE: usize = 4;
/// Messages queued towards the upstream.
const UP_QUEUE: usize = 16;

/// The control stream's side.
pub struct ProbeEnd {
    pub configs: mpsc::Sender<ProbeConfig>,
    /// Acknowledgements and results, to send upstream.
    pub up: mpsc::Receiver<Payload>,
}

/// Starts the probe task with sensors from `factory`.
pub fn start(factory: Arc<dyn ProbeFactory>, shutdown: Shutdown) -> ProbeEnd {
    let (configs, configs_rx) = mpsc::channel(CONFIG_QUEUE);
    let (up_tx, up) = mpsc::channel(UP_QUEUE);
    tokio::spawn(run(factory, configs_rx, up_tx, shutdown));
    ProbeEnd { configs, up }
}

async fn run(
    factory: Arc<dyn ProbeFactory>,
    mut configs: mpsc::Receiver<ProbeConfig>,
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
        }
    }
    runner.shutdown().await;
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

    fn ping(device: &str, host: &str) -> ProbeCheck {
        ProbeCheck {
            device: device.into(),
            spec: Some(SensorSpec {
                kind: "ping".into(),
                name: "ping".into(),
                instance: "relay-1".into(),
                interval_ms: 1_000,
                timeout_ms: 900,
                params: BTreeMap::from([
                    ("host".to_string(), host.to_string()),
                    ("count".to_string(), "1".to_string()),
                ])
                .into_iter()
                .collect(),
            }),
        }
    }

    #[tokio::test]
    async fn checks_run_and_are_acknowledged() {
        let (trigger, stop) = shutdown::channel();
        let mut end = start(Arc::new(monitorflow_probe::Builtin), stop);
        let config = ProbeConfig {
            revision: 7,
            checks: vec![
                ping("lo", "127.0.0.1"),
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
            .any(|s| s.sensor == "ping_up" && s.instance == "relay-1"));
        trigger.fire();
    }
}
