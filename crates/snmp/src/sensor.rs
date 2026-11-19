//! The `snmp` sensor: polls one group of a device per collection.

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_snmp::Client;
use monitorflow_sensors::{Sample, Sensor, SensorError};
use tokio::time::{timeout_at, Instant};

use crate::mibs::{self, Counters, Group, Table, Val};
use crate::session::Session;
use crate::Target;

pub struct SnmpSensor {
    group: Group,
    session: Session,
    /// Interface counters of the last collection, for rates.
    counters: Mutex<BTreeMap<String, Counters>>,
}

impl std::fmt::Debug for SnmpSensor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SnmpSensor")
            .field("group", &self.group)
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

impl SnmpSensor {
    pub fn new(target: Target) -> Self {
        Self {
            group: target.group,
            session: Session::new(target.host, target.port, target.credentials),
            counters: Mutex::default(),
        }
    }

    async fn get(
        &self,
        client: &Client,
        oids: &[&[u32]],
    ) -> Result<BTreeMap<Vec<u32>, Val>, SensorError> {
        self.session.get(client, oids).await
    }

    async fn walk(
        &self,
        client: &Client,
        root: &[u32],
    ) -> Result<Vec<(Vec<u32>, Val)>, SensorError> {
        self.session.walk(client, root).await
    }

    async fn table(&self, client: &Client, root: &[u32]) -> Result<Table, SensorError> {
        Ok(mibs::table(root, self.walk(client, root).await?))
    }

    async fn poll(&self, client: &Client) -> Result<Vec<Sample>, SensorError> {
        match self.group {
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
                let now = monitorflow_sensors::now_unix_ms();
                let mut kept = self.counters.lock().unwrap_or_else(|e| e.into_inner());
                let (samples, counters) = mibs::interfaces(&if_table, &ifx_table, &kept, now);
                *kept = counters;
                Ok(samples)
            }
            Group::Host => {
                // Some agents answer the processor table with an error for
                // a while after they start: memory and storage still count.
                let loads: Vec<f64> = match self.walk(client, mibs::HR_PROCESSOR_LOAD).await {
                    Ok(rows) => rows.iter().filter_map(|(_, v)| v.number()).collect(),
                    Err(error) => {
                        tracing::debug!(host = %self.session.host, %error, "no processor load");
                        Vec::new()
                    }
                };
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
                    has.push((
                        group,
                        self.session.answers_under(client, group.root()).await,
                    ));
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
}

#[async_trait::async_trait]
impl Sensor for SnmpSensor {
    async fn collect(&self, deadline: Instant) -> Result<Vec<Sample>, SensorError> {
        let client = match self.session.client(deadline).await {
            Ok(client) => client,
            Err(error) if self.group == Group::System => {
                tracing::debug!(host = %self.session.host, %error, "no SNMP answer");
                return Ok(vec![Sample::metric("up", 0.0)]);
            }
            Err(error) => return Err(error),
        };
        match timeout_at(deadline, self.poll(&client)).await {
            Ok(Ok(samples)) => Ok(samples),
            // The device does not answer: the system check says so.
            Ok(Err(_)) | Err(_) if self.group == Group::System => {
                Ok(vec![Sample::metric("up", 0.0)])
            }
            Ok(Err(error)) => Err(error),
            Err(_) => Err(SensorError::Transient(
                "the device did not answer in time".into(),
            )),
        }
    }
}
