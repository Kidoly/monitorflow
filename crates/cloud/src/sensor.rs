//! The `aws` and `azure` sensors: one client kept between collections,
//! so Azure's access token is asked for once an hour.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use monitorflow_sensors::{virt, Sample, Sensor, SensorError};
use tokio::time::Instant;

use crate::http::{Failure, Http};
use crate::{aws, azure, Account, CloudVm, Target, What, MAX_VMS};

/// Kept before the deadline so a slow answer still reports `up 0`.
const MARGIN: Duration = Duration::from_millis(300);
/// Longest single request.
const REQUEST: Duration = Duration::from_secs(60);

/// Measures of the shared model that clouds do not have.
const NOT_IN_CLOUDS: [&str; 2] = ["vm_snapshots", "vm_snapshot_age_seconds"];

enum Client {
    Aws(aws::Client),
    Azure(azure::Client),
}

pub struct CloudSensor {
    target: Target,
    client: Client,
}

impl std::fmt::Debug for CloudSensor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudSensor")
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

/// What a collection read: the machines, and the first failure.
struct Read {
    vms: Vec<CloudVm>,
    failure: Option<Failure>,
    /// Why the machines could not be listed, for a test.
    listing: Option<Failure>,
    /// Whether metrics could be read, when they were asked for.
    metrics: Option<Result<(), Failure>>,
}

impl Read {
    fn failed(failure: Failure) -> Self {
        Self {
            vms: Vec::new(),
            failure: Some(failure.clone()),
            listing: Some(failure),
            metrics: None,
        }
    }
}

impl CloudSensor {
    pub fn new(target: Target) -> Result<Self, String> {
        Self::build(target, None)
    }

    /// A sensor whose requests go to `base` over plain HTTP, for tests
    /// against a fake API: every service of the provider answers there.
    pub fn with_base(target: Target, base: &str) -> Result<Self, String> {
        Self::build(target, Some(base.trim_end_matches('/').to_owned()))
    }

    fn build(target: Target, base: Option<String>) -> Result<Self, String> {
        let http = Http::new(REQUEST, base.is_none()).map_err(|f| f.message().to_owned())?;
        let client = match &target.account {
            Account::Aws { key_id, secret, .. } => Client::Aws(aws::Client {
                http,
                key_id: key_id.clone(),
                secret: secret.clone(),
                base,
            }),
            Account::Azure {
                tenant,
                client_id,
                secret,
                subscription,
            } => Client::Azure(azure::Client::new(
                http,
                tenant.clone(),
                client_id.clone(),
                secret.clone(),
                subscription.clone(),
                base.map(|b| (format!("{b}/login"), b)),
            )),
        };
        Ok(Self { target, client })
    }

    fn check_name(&self) -> &'static str {
        match self.client {
            Client::Aws(_) => crate::AWS,
            Client::Azure(_) => crate::AZURE,
        }
    }

    /// Words for the source's `product` label.
    fn product(&self) -> String {
        match &self.target.account {
            Account::Aws { regions, .. } => format!("Amazon EC2 ({})", regions.join(", ")),
            Account::Azure { subscription, .. } => format!("Azure (subscription {subscription})"),
        }
    }

    async fn read(&self, max: usize) -> Read {
        let now = chrono::Utc::now();
        match (&self.client, &self.target.account) {
            (Client::Aws(client), Account::Aws { regions, .. }) => {
                let mut read = Read {
                    vms: Vec::new(),
                    failure: None,
                    listing: None,
                    metrics: None,
                };
                for region in regions {
                    let left = max.saturating_sub(read.vms.len());
                    if left == 0 {
                        break;
                    }
                    let instances = match client.instances(region, left).await {
                        Ok(instances) => instances,
                        Err(failure) => {
                            let failure = failure.within(region);
                            read.listing.get_or_insert(failure.clone());
                            read.failure.get_or_insert(failure);
                            continue;
                        }
                    };
                    let checks = client.statuses(region).await.unwrap_or_else(|failure| {
                        read.failure.get_or_insert(failure.within(region));
                        HashMap::new()
                    });
                    let running: Vec<&str> = instances
                        .iter()
                        .filter(|i| i.state == "running")
                        .map(|i| i.id.as_str())
                        .collect();
                    // A test asks CloudWatch even without a machine, to
                    // say whether the right is there.
                    let ask: Vec<&str> = if running.is_empty() && self.target.what == What::Test {
                        vec!["i-00000000000000000"]
                    } else {
                        running
                    };
                    let mut metrics = HashMap::new();
                    if !ask.is_empty() {
                        match client.metrics(region, &ask, now).await {
                            Ok(found) => {
                                metrics = found;
                                read.metrics.get_or_insert(Ok(()));
                            }
                            Err(failure) => {
                                let failure = failure.within(&format!("CloudWatch {region}"));
                                read.metrics = Some(Err(failure.clone()));
                                read.failure.get_or_insert(failure);
                            }
                        }
                    }
                    for instance in instances {
                        let measures = metrics.remove(&instance.id).unwrap_or_default();
                        let found = checks.get(&instance.id);
                        let mut vm = instance.into_vm(found);
                        if vm.vm.powered_on {
                            vm.measures.extend(measures);
                        }
                        read.vms.push(vm);
                    }
                }
                read
            }
            (Client::Azure(client), _) => {
                let machines = match client.machines(max).await {
                    Ok(machines) => machines,
                    Err(failure) => return Read::failed(failure),
                };
                let mut failure = None;
                let nics = client.interfaces().await.unwrap_or_else(|f| {
                    failure = Some(f.within("network interfaces"));
                    HashMap::new()
                });
                let running: Vec<String> = machines
                    .iter()
                    .filter(|m| m.running())
                    .map(|m| m.id.clone())
                    .collect();
                let (mut metrics, failed) = client.metrics(&running, now).await;
                let metrics_read = match failed {
                    Some(f) => {
                        let f = f.within("Azure Monitor");
                        failure.get_or_insert(f.clone());
                        Some(Err(f))
                    }
                    None => (!running.is_empty()).then_some(Ok(())),
                };
                let vms = machines
                    .into_iter()
                    .map(|m| {
                        let id = m.id.to_ascii_lowercase();
                        let measures = metrics.remove(&id).unwrap_or_default();
                        m.into_vm(nics.get(&id), measures)
                    })
                    .collect();
                Read {
                    vms,
                    failure,
                    listing: None,
                    metrics: metrics_read,
                }
            }
            _ => unreachable!("the client is built from the account"),
        }
    }

    async fn collect_inventory(&self, deadline: Instant) -> Vec<Sample> {
        let read = tokio::time::timeout_at(
            deadline.checked_sub(MARGIN).unwrap_or(deadline),
            self.read(MAX_VMS),
        )
        .await
        .unwrap_or_else(|_| Read::failed(Failure::Connect("no answer in time".into())));
        let now = chrono::Utc::now().timestamp_millis();
        if let Some(failure) = &read.failure {
            tracing::debug!(kind = self.check_name(), reason = failure.reason(), error = %failure.message(), "cloud collection failed");
        }
        let mut samples = samples(&read.vms, now);
        let product = self.product();
        samples.push(virt::up(
            read.failure.as_ref().map(|f| (f.reason(), f.message())),
            &[("product", &product), ("provider", self.check_name())],
            now,
        ));
        samples
    }

    /// Signs in and counts the machines, for "Add a device".
    async fn test(&self, deadline: Instant) -> Vec<Sample> {
        let read = tokio::time::timeout_at(
            deadline.checked_sub(MARGIN).unwrap_or(deadline),
            self.read(MAX_VMS),
        )
        .await
        .unwrap_or_else(|_| Read::failed(Failure::Connect("no answer in time".into())));
        if let Some(failure) = &read.listing {
            return vec![failed("login", failure)];
        }
        let running = read.vms.iter().filter(|v| v.vm.powered_on).count();
        let mut out = vec![
            Sample::metric("login", 1.0),
            Sample::metric("vms", read.vms.len() as f64),
            Sample::metric("running", running as f64),
        ];
        match &read.metrics {
            Some(Err(failure)) => out.push(failed("metrics", failure)),
            Some(Ok(())) => out.push(Sample::metric("metrics", 1.0)),
            None => {}
        }
        // Another call failed: network interfaces, status checks.
        if let Some(failure) = read
            .failure
            .as_ref()
            .filter(|_| !matches!(read.metrics, Some(Err(_))))
        {
            out.push(failed("partial", failure));
        }
        out
    }
}

fn failed(metric: &str, failure: &Failure) -> Sample {
    Sample::metric(metric, 0.0)
        .with_label("reason", failure.reason())
        .with_label("error", virt::cut(failure.message()))
}

/// The samples of `vms`: the shared model without snapshots, the size on
/// `vm_info`, and the measures only clouds give.
pub fn samples(vms: &[CloudVm], now_ms: i64) -> Vec<Sample> {
    let shared: Vec<virt::Vm> = vms.iter().map(|v| v.vm.clone()).collect();
    let mut out: Vec<Sample> = virt::samples(&shared, &[], &[], &[], now_ms)
        .into_iter()
        .filter(|s| !NOT_IN_CLOUDS.contains(&s.sensor.as_str()))
        .collect();
    let named = virt::instances(vms.iter().map(|v| (v.vm.key.as_str(), v.vm.name.as_str())));
    for vm in vms {
        let instance = &named[&vm.vm.key];
        if let Some(size) = &vm.size {
            if let Some(info) = out
                .iter_mut()
                .find(|s| s.sensor == "vm_info" && &s.instance == instance)
            {
                info.labels.insert("size".into(), virt::cut(size));
            }
        }
        for (measure, value) in vm.measures.iter().filter(|(_, v)| v.is_finite()) {
            let mut sample = Sample::metric(format!("vm_{measure}"), *value)
                .with_instance(instance.clone())
                .with_label(virt::LABEL, "vm");
            sample.timestamp_unix_ms = now_ms;
            out.push(sample);
        }
    }
    out
}

#[async_trait]
impl Sensor for CloudSensor {
    async fn collect(&self, deadline: Instant) -> Result<Vec<Sample>, SensorError> {
        Ok(match self.target.what {
            What::Inventory => self.collect_inventory(deadline).await,
            What::Test => self.test(deadline).await,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use monitorflow_sensors::virt::Vm;

    #[test]
    fn cloud_machines_have_no_snapshots_but_their_own_measures() {
        let vms = [
            CloudVm {
                vm: Vm {
                    key: "i-1".into(),
                    name: "web".into(),
                    powered_on: true,
                    ..Vm::default()
                },
                size: Some("t3.micro".into()),
                measures: vec![
                    ("network_in_bytes_per_second", 10.0),
                    ("cpu_usage_ratio", f64::NAN),
                ],
            },
            CloudVm {
                vm: Vm {
                    key: "i-2".into(),
                    name: "web".into(),
                    ..Vm::default()
                },
                size: None,
                measures: vec![],
            },
        ];
        let samples = samples(&vms, 7);
        assert!(!samples.iter().any(|s| s.sensor.contains("snapshot")));
        let info = samples
            .iter()
            .find(|s| s.sensor == "vm_info" && s.instance == "web (i-1)")
            .unwrap();
        assert_eq!(info.labels["size"], "t3.micro");
        let net = samples
            .iter()
            .find(|s| s.sensor == "vm_network_in_bytes_per_second")
            .unwrap();
        assert_eq!((net.instance.as_str(), net.value), ("web (i-1)", 10.0));
        assert_eq!(net.labels[virt::LABEL], "vm");
        assert!(!samples.iter().any(|s| !s.value.is_finite()));
        assert!(samples.iter().all(|s| s.timestamp_unix_ms == 7));
    }
}
