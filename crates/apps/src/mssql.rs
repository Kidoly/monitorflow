//! `mssql`: SQL Server from its dynamic management views: connections,
//! batch requests, page life expectancy, buffer cache hit ratio, blocked
//! processes, locks and deadlocks, the size and log use of each database,
//! and the replicas of its Always On availability groups.
//!
//! Parameters: `host` (`127.0.0.1`), `port` (1433), `user`
//! (`monitorflow`), `password`, `tls` (`prefer`, `verify` or `off`). A
//! SQL Server login with `VIEW SERVER STATE` (`VIEW SERVER PERFORMANCE
//! STATE` is enough from SQL Server 2022) is all the check needs: it
//! reads no database's data.

use std::collections::BTreeMap;
use std::time::Instant;

use monitorflow_sensors::{Sample, Sensor, SensorError};

use crate::rates::{Rates, Round};
pub(crate) use crate::tds::TdsError;
use crate::tds::{Client, Encryption, Login, Row, Value};
use crate::{down, gauge, label, ratio, within, Failure, Params, Place, Reason, Target, Tls};

/// Databases whose sizes are reported, the largest first.
const MAX_DATABASES: usize = 50;
/// Availability replicas and their databases reported.
const MAX_REPLICAS: usize = 50;
/// What SQL Server allows when `user connections` is 0, its default.
const DEFAULT_MAX_CONNECTIONS: f64 = 32_767.0;

const SERVER: &str = "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(64)),
       CAST(SERVERPROPERTY('Edition') AS NVARCHAR(128)),
       CAST(ISNULL(SERVERPROPERTY('IsHadrEnabled'), 0) AS INT),
       (SELECT CAST(value_in_use AS INT) FROM sys.configurations WHERE name = 'user connections')";

const COUNTERS: &str = "SELECT RTRIM(SUBSTRING(object_name, CHARINDEX(':', object_name) + 1, 128)),
       RTRIM(counter_name), RTRIM(instance_name), cntr_value
FROM sys.dm_os_performance_counters
WHERE counter_name IN ('User Connections', 'Processes blocked', 'Batch Requests/sec',
    'Page life expectancy', 'Buffer cache hit ratio', 'Buffer cache hit ratio base',
    'Lock Waits/sec', 'Number of Deadlocks/sec',
    'Data File(s) Size (KB)', 'Log File(s) Size (KB)', 'Log File(s) Used Size (KB)')";

const REQUESTS: &str = "SELECT CAST(ISNULL(MAX(CASE WHEN r.blocking_session_id > 0 THEN r.wait_time END), 0) AS BIGINT),
       CAST(ISNULL(MAX(r.total_elapsed_time), 0) AS BIGINT)
FROM sys.dm_exec_requests AS r
JOIN sys.dm_exec_sessions AS s ON s.session_id = r.session_id
WHERE s.is_user_process = 1 AND r.session_id <> @@SPID";

const REPLICAS: &str =
    "SELECT CAST(g.name AS NVARCHAR(128)), CAST(c.replica_server_name AS NVARCHAR(256)),
       CAST(s.role AS INT), CAST(s.connected_state AS INT), CAST(s.synchronization_health AS INT)
FROM sys.dm_hadr_availability_replica_states AS s
JOIN sys.dm_hadr_availability_replica_cluster_states AS c ON c.replica_id = s.replica_id
JOIN sys.availability_groups_cluster AS g ON g.group_id = s.group_id";

/// `secondary_lag_seconds` exists from SQL Server 2016 (13.x).
fn replica_databases(lag: bool) -> String {
    let lag = if lag {
        "CAST(d.secondary_lag_seconds AS BIGINT)"
    } else {
        "CAST(NULL AS BIGINT)"
    };
    format!(
        "SELECT CAST(g.name AS NVARCHAR(128)), CAST(c.replica_server_name AS NVARCHAR(256)),
       CAST(DB_NAME(d.database_id) AS NVARCHAR(128)),
       CAST(d.log_send_queue_size AS BIGINT), CAST(d.redo_queue_size AS BIGINT), {lag}
FROM sys.dm_hadr_database_replica_states AS d
JOIN sys.dm_hadr_availability_replica_cluster_states AS c ON c.replica_id = d.replica_id
JOIN sys.availability_groups_cluster AS g ON g.group_id = d.group_id"
    )
}

/// Databases of SQL Server's own the counters list but nobody runs.
const HIDDEN: [&str; 4] = [
    "_Total",
    "mssqlsystemresource",
    "model_msdb",
    "model_replicatedmaster",
];

#[derive(Debug)]
pub struct Mssql {
    target: Target,
    rates: Rates,
}

/// What one collection read, before it becomes samples.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Readings {
    pub version: String,
    pub edition: String,
    pub max_connections: Option<f64>,
    /// `(object, counter, instance)` to value.
    pub counters: BTreeMap<(String, String, String), f64>,
    /// Longest wait of a blocked request, and longest request, in ms.
    pub longest_blocked_ms: Option<f64>,
    pub longest_query_ms: Option<f64>,
    pub replicas: Vec<Replica>,
    pub replica_databases: Vec<ReplicaDatabase>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Replica {
    pub group: String,
    pub server: String,
    /// 1 primary, 2 secondary, 0 resolving.
    pub role: i64,
    pub connected: bool,
    /// 0 not healthy, 1 partially, 2 healthy.
    pub health: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ReplicaDatabase {
    pub group: String,
    pub server: String,
    pub database: String,
    /// Kilobytes, as the view gives them.
    pub log_send_queue_kb: Option<f64>,
    pub redo_queue_kb: Option<f64>,
    pub lag_seconds: Option<f64>,
}

impl Mssql {
    pub(crate) fn from_params(
        p: &mut Params<'_>,
        place: Place<'_>,
    ) -> Result<Self, monitorflow_sensors::BuildError> {
        Ok(Self {
            target: Target::parse(p, place, 1433, Some("monitorflow"), Tls::Prefer)?,
            rates: Rates::default(),
        })
    }

    async fn read(&self, round: &mut Round) -> Result<Vec<Sample>, Failure> {
        let started = Instant::now();
        let t = &self.target;
        let login = Login {
            host: &t.host,
            user: t.user.as_deref().unwrap_or("monitorflow"),
            password: t.password.as_deref().unwrap_or(""),
            encryption: match t.tls {
                Tls::Off => Encryption::Off,
                Tls::Prefer => Encryption::Unverified,
                Tls::Verify => Encryption::Verified,
            },
            roots: (t.tls == Tls::Verify).then(crate::roots),
        };
        let mut client = Client::connect(t.port, &login)
            .await
            .map_err(|e| failure(&e))?;
        let result = queries(&mut client, started).await;
        client.close().await;
        let (readings, response) = result.map_err(|e| failure(&e))?;
        Ok(samples(&readings, response, round))
    }
}

async fn queries(client: &mut Client, started: Instant) -> Result<(Readings, f64), TdsError> {
    let server = client.query(SERVER).await?;
    let response = started.elapsed().as_secs_f64();
    let mut readings = Readings::default();
    let first = server.first().map(Vec::as_slice).unwrap_or_default();
    let text = |row: &[Value], i: usize| row.get(i).and_then(Value::text).unwrap_or("").to_owned();
    let num = |row: &[Value], i: usize| row.get(i).and_then(Value::number);
    readings.version = text(first, 0);
    readings.edition = text(first, 1);
    let hadr = num(first, 2).is_some_and(|v| v > 0.0);
    readings.max_connections =
        num(first, 3).map(|v| if v > 0.0 { v } else { DEFAULT_MAX_CONNECTIONS });

    for row in client.query(COUNTERS).await? {
        if let Some(value) = num(&row, 3) {
            let key = (text(&row, 0), text(&row, 1), text(&row, 2));
            readings.counters.insert(key, value);
        }
    }
    let requests = client.query(REQUESTS).await?;
    if let Some(row) = requests.first() {
        readings.longest_blocked_ms = num(row, 0);
        readings.longest_query_ms = num(row, 1);
    }
    if hadr {
        readings.replicas = client
            .query(REPLICAS)
            .await?
            .iter()
            .take(MAX_REPLICAS)
            .map(|row| Replica {
                group: text(row, 0),
                server: text(row, 1),
                role: num(row, 2).unwrap_or(0.0) as i64,
                connected: num(row, 3).is_some_and(|v| v > 0.0),
                health: num(row, 4).unwrap_or(0.0) as i64,
            })
            .collect();
        let major = readings
            .version
            .split('.')
            .next()
            .and_then(|m| m.parse::<u32>().ok())
            .unwrap_or(0);
        readings.replica_databases = client
            .query(&replica_databases(major >= 13))
            .await?
            .iter()
            .take(MAX_REPLICAS)
            .map(|row: &Row| ReplicaDatabase {
                group: text(row, 0),
                server: text(row, 1),
                database: text(row, 2),
                log_send_queue_kb: num(row, 3),
                redo_queue_kb: num(row, 4),
                lag_seconds: num(row, 5),
            })
            .collect();
    }
    Ok((readings, response))
}

/// The samples of one collection.
pub(crate) fn samples(r: &Readings, response: f64, round: &mut Round) -> Vec<Sample> {
    let counter = |object: &str, name: &str, instance: &str| {
        r.counters
            .get(&(object.to_owned(), name.to_owned(), instance.to_owned()))
            .copied()
    };
    let mut out = vec![
        Sample::metric("up", 1.0)
            .with_label("version", label(&r.version, 64))
            .with_label("edition", label(&r.edition, 64)),
        Sample::metric("response_seconds", response),
    ];
    let connections = counter("General Statistics", "User Connections", "");
    out.extend(connections.and_then(|c| gauge("connections", c)));
    out.extend(r.max_connections.and_then(|m| gauge("connections_max", m)));
    if let (Some(c), Some(m)) = (connections, r.max_connections) {
        out.extend(ratio(c, m).map(|v| Sample::metric("connections_ratio", v)));
    }
    out.extend(
        counter("SQL Statistics", "Batch Requests/sec", "")
            .and_then(|v| round.rate("batches", v))
            .and_then(|v| gauge("batch_requests_per_second", v)),
    );
    out.extend(
        counter("Buffer Manager", "Page life expectancy", "")
            .and_then(|v| gauge("page_life_expectancy_seconds", v)),
    );
    if let (Some(hits), Some(base)) = (
        counter("Buffer Manager", "Buffer cache hit ratio", ""),
        counter("Buffer Manager", "Buffer cache hit ratio base", ""),
    ) {
        out.extend(ratio(hits, base).map(|v| Sample::metric("buffer_cache_hit_ratio", v)));
    }
    out.extend(
        counter("General Statistics", "Processes blocked", "")
            .and_then(|v| gauge("processes_blocked", v)),
    );
    out.extend(
        r.longest_blocked_ms
            .and_then(|v| gauge("longest_blocked_seconds", v / 1000.0)),
    );
    out.extend(
        r.longest_query_ms
            .and_then(|v| gauge("longest_query_seconds", v / 1000.0)),
    );
    out.extend(
        counter("Locks", "Lock Waits/sec", "_Total")
            .and_then(|v| round.rate("lock_waits", v))
            .and_then(|v| gauge("lock_waits_per_second", v)),
    );
    out.extend(
        counter("Locks", "Number of Deadlocks/sec", "_Total")
            .and_then(|v| round.delta("deadlocks", v))
            .and_then(|v| gauge("deadlocks", v)),
    );

    // Each database: data, log, and how full the log is.
    let mut databases: Vec<(&str, f64)> = r
        .counters
        .iter()
        .filter(|((object, name, db), _)| {
            object == "Databases"
                && name == "Data File(s) Size (KB)"
                && !HIDDEN.contains(&db.as_str())
        })
        .map(|((_, _, db), kb)| (db.as_str(), *kb))
        .collect();
    databases.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    for (db, data_kb) in databases.into_iter().take(MAX_DATABASES) {
        let instance = label(db, 128);
        out.push(Sample::metric("database_size_bytes", data_kb * 1024.0).with_instance(&instance));
        let log = counter("Databases", "Log File(s) Size (KB)", db);
        let used = counter("Databases", "Log File(s) Used Size (KB)", db);
        if let Some(log) = log {
            out.push(Sample::metric("log_size_bytes", log * 1024.0).with_instance(&instance));
        }
        if let Some(used) = log.zip(used).and_then(|(log, used)| ratio(used, log)) {
            out.push(Sample::metric("log_used_ratio", used).with_instance(&instance));
        }
    }

    for replica in &r.replicas {
        let instance = label(&format!("{}/{}", replica.group, replica.server), 200);
        let role = match replica.role {
            1 => "primary",
            2 => "secondary",
            _ => "resolving",
        };
        out.push(
            Sample::metric(
                "ag_replica_connected",
                f64::from(u8::from(replica.connected)),
            )
            .with_instance(&instance)
            .with_label("role", role),
        );
        out.push(
            Sample::metric(
                "ag_replica_healthy",
                f64::from(u8::from(replica.health == 2)),
            )
            .with_instance(&instance)
            .with_label("role", role),
        );
    }
    for db in &r.replica_databases {
        let instance = label(&format!("{}/{}/{}", db.group, db.server, db.database), 200);
        let kb = |v: Option<f64>| v.map(|v| v * 1024.0);
        for (metric, value) in [
            ("ag_log_send_queue_bytes", kb(db.log_send_queue_kb)),
            ("ag_redo_queue_bytes", kb(db.redo_queue_kb)),
            ("ag_secondary_lag_seconds", db.lag_seconds),
        ] {
            if let Some(sample) = value.and_then(|v| gauge(metric, v)) {
                out.push(sample.with_instance(&instance));
            }
        }
    }
    out
}

/// What a client error says about the server.
pub(crate) fn failure(error: &TdsError) -> Failure {
    let reason = match error {
        TdsError::Io(_) => Reason::Connect,
        TdsError::Tls(_) => Reason::Tls,
        TdsError::Protocol(_) => Reason::Protocol,
        TdsError::Server { number, .. } => match number {
            // Login failed, its password expired or must change, its
            // database cannot be opened, or SQL Server sign-in is off.
            18456 | 18452 | 18487 | 18488 | 4060 | 4064 | 18470 => Reason::Login,
            // Permission denied on an object, a view or the server.
            229 | 230 | 262 | 297 | 300 | 916 | 15247 => Reason::Permission,
            _ => Reason::Protocol,
        },
    };
    Failure::new(reason, error.to_string())
}

#[async_trait::async_trait]
impl Sensor for Mssql {
    async fn collect(&self, deadline: tokio::time::Instant) -> Result<Vec<Sample>, SensorError> {
        let mut round = self.rates.round();
        match within(deadline, self.read(&mut round)).await {
            Ok(samples) => {
                self.rates.finish(round);
                Ok(samples)
            }
            Err(failure) => {
                self.rates.reset();
                Ok(vec![down(Some(&self.target), &failure)])
            }
        }
    }
}
