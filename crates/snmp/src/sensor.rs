//! The `snmp` sensor: polls one group of a device per collection.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_snmp::{Auth, Client, EngineCache, Oid, Retry, UsmConfig, Value};
use monitorflow_sensors::{Sample, Sensor, SensorError};
use tokio::time::{timeout_at, Instant};

use crate::mibs::{self, Counters, Group, Table, Val};
use crate::{Credentials, Target};

/// Longest wait for one request; a collection retries once within its
/// deadline.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
/// Rows kept from one walk: past it, a table is cut.
const MAX_ROWS: usize = 5000;

pub struct SnmpSensor {
    target: Target,
    /// v3 engines learned, kept between collections.
    engines: Arc<EngineCache>,
    /// Interface counters of the last collection, for rates.
    counters: Mutex<BTreeMap<String, Counters>>,
}

impl std::fmt::Debug for SnmpSensor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SnmpSensor")
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

fn oid(arcs: &[u32]) -> Oid {
    Oid::from_slice(arcs)
}

fn val(value: &Value) -> Option<Val> {
    Some(match value {
        Value::Integer(v) => Val::Int(i64::from(*v)),
        Value::Counter32(v) | Value::Gauge32(v) | Value::UInteger32(v) | Value::TimeTicks(v) => {
            Val::Uint(u64::from(*v))
        }
        Value::Counter64(v) => Val::Uint(*v),
        Value::OctetString(bytes) => Val::Text(String::from_utf8_lossy(bytes).into_owned()),
        Value::ObjectIdentifier(o) => Val::Oid(o.arcs().to_vec()),
        Value::IpAddress(ip) => Val::Text(std::net::Ipv4Addr::from(*ip).to_string()),
        _ => return None,
    })
}

impl SnmpSensor {
    pub fn new(target: Target) -> Self {
        Self {
            target,
            engines: Arc::new(EngineCache::new()),
            counters: Mutex::default(),
        }
    }

    fn auth(&self) -> Result<Auth, SensorError> {
        let permanent =
            |e: &dyn std::fmt::Display| SensorError::Permanent(format!("SNMP credentials: {e}"));
        Ok(match &self.target.credentials {
            Credentials::V2c { community } => Auth::v2c(community.as_str()),
            Credentials::V3 {
                user,
                auth,
                privacy,
            } => {
                let config = UsmConfig::new(user.clone().into_bytes());
                let config = match (auth, privacy) {
                    (None, _) => config,
                    (Some((a, ap)), None) => config
                        .auth(a.library(), ap.as_bytes())
                        .map_err(|e| permanent(&e))?,
                    (Some((a, ap)), Some((p, pp))) => config
                        .auth_priv(a.library(), ap.as_bytes(), p.library(), pp.as_bytes())
                        .map_err(|e| permanent(&e))?,
                };
                config.into()
            }
        })
    }

    async fn client(&self, deadline: Instant) -> Result<Client, SensorError> {
        let left = deadline.saturating_duration_since(Instant::now());
        let request = REQUEST_TIMEOUT
            .min(left / 2)
            .max(Duration::from_millis(200));
        let builder = Client::builder((self.target.host.as_str(), self.target.port), self.auth()?)
            .request_timeout(request)
            .retry(Retry::none())
            .engine_cache(self.engines.clone());
        match timeout_at(deadline, builder.connect()).await {
            Ok(Ok(client)) => Ok(client),
            Ok(Err(error)) => Err(SensorError::Transient(format!(
                "cannot reach the device: {error}"
            ))),
            Err(_) => Err(SensorError::Transient(
                "the device did not answer in time".into(),
            )),
        }
    }

    /// The values of `oids`, by OID; absent ones left out.
    async fn get(
        &self,
        client: &Client,
        oids: &[&[u32]],
    ) -> Result<BTreeMap<Vec<u32>, Val>, SensorError> {
        let list: Vec<Oid> = oids.iter().map(|o| oid(o)).collect();
        let response = client
            .get_many(&list)
            .await
            .map_err(|e| SensorError::Transient(format!("SNMP request failed: {e}")))?;
        Ok(response
            .varbinds
            .iter()
            .filter_map(|vb| Some((vb.oid.arcs().to_vec(), val(&vb.value)?)))
            .collect())
    }

    /// Every row under `root`, at most [`MAX_ROWS`].
    async fn walk(
        &self,
        client: &Client,
        root: &[u32],
    ) -> Result<Vec<(Vec<u32>, Val)>, SensorError> {
        let mut stream = client
            .walk(oid(root))
            .map_err(|e| SensorError::Transient(format!("SNMP walk failed: {e}")))?;
        let mut rows = Vec::new();
        while let Some(item) = stream.next().await {
            let vb = item.map_err(|e| SensorError::Transient(format!("SNMP walk failed: {e}")))?;
            if let Some(value) = val(&vb.value) {
                rows.push((vb.oid.arcs().to_vec(), value));
            }
            if rows.len() >= MAX_ROWS {
                tracing::debug!(host = %self.target.host, "table cut at {MAX_ROWS} rows");
                break;
            }
        }
        Ok(rows)
    }

    async fn table(&self, client: &Client, root: &[u32]) -> Result<Table, SensorError> {
        Ok(mibs::table(root, self.walk(client, root).await?))
    }

    async fn poll(&self, client: &Client) -> Result<Vec<Sample>, SensorError> {
        match self.target.group {
            Group::System => {
                let values = self.get(client, &[mibs::SYS_UPTIME]).await?;
                let ticks = values.get(mibs::SYS_UPTIME).and_then(Val::number);
                Ok(mibs::system(ticks))
            }
            Group::Interfaces => {
                let if_table = self.table(client, mibs::IF_TABLE).await?;
                // Devices with IF-MIB v1 only have no ifXTable.
                let ifx_table = self
                    .table(client, mibs::IFX_TABLE)
                    .await
                    .unwrap_or_default();
                let now = chrono_now_ms();
                let mut kept = self.counters.lock().unwrap_or_else(|e| e.into_inner());
                let (samples, counters) = mibs::interfaces(&if_table, &ifx_table, &kept, now);
                *kept = counters;
                Ok(samples)
            }
            Group::Host => {
                let loads: Vec<f64> = self
                    .walk(client, mibs::HR_PROCESSOR_LOAD)
                    .await?
                    .iter()
                    .filter_map(|(_, v)| v.number())
                    .collect();
                let storage = self.table(client, mibs::HR_STORAGE).await?;
                Ok(mibs::host(&loads, &storage))
            }
            Group::Ups => {
                let rows = self.walk(client, mibs::UPS).await?;
                let values = rows
                    .into_iter()
                    .filter_map(|(o, v)| {
                        let rest = o.strip_prefix(mibs::UPS)?;
                        Some((
                            rest.iter()
                                .map(u32::to_string)
                                .collect::<Vec<_>>()
                                .join("."),
                            v,
                        ))
                    })
                    .collect();
                Ok(mibs::ups(&values))
            }
            Group::Temperature => {
                let sensors = self.table(client, mibs::ENT_SENSOR).await?;
                let names = self
                    .walk(client, mibs::ENT_PHYSICAL_NAME)
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|(o, v)| {
                        let rest = o.strip_prefix(mibs::ENT_PHYSICAL_NAME)?;
                        Some((
                            rest.iter()
                                .map(u32::to_string)
                                .collect::<Vec<_>>()
                                .join("."),
                            v,
                        ))
                    })
                    .collect();
                Ok(mibs::temperature(&sensors, &names))
            }
            Group::Printer => Ok(mibs::printer(
                &self.table(client, mibs::PRT_SUPPLIES).await?,
            )),
            Group::Detect => {
                let values = self
                    .get(
                        client,
                        &[mibs::SYS_DESCR, mibs::SYS_NAME, mibs::SYS_OBJECT_ID],
                    )
                    .await?;
                let text = |o: &[u32]| match values.get(o) {
                    Some(Val::Text(t)) => t.clone(),
                    Some(Val::Oid(arcs)) => arcs
                        .iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join("."),
                    _ => String::new(),
                };
                let mut has = Vec::new();
                for group in [
                    Group::Interfaces,
                    Group::Host,
                    Group::Ups,
                    Group::Temperature,
                    Group::Printer,
                ] {
                    has.push((group, self.answers_under(client, group.root()).await));
                }
                Ok(mibs::detect(
                    &text(mibs::SYS_DESCR),
                    &text(mibs::SYS_NAME),
                    &text(mibs::SYS_OBJECT_ID),
                    &has,
                ))
            }
        }
    }

    /// Whether the device has an object under `root`.
    async fn answers_under(&self, client: &Client, root: &[u32]) -> bool {
        match client.get_next(&oid(root)).await {
            Ok(response) => response
                .varbinds
                .iter()
                .any(|vb| vb.oid.arcs().starts_with(root) && !vb.value.is_exception()),
            Err(_) => false,
        }
    }
}

fn chrono_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[async_trait::async_trait]
impl Sensor for SnmpSensor {
    async fn collect(&self, deadline: Instant) -> Result<Vec<Sample>, SensorError> {
        let client = match self.client(deadline).await {
            Ok(client) => client,
            Err(error) if self.target.group == Group::System => {
                tracing::debug!(host = %self.target.host, %error, "no SNMP answer");
                return Ok(vec![Sample::metric("up", 0.0)]);
            }
            Err(error) => return Err(error),
        };
        match timeout_at(deadline, self.poll(&client)).await {
            Ok(Ok(samples)) => Ok(samples),
            // The device does not answer: the system check says so.
            Ok(Err(_)) | Err(_) if self.target.group == Group::System => {
                Ok(vec![Sample::metric("up", 0.0)])
            }
            Ok(Err(error)) => Err(error),
            Err(_) => Err(SensorError::Transient(
                "the device did not answer in time".into(),
            )),
        }
    }
}
