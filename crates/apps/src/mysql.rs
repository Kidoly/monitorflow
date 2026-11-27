//! `mysql`: MySQL and MariaDB, from their global status and variables:
//! connections, queries, slow queries, InnoDB buffer pool hit ratio, row
//! lock waits, schema sizes and replication.
//!
//! Parameters: `host` (`127.0.0.1`), `port` (3306), `user`
//! (`monitorflow`), `password`, `tls` (`prefer`, `verify` or `off`).
//! `PROCESS` and `REPLICATION CLIENT` are all the account needs; schema
//! sizes cover the schemas it can see.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use monitorflow_sensors::{Sample, Sensor, SensorError};
use sqlx::mysql::{MySqlConnectOptions, MySqlConnection, MySqlRow, MySqlSslMode};
use sqlx::{ConnectOptions, Connection, Row};

use crate::rates::{Rates, Round};
use crate::{down, gauge, label, ratio, within, Failure, Params, Place, Reason, Target, Tls};

/// Schemas whose size is reported, the largest first.
const MAX_SCHEMAS: usize = 50;
/// Sizes are read again after this: summing `information_schema.tables`
/// costs more than the rest on a server with many tables.
const SIZES_EVERY: Duration = Duration::from_secs(10 * 60);

/// Schema names and sizes in bytes.
type Sizes = Vec<(String, f64)>;

#[derive(Debug)]
pub struct Mysql {
    target: Target,
    rates: Rates,
    /// The last schema sizes and when they were read.
    sizes: std::sync::Mutex<Option<(Instant, Sizes)>>,
}

impl Mysql {
    pub(crate) fn from_params(
        p: &mut Params<'_>,
        place: Place<'_>,
    ) -> Result<Self, monitorflow_sensors::BuildError> {
        Ok(Self {
            target: Target::parse(p, place, 3306, Some("monitorflow"), Tls::Prefer)?,
            rates: Rates::default(),
            sizes: std::sync::Mutex::default(),
        })
    }

    fn options(&self) -> MySqlConnectOptions {
        let t = &self.target;
        let mut options = MySqlConnectOptions::new()
            .host(&t.host)
            .port(t.port)
            .statement_cache_capacity(0)
            .ssl_mode(match t.tls {
                Tls::Off => MySqlSslMode::Disabled,
                Tls::Prefer => MySqlSslMode::Preferred,
                Tls::Verify | Tls::Strict => MySqlSslMode::VerifyIdentity,
            })
            .disable_statement_logging();
        if let Some(user) = &t.user {
            options = options.username(user);
        }
        if let Some(password) = &t.password {
            options = options.password(password);
        }
        options
    }

    async fn read(&self, round: &mut Round) -> Result<Vec<Sample>, Failure> {
        let started = Instant::now();
        let mut conn = MySqlConnection::connect_with(&self.options())
            .await
            .map_err(|e| failure(&e))?;
        let result = self.queries(&mut conn, round, started).await;
        let _ = conn.close().await;
        result
    }

    async fn queries(
        &self,
        conn: &mut MySqlConnection,
        round: &mut Round,
        started: Instant,
    ) -> Result<Vec<Sample>, Failure> {
        let fail = |e: sqlx::Error| failure(&e);
        let status = pairs(conn, "SHOW GLOBAL STATUS").await.map_err(fail)?;
        let response = started.elapsed().as_secs_f64();
        let variables = pairs(
            conn,
            "SHOW GLOBAL VARIABLES WHERE Variable_name IN ('version', 'version_comment', 'max_connections')",
        )
        .await
        .map_err(fail)?;
        let replica = replica_status(conn).await.map_err(fail)?;
        let num = |key: &str| status.get(key).and_then(|v| v.parse::<f64>().ok());

        let version = variables.get("version").map_or("", String::as_str);
        let comment = variables.get("version_comment").map_or("", String::as_str);
        let flavor = if format!("{version} {comment}")
            .to_ascii_lowercase()
            .contains("mariadb")
        {
            "mariadb"
        } else {
            "mysql"
        };
        let mut out = vec![
            Sample::metric("up", 1.0)
                .with_label("version", label(version, 64))
                .with_label("flavor", flavor)
                .with_label(
                    "role",
                    if replica.is_some() {
                        "replica"
                    } else {
                        "primary"
                    },
                ),
            Sample::metric("response_seconds", response),
        ];
        let connections = num("Threads_connected");
        let max = variables
            .get("max_connections")
            .and_then(|v| v.parse::<f64>().ok());
        out.extend(connections.and_then(|c| gauge("connections", c)));
        out.extend(max.and_then(|m| gauge("connections_max", m)));
        if let (Some(c), Some(m)) = (connections, max) {
            out.extend(ratio(c, m).map(|r| Sample::metric("connections_ratio", r)));
        }
        out.extend(num("Threads_running").and_then(|v| gauge("threads_running", v)));
        out.extend(
            num("Questions")
                .and_then(|v| round.rate("questions", v))
                .and_then(|v| gauge("queries_per_second", v)),
        );
        out.extend(
            num("Slow_queries")
                .and_then(|v| round.delta("slow", v))
                .and_then(|v| gauge("slow_queries", v)),
        );
        out.extend(
            num("Aborted_connects")
                .and_then(|v| round.delta("aborted", v))
                .and_then(|v| gauge("aborted_connections", v)),
        );
        if let (Some(requests), Some(reads)) = (
            num("Innodb_buffer_pool_read_requests"),
            num("Innodb_buffer_pool_reads"),
        ) {
            // Reads went to disk; requests include them.
            let hits = (requests - reads).max(0.0);
            out.extend(
                round
                    .hit_ratio("buffer_pool", hits, reads)
                    .and_then(|r| gauge("buffer_pool_hit_ratio", r)),
            );
        }
        out.extend(num("Innodb_row_lock_current_waits").and_then(|v| gauge("lock_waits", v)));

        for (schema, size) in self.sizes(conn).await.map_err(fail)? {
            out.push(Sample::metric("database_size_bytes", size).with_instance(schema));
        }

        if let Some(replica) = replica {
            let running = |keys: [&str; 2]| {
                let value = keys
                    .iter()
                    .find_map(|k| replica.get(*k))
                    .map_or("", String::as_str);
                f64::from(u8::from(value.eq_ignore_ascii_case("yes")))
            };
            out.push(Sample::metric(
                "replica_io_running",
                running(["Replica_IO_Running", "Slave_IO_Running"]),
            ));
            out.push(Sample::metric(
                "replica_sql_running",
                running(["Replica_SQL_Running", "Slave_SQL_Running"]),
            ));
            let behind = ["Seconds_Behind_Source", "Seconds_Behind_Master"]
                .iter()
                .find_map(|k| replica.get(*k))
                .and_then(|v| v.parse::<f64>().ok());
            out.extend(behind.and_then(|b| gauge("replication_delay_seconds", b)));
        }
        Ok(out)
    }

    /// The schema sizes, read at most every [`SIZES_EVERY`].
    async fn sizes(&self, conn: &mut MySqlConnection) -> Result<Vec<(String, f64)>, sqlx::Error> {
        {
            let kept = self.sizes.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((at, sizes)) = kept.as_ref() {
                if at.elapsed() < SIZES_EVERY {
                    return Ok(sizes.clone());
                }
            }
        }
        let rows: Vec<MySqlRow> = sqlx::raw_sql(
            "SELECT table_schema, SUM(data_length + index_length)
             FROM information_schema.tables
             WHERE table_schema NOT IN ('mysql', 'information_schema', 'performance_schema', 'sys')
             GROUP BY table_schema ORDER BY 2 DESC LIMIT 50",
        )
        .fetch_all(&mut *conn)
        .await?;
        let sizes: Vec<(String, f64)> = rows
            .iter()
            .filter_map(|row| {
                let name = text(row, 0)?;
                let size = text(row, 1)?.parse::<f64>().ok()?;
                Some((label(&name, 128), size))
            })
            .take(MAX_SCHEMAS)
            .collect();
        *self.sizes.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((Instant::now(), sizes.clone()));
        Ok(sizes)
    }
}

/// Column `index` of a text-protocol row as text; `None` for NULL.
fn text(row: &MySqlRow, index: usize) -> Option<String> {
    row.try_get_unchecked::<Option<String>, _>(index)
        .ok()
        .flatten()
}

/// The name and value rows of a `SHOW` statement.
async fn pairs(
    conn: &mut MySqlConnection,
    sql: &'static str,
) -> Result<HashMap<String, String>, sqlx::Error> {
    let rows: Vec<MySqlRow> = sqlx::raw_sql(sql).fetch_all(&mut *conn).await?;
    Ok(rows
        .iter()
        .filter_map(|row| Some((text(row, 0)?, text(row, 1).unwrap_or_default())))
        .collect())
}

/// The replica's status by column name, `None` on a server that is not a
/// replica. `SHOW REPLICA STATUS` exists from MySQL 8.0.22 and MariaDB
/// 10.5.1; older ones know `SHOW SLAVE STATUS` only.
async fn replica_status(
    conn: &mut MySqlConnection,
) -> Result<Option<HashMap<String, String>>, sqlx::Error> {
    let rows = match sqlx::raw_sql("SHOW REPLICA STATUS")
        .fetch_all(&mut *conn)
        .await
    {
        Ok(rows) => rows,
        // ER_PARSE_ERROR on an older server, or a missing privilege.
        Err(sqlx::Error::Database(_)) => {
            match sqlx::raw_sql("SHOW SLAVE STATUS")
                .fetch_all(&mut *conn)
                .await
            {
                Ok(rows) => rows,
                // Without REPLICATION CLIENT, the rest still says a lot:
                // replication is left out rather than the whole check.
                Err(sqlx::Error::Database(e)) => {
                    tracing::debug!(error = %e, "cannot read the replica status");
                    return Ok(None);
                }
                Err(e) => return Err(e),
            }
        }
        Err(e) => return Err(e),
    };
    // Several rows with MariaDB's multi-source replication: the first
    // one, the default connection, speaks for the server.
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    use sqlx::Column;
    let columns = row.columns();
    Ok(Some(
        columns
            .iter()
            .enumerate()
            .filter_map(|(i, c)| Some((c.name().to_owned(), text(row, i)?)))
            .collect(),
    ))
}

/// What a client error says about the server.
fn failure(error: &sqlx::Error) -> Failure {
    let reason = match error {
        sqlx::Error::Io(_) | sqlx::Error::PoolTimedOut => Reason::Connect,
        sqlx::Error::Tls(_) => Reason::Tls,
        sqlx::Error::Database(e) => match e.code().as_deref() {
            // ER_ACCESS_DENIED_ERROR, ER_DBACCESS_DENIED_ERROR
            Some("28000") => Reason::Login,
            // ER_SPECIFIC_ACCESS_DENIED_ERROR and other access errors
            Some("42000") => Reason::Permission,
            _ => Reason::Protocol,
        },
        _ => Reason::Protocol,
    };
    Failure::new(reason, error.to_string())
}

#[async_trait::async_trait]
impl Sensor for Mysql {
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
