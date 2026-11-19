//! One device's SNMP session, shared by every kind of this crate: how to
//! reach and authenticate to it, a GET of a few objects and a walk.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_snmp::{Auth, Client, EngineCache, Oid, Retry, UsmConfig, Value};
use monitorflow_sensors::SensorError;
use tokio::time::{timeout_at, Instant};

use crate::mibs::Val;
use crate::Credentials;

/// Longest wait for one request; a collection retries once within its
/// deadline.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
/// Rows kept from one walk: past it, a table is cut.
pub(crate) const MAX_ROWS: usize = 5000;

/// Where a device is and how to speak to it.
pub(crate) struct Session {
    pub host: String,
    pub port: u16,
    credentials: Credentials,
    /// v3 engines learned, kept between collections.
    engines: Arc<EngineCache>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("credentials", &self.credentials)
            .finish_non_exhaustive()
    }
}

pub(crate) fn oid(arcs: &[u32]) -> Oid {
    Oid::from_slice(arcs)
}

/// A value as the common MIBs' parsers read it; exceptions and types they
/// do not use are `None`.
pub(crate) fn val(value: &Value) -> Option<Val> {
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

impl Session {
    pub fn new(host: String, port: u16, credentials: Credentials) -> Self {
        Self {
            host,
            port,
            credentials,
            engines: Arc::new(EngineCache::new()),
        }
    }

    fn auth(&self) -> Result<Auth, SensorError> {
        let permanent =
            |e: &dyn std::fmt::Display| SensorError::Permanent(format!("SNMP credentials: {e}"));
        Ok(match &self.credentials {
            Credentials::V1 { community } => Auth::v1(community.as_str()),
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

    pub fn is_v1(&self) -> bool {
        matches!(self.credentials, Credentials::V1 { .. })
    }

    pub async fn client(&self, deadline: Instant) -> Result<Client, SensorError> {
        let left = deadline.saturating_duration_since(Instant::now());
        let request = REQUEST_TIMEOUT
            .min(left / 2)
            .max(Duration::from_millis(200));
        let builder = Client::builder((self.host.as_str(), self.port), self.auth()?)
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

    /// The raw values of `oids`, by OID, exceptions included. SNMPv1
    /// answers a whole request with an error when one object is missing,
    /// so a failed request of several objects is asked again one by one.
    pub async fn get_raw(
        &self,
        client: &Client,
        oids: &[Vec<u32>],
    ) -> Result<BTreeMap<Vec<u32>, Value>, SensorError> {
        let list: Vec<Oid> = oids.iter().map(|o| oid(o)).collect();
        let failed =
            |e: &dyn std::fmt::Display| SensorError::Transient(format!("SNMP request failed: {e}"));
        match client.get_many(&list).await {
            Ok(response) => Ok(response
                .varbinds
                .iter()
                .map(|vb| (vb.oid.arcs().to_vec(), vb.value.clone()))
                .collect()),
            Err(error) if list.len() > 1 && self.is_v1() => {
                tracing::debug!(host = %self.host, %error, "SNMPv1 request failed; asking one object at a time");
                let mut out = BTreeMap::new();
                for one in &list {
                    match client.get(one).await {
                        Ok(response) => out.extend(
                            response
                                .varbinds
                                .iter()
                                .map(|vb| (vb.oid.arcs().to_vec(), vb.value.clone())),
                        ),
                        Err(error) => {
                            tracing::debug!(host = %self.host, oid = %one, %error, "no answer")
                        }
                    }
                }
                if out.is_empty() {
                    return Err(failed(&error));
                }
                Ok(out)
            }
            Err(error) => Err(failed(&error)),
        }
    }

    /// The values of `oids`, by OID; absent ones left out.
    pub async fn get(
        &self,
        client: &Client,
        oids: &[&[u32]],
    ) -> Result<BTreeMap<Vec<u32>, Val>, SensorError> {
        let list: Vec<Vec<u32>> = oids.iter().map(|o| o.to_vec()).collect();
        Ok(self
            .get_raw(client, &list)
            .await?
            .iter()
            .filter_map(|(o, v)| Some((o.clone(), val(v)?)))
            .collect())
    }

    /// Every object under `root`, at most `max`, with its raw value: with
    /// GETBULK, or GETNEXT when the agent answers GETBULK with an error,
    /// as net-snmp does to some bulk requests over v3. The flag says
    /// whether the walk stopped at `max`.
    pub async fn walk_raw(
        &self,
        client: &Client,
        root: &[u32],
        max: usize,
    ) -> Result<(Vec<(Vec<u32>, Value)>, bool), SensorError> {
        match self.walk_with(client, root, true, max).await {
            Ok(rows) => Ok(rows),
            Err(error) => {
                tracing::debug!(host = %self.host, %error, "bulk walk failed; walking one row at a time");
                self.walk_with(client, root, false, max).await
            }
        }
    }

    /// Every row under `root`, at most [`MAX_ROWS`], as the common MIBs'
    /// parsers read them.
    pub async fn walk(
        &self,
        client: &Client,
        root: &[u32],
    ) -> Result<Vec<(Vec<u32>, Val)>, SensorError> {
        let (rows, cut) = self.walk_raw(client, root, MAX_ROWS).await?;
        if cut {
            tracing::debug!(host = %self.host, "table cut at {MAX_ROWS} rows");
        }
        Ok(rows
            .into_iter()
            .filter_map(|(o, v)| Some((o, val(&v)?)))
            .collect())
    }

    async fn walk_with(
        &self,
        client: &Client,
        root: &[u32],
        bulk: bool,
        max: usize,
    ) -> Result<(Vec<(Vec<u32>, Value)>, bool), SensorError> {
        let failed =
            |e: &dyn std::fmt::Display| SensorError::Transient(format!("SNMP walk failed: {e}"));
        // SNMPv1 has no GETBULK: the library walks it with GETNEXT.
        let mut stream = if bulk || self.is_v1() {
            client.walk(oid(root))
        } else {
            client.walk_getnext(oid(root))
        }
        .map_err(|e| failed(&e))?;
        let mut rows = Vec::new();
        while let Some(item) = stream.next().await {
            let vb = item.map_err(|e| failed(&e))?;
            if vb.value.is_exception() {
                continue;
            }
            if rows.len() >= max {
                return Ok((rows, true));
            }
            rows.push((vb.oid.arcs().to_vec(), vb.value));
        }
        Ok((rows, false))
    }

    /// Whether the device has an object under `root`.
    pub async fn answers_under(&self, client: &Client, root: &[u32]) -> bool {
        match client.get_next(&oid(root)).await {
            Ok(response) => response
                .varbinds
                .iter()
                .any(|vb| vb.oid.arcs().starts_with(root) && !vb.value.is_exception()),
            Err(_) => false,
        }
    }
}
