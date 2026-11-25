//! Azure: the virtual machines of one subscription, their network
//! interfaces and Azure Monitor's platform metrics, through Azure
//! Resource Manager with a service principal (OAuth client credentials).
//!
//! Calls, all read-only (the built-in Reader role):
//! `Microsoft.Compute/virtualMachines/read`,
//! `Microsoft.Network/networkInterfaces/read` and
//! `Microsoft.Insights/metrics/read`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use monitorflow_sensors::virt::{Status, Vm};
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::http::{bounded, form, Failure, Http};
use crate::CloudVm;

const LOGIN: &str = "https://login.microsoftonline.com";
const ARM: &str = "https://management.azure.com";
const SCOPE: &str = "https://management.azure.com/.default";
const COMPUTE_VERSION: &str = "2024-07-01";
const NETWORK_VERSION: &str = "2024-05-01";
const METRICS_VERSION: &str = "2023-10-01";
/// Pages read at most per list.
const MAX_PAGES: usize = 50;
/// Metric requests at once: one per running machine.
const PARALLEL: usize = 8;
/// How far back metrics are asked for, and their grain.
const WINDOW: i64 = 20 * 60;
const GRAIN_SECONDS: f64 = 300.0;

/// What Azure Monitor is asked per running machine: its metric, the
/// aggregation read, and the measure it becomes.
const METRICS: [(&str, &str, &str); 5] = [
    ("Percentage CPU", "average", "cpu_usage_ratio"),
    ("Network In Total", "total", "network_in_bytes_per_second"),
    ("Network Out Total", "total", "network_out_bytes_per_second"),
    ("Disk Read Bytes", "total", "disk_read_bytes_per_second"),
    ("Disk Write Bytes", "total", "disk_write_bytes_per_second"),
];
/// 1 while Azure sees the machine available, 0 otherwise.
const AVAILABILITY: &str = "VmAvailabilityMetric";

/// A subscription read with one service principal.
pub struct Client {
    pub http: Http,
    pub tenant: String,
    pub client_id: String,
    pub secret: String,
    pub subscription: String,
    /// Fake endpoints for tests: `(login, arm)`.
    pub bases: Option<(String, String)>,
    /// The access token and when to stop using it.
    token: Mutex<Option<(String, Instant)>>,
}

impl Client {
    pub fn new(
        http: Http,
        tenant: String,
        client_id: String,
        secret: String,
        subscription: String,
        bases: Option<(String, String)>,
    ) -> Self {
        Self {
            http,
            tenant,
            client_id,
            secret,
            subscription,
            bases,
            token: Mutex::new(None),
        }
    }

    fn arm(&self) -> &str {
        self.bases.as_ref().map_or(ARM, |(_, arm)| arm)
    }

    /// An access token, kept until a minute before it expires.
    async fn token(&self) -> Result<String, Failure> {
        let mut kept = self.token.lock().await;
        if let Some((token, until)) = kept.as_ref() {
            if Instant::now() < *until {
                return Ok(token.clone());
            }
        }
        let login = self.bases.as_ref().map_or(LOGIN, |(login, _)| login);
        let url = format!("{login}/{}/oauth2/v2.0/token", self.tenant);
        let body = form(&[
            ("grant_type".into(), "client_credentials".into()),
            ("client_id".into(), self.client_id.clone()),
            ("client_secret".into(), self.secret.clone()),
            ("scope".into(), SCOPE.into()),
        ]);
        let request = self
            .http
            .client()
            .post(url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body);
        let (status, bytes) = self.http.send(request).await?;
        let answer: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let token = answer["access_token"]
            .as_str()
            .filter(|_| status.is_success());
        let Some(token) = token else {
            let error = answer["error"].as_str().unwrap_or("refused");
            let description = answer["error_description"]
                .as_str()
                .and_then(|d| d.lines().next())
                .unwrap_or_default();
            let message = bounded(&format!("{error}: {description}"));
            return Err(if status.is_server_error() {
                Failure::Api(message)
            } else {
                Failure::Login(message)
            });
        };
        let lasts = answer["expires_in"].as_u64().unwrap_or(3600).max(120);
        let until = Instant::now() + Duration::from_secs(lasts - 60);
        *kept = Some((token.to_owned(), until));
        Ok(token.to_owned())
    }

    /// One `GET` of Resource Manager. A refused token is forgotten.
    async fn get(&self, url: &str) -> Result<Value, Failure> {
        let token = self.token().await?;
        let result = get(&self.http, &token, url).await;
        if matches!(result, Err(Failure::Login(_))) {
            *self.token.lock().await = None;
        }
        result
    }

    /// Every page of a list, following `nextLink` on Resource Manager only.
    async fn list(&self, first: String, max: usize) -> Result<Vec<Value>, Failure> {
        let mut out = Vec::new();
        let mut url = Some(first);
        let arm = format!("{}/", self.arm());
        for _ in 0..MAX_PAGES {
            let Some(next) = url.take() else { break };
            let mut page = self.get(&next).await?;
            if let Some(items) = page["value"].as_array_mut() {
                out.append(items);
            }
            if out.len() >= max {
                out.truncate(max);
                break;
            }
            // The token goes nowhere but Resource Manager.
            url = page["nextLink"]
                .as_str()
                .filter(|n| n.starts_with(&arm))
                .map(str::to_owned);
        }
        Ok(out)
    }

    /// The machines of the subscription with their power state.
    pub async fn machines(&self, max: usize) -> Result<Vec<Machine>, Failure> {
        let url = format!(
            "{}/subscriptions/{}/providers/Microsoft.Compute/virtualMachines?api-version={COMPUTE_VERSION}&statusOnly=true",
            self.arm(),
            self.subscription
        );
        Ok(self
            .list(url, max)
            .await?
            .iter()
            .filter_map(Machine::read)
            .collect())
    }

    /// The MAC and private addresses of each machine, by its resource ID
    /// in lower case.
    pub async fn interfaces(&self) -> Result<HashMap<String, Interfaces>, Failure> {
        let url = format!(
            "{}/subscriptions/{}/providers/Microsoft.Network/networkInterfaces?api-version={NETWORK_VERSION}",
            self.arm(),
            self.subscription
        );
        let mut out: HashMap<String, Interfaces> = HashMap::new();
        for nic in self.list(url, 10_000).await? {
            let properties = &nic["properties"];
            let Some(vm) = properties["virtualMachine"]["id"].as_str() else {
                continue;
            };
            let entry = out.entry(vm.to_ascii_lowercase()).or_default();
            if let Some(mac) = properties["macAddress"].as_str().and_then(mac) {
                entry.macs.push(mac);
            }
            for config in properties["ipConfigurations"]
                .as_array()
                .into_iter()
                .flatten()
            {
                if let Some(ip) = config["properties"]["privateIPAddress"].as_str() {
                    if !ip.is_empty() && !entry.addresses.iter().any(|a| a == ip) {
                        entry.addresses.push(ip.to_owned());
                    }
                }
            }
        }
        Ok(out)
    }

    /// The latest platform metrics of each machine of `ids` (resource IDs),
    /// a few at a time; a machine whose metrics fail is left without.
    /// The first failure is returned with what came.
    pub async fn metrics(
        &self,
        ids: &[String],
        now: DateTime<Utc>,
    ) -> (HashMap<String, Vec<(&'static str, f64)>>, Option<Failure>) {
        let mut out = HashMap::new();
        if ids.is_empty() {
            return (out, None);
        }
        let token = match self.token().await {
            Ok(token) => token,
            Err(failure) => return (out, Some(failure)),
        };
        let limit = Arc::new(tokio::sync::Semaphore::new(PARALLEL));
        let mut tasks = tokio::task::JoinSet::new();
        let names: Vec<&str> = METRICS
            .iter()
            .map(|(n, _, _)| *n)
            .chain([AVAILABILITY])
            .collect();
        let names = names.join(",");
        let start = (now - chrono::Duration::seconds(WINDOW)).format("%Y-%m-%dT%H:%M:%SZ");
        let timespan = format!("{start}/{}", now.format("%Y-%m-%dT%H:%M:%SZ"));
        for id in ids {
            let url = match reqwest::Url::parse_with_params(
                &format!("{}{id}/providers/Microsoft.Insights/metrics", self.arm()),
                &[
                    ("api-version", METRICS_VERSION),
                    ("metricnames", &names),
                    ("timespan", &timespan),
                    ("interval", "PT5M"),
                    ("aggregation", "Average,Total"),
                ],
            ) {
                Ok(url) => url.to_string(),
                Err(_) => continue,
            };
            let (http, token, id, limit) =
                (self.http.clone(), token.clone(), id.clone(), limit.clone());
            tasks.spawn(async move {
                let _turn = limit.acquire_owned().await;
                (id, get(&http, &token, &url).await)
            });
        }
        let mut first = None;
        while let Some(done) = tasks.join_next().await {
            let Ok((id, result)) = done else { continue };
            match result {
                Ok(answer) => {
                    out.insert(id.to_ascii_lowercase(), read_metrics(&answer));
                }
                Err(failure) => {
                    first.get_or_insert(failure);
                }
            }
        }
        (out, first)
    }
}

/// One `GET` with `token`, its answer as JSON.
async fn get(http: &Http, token: &str, url: &str) -> Result<Value, Failure> {
    let request = http.client().get(url).bearer_auth(token);
    let (status, bytes) = http.send(request).await?;
    let answer: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if status.is_success() {
        return Ok(answer);
    }
    let code = answer["error"]["code"].as_str().unwrap_or_default();
    let message = answer["error"]["message"]
        .as_str()
        .unwrap_or("the API refused the call");
    let message = bounded(&format!("{code}: {message}"));
    Err(match (status.as_u16(), code) {
        (401, _) | (_, "InvalidAuthenticationToken" | "SubscriptionNotFound") => {
            Failure::Login(message)
        }
        (403, _) | (_, "AuthorizationFailed") => Failure::Permission(message),
        _ => Failure::Api(if code.is_empty() {
            format!("the API answered {status}")
        } else {
            message
        }),
    })
}

/// A MAC address as agents report it: `00-0D-3A-1B-2C-3D` gives
/// `00:0d:3a:1b:2c:3d`.
fn mac(text: &str) -> Option<String> {
    let hex: String = text.chars().filter(char::is_ascii_hexdigit).collect();
    (hex.len() == 12).then(|| {
        hex.as_bytes()
            .chunks(2)
            .map(|p| String::from_utf8_lossy(p).to_ascii_lowercase())
            .collect::<Vec<_>>()
            .join(":")
    })
}

/// The newest value of each metric of an answer.
fn read_metrics(answer: &Value) -> Vec<(&'static str, f64)> {
    let mut out = Vec::new();
    for metric in answer["value"].as_array().into_iter().flatten() {
        let name = metric["name"]["value"].as_str().unwrap_or_default();
        let newest = |aggregation: &str| -> Option<f64> {
            metric["timeseries"]
                .as_array()?
                .iter()
                .flat_map(|t| t["data"].as_array().into_iter().flatten())
                .filter_map(|d| Some((d["timeStamp"].as_str()?, d[aggregation].as_f64()?)))
                .max_by(|a, b| a.0.cmp(b.0))
                .map(|(_, v)| v)
                .filter(|v| v.is_finite())
        };
        if name == AVAILABILITY {
            if let Some(available) = newest("average") {
                out.push(("status_checks_failed", f64::from(u8::from(available < 1.0))));
            }
            continue;
        }
        let Some((_, aggregation, measure)) = METRICS.iter().find(|(n, _, _)| *n == name) else {
            continue;
        };
        if let Some(value) = newest(aggregation) {
            out.push((
                measure,
                if *aggregation == "total" {
                    value / GRAIN_SECONDS
                } else {
                    value / 100.0
                },
            ));
        }
    }
    out
}

/// The network interfaces of one machine.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Interfaces {
    pub macs: Vec<String>,
    pub addresses: Vec<String>,
}

/// One virtual machine.
#[derive(Debug, Clone, PartialEq)]
pub struct Machine {
    /// Its resource ID.
    pub id: String,
    pub name: String,
    /// The SMBIOS UUID its guest reads.
    pub vm_id: Option<String>,
    pub location: String,
    pub size: Option<String>,
    pub computer_name: Option<String>,
    pub os: Option<String>,
    /// `running`, `stopped`, `deallocated`, `starting`, …; empty when
    /// not known.
    pub power: String,
}

impl Machine {
    fn read(value: &Value) -> Option<Self> {
        let text = |v: &Value| v.as_str().filter(|t| !t.is_empty()).map(str::to_owned);
        let properties = &value["properties"];
        let power = properties["instanceView"]["statuses"]
            .as_array()
            .into_iter()
            .flatten()
            .find_map(|s| s["code"].as_str()?.strip_prefix("PowerState/"))
            .unwrap_or_default()
            .to_owned();
        Some(Self {
            id: text(&value["id"])?,
            name: text(&value["name"])?,
            vm_id: text(&properties["vmId"]).map(|u| u.to_ascii_lowercase()),
            location: text(&value["location"]).unwrap_or_default(),
            size: text(&properties["hardwareProfile"]["vmSize"]),
            computer_name: text(&properties["osProfile"]["computerName"])
                .or_else(|| text(&properties["instanceView"]["computerName"])),
            os: text(&properties["storageProfile"]["osDisk"]["osType"])
                .or_else(|| text(&properties["instanceView"]["osName"])),
            power,
        })
    }

    pub fn running(&self) -> bool {
        self.power == "running"
    }

    /// As a virtual machine of the inventory, with its interfaces and
    /// metrics.
    pub fn into_vm(self, nics: Option<&Interfaces>, metrics: Vec<(&'static str, f64)>) -> CloudVm {
        let running = self.running();
        let status = metrics
            .iter()
            .find(|(m, _)| *m == "status_checks_failed")
            .map(|(_, failed)| {
                if *failed > 0.0 {
                    Status::Critical
                } else {
                    Status::Ok
                }
            });
        CloudVm {
            vm: Vm {
                key: self.vm_id.clone().unwrap_or_else(|| self.id.clone()),
                name: self.name,
                uuid: self.vm_id,
                host: Some(self.location).filter(|l| !l.is_empty()),
                power_state: self.power,
                powered_on: running,
                status: status.filter(|_| running),
                guest_hostname: self.computer_name,
                guest_os: self.os,
                addresses: nics.map(|n| n.addresses.clone()).unwrap_or_default(),
                macs: nics.map(|n| n.macs.clone()).unwrap_or_default(),
                ..Vm::default()
            },
            size: self.size,
            measures: if running { metrics } else { Vec::new() },
        }
    }
}

/// An Azure ID: a GUID.
pub fn valid_id(text: &str) -> bool {
    let parts: Vec<&str> = text.split('-').collect();
    parts.iter().map(|p| p.len()).eq([8, 4, 4, 4, 12])
        && parts
            .iter()
            .all(|p| p.bytes().all(|b| b.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn machines_read_from_a_list() {
        let answer = json!({"value": [
            {"id": "/subscriptions/s/resourceGroups/rg/providers/Microsoft.Compute/virtualMachines/web-01",
             "name": "web-01", "location": "westeurope",
             "properties": {"vmId": "4A1B2C3D-0000-4000-8000-000000000001",
                            "hardwareProfile": {"vmSize": "Standard_B2s"},
                            "osProfile": {"computerName": "web-01"},
                            "storageProfile": {"osDisk": {"osType": "Linux"}},
                            "instanceView": {"statuses": [
                                {"code": "ProvisioningState/succeeded"},
                                {"code": "PowerState/running"}]}}},
            {"id": "/subscriptions/s/x/db", "name": "db",
             "properties": {"instanceView": {"statuses": [{"code": "PowerState/deallocated"}]}}},
            {"name": "no id"}
        ]});
        let machines: Vec<Machine> = answer["value"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Machine::read)
            .collect();
        assert_eq!(machines.len(), 2);
        assert!(machines[0].running());
        assert_eq!(
            machines[0].vm_id.as_deref(),
            Some("4a1b2c3d-0000-4000-8000-000000000001")
        );
        assert_eq!(machines[1].power, "deallocated");
        let vm = machines[0].clone().into_vm(
            Some(&Interfaces {
                macs: vec!["00:0d:3a:1b:2c:3d".into()],
                addresses: vec!["10.1.0.4".into()],
            }),
            vec![("cpu_usage_ratio", 0.25), ("status_checks_failed", 1.0)],
        );
        assert_eq!(vm.vm.key, "4a1b2c3d-0000-4000-8000-000000000001");
        assert_eq!(vm.vm.status, Some(Status::Critical));
        assert_eq!(vm.vm.host.as_deref(), Some("westeurope"));
        assert_eq!(vm.size.as_deref(), Some("Standard_B2s"));
        let off = machines[1]
            .clone()
            .into_vm(None, vec![("cpu_usage_ratio", 0.0)]);
        assert!(!off.vm.powered_on && off.measures.is_empty());
        assert_eq!(
            off.vm.key, "/subscriptions/s/x/db",
            "no vmId: the resource ID"
        );
    }

    #[test]
    fn metrics_take_the_newest_point() {
        let answer = json!({"value": [
            {"name": {"value": "Percentage CPU"}, "timeseries": [{"data": [
                {"timeStamp": "2026-11-24T10:00:00Z", "average": 10.0},
                {"timeStamp": "2026-11-24T10:05:00Z", "average": 40.0},
                {"timeStamp": "2026-11-24T10:10:00Z"}]}]},
            {"name": {"value": "Network In Total"}, "timeseries": [{"data": [
                {"timeStamp": "2026-11-24T10:05:00Z", "total": 3000.0}]}]},
            {"name": {"value": "VmAvailabilityMetric"}, "timeseries": [{"data": [
                {"timeStamp": "2026-11-24T10:05:00Z", "average": 1.0}]}]},
            {"name": {"value": "Something else"}, "timeseries": []}
        ]});
        assert_eq!(
            read_metrics(&answer),
            [
                ("cpu_usage_ratio", 0.4),
                ("network_in_bytes_per_second", 10.0),
                ("status_checks_failed", 0.0)
            ]
        );
    }

    #[test]
    fn ids_and_macs_are_checked() {
        assert!(valid_id("72f988bf-86f1-41af-91ab-2d7cd011db47"));
        assert!(!valid_id("72f988bf-86f1-41af-91ab-2d7cd011db4"));
        assert!(!valid_id("72f988bf-86f1-41af-91ab-2d7cd011db4/"));
        assert!(!valid_id("contoso.onmicrosoft.com"));
        assert_eq!(
            mac("00-0D-3A-1B-2C-3D").as_deref(),
            Some("00:0d:3a:1b:2c:3d")
        );
        assert_eq!(mac("00-0D"), None);
    }
}
