//! `postgresql`: connections, long queries and transactions, waiting
//! locks, deadlocks, cache hit ratio, database sizes and replication.
//!
//! Parameters: `host` (`127.0.0.1`), `port` (5432), `user`
//! (`monitorflow`), `password`, `database` (`postgres`), `tls` (`prefer`,
//! `verify` or `off`) and `slow_after` (seconds after which a running
//! query counts as slow, 30 by default). A role with `pg_monitor` sees
//! every session and the replication; with less, PostgreSQL hides what
//! other roles run and the numbers cover the check's own sessions only.

use std::time::Instant;

use monitorflow_sensors::{Sample, Sensor, SensorError};
use sqlx::postgres::{PgConnectOptions, PgConnection, PgSslMode};
use sqlx::{ConnectOptions, Connection};

use crate::rates::{Rates, Round};
use crate::{down, gauge, label, ratio, within, Failure, Params, Place, Reason, Target, Tls};

/// Databases whose size is reported, the largest first.
const MAX_DATABASES: i64 = 50;
/// Replicas reported, by lag.
const MAX_REPLICAS: i64 = 20;
/// Longest database name, the most PostgreSQL allows.
const MAX_NAME: usize = 63;

/// Five sums of `pg_stat_database`, `NULL` without databases.
type Five = (
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
);

#[derive(Debug)]
pub struct Postgresql {
    target: Target,
    database: String,
    slow_after: u32,
    rates: Rates,
}

impl Postgresql {
    pub(crate) fn from_params(
        p: &mut Params<'_>,
        place: Place<'_>,
    ) -> Result<Self, monitorflow_sensors::BuildError> {
        let target = Target::parse(p, place, 5432, Some("monitorflow"), Tls::Prefer)?;
        let database = p.get("database").unwrap_or("postgres");
        if database.is_empty()
            || database.len() > MAX_NAME
            || database.chars().any(char::is_control)
        {
            return Err(p.bad(
                "database",
                "must be 1 to 63 bytes without control characters",
            ));
        }
        let slow_after = p.number("slow_after", 30, 1..=86_400)?;
        Ok(Self {
            target,
            database: database.to_owned(),
            slow_after,
            rates: Rates::default(),
        })
    }

    fn options(&self) -> PgConnectOptions {
        let t = &self.target;
        let mut options = PgConnectOptions::new_without_pgpass()
            .host(&t.host)
            .port(t.port)
            .database(&self.database)
            .application_name("monitorflow")
            .statement_cache_capacity(0)
            .ssl_mode(match t.tls {
                Tls::Off => PgSslMode::Disable,
                Tls::Prefer => PgSslMode::Prefer,
                Tls::Verify | Tls::Strict => PgSslMode::VerifyFull,
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
        let mut conn = PgConnection::connect_with(&self.options())
            .await
            .map_err(|e| failure(&e))?;
        let result = self.queries(&mut conn, round, started).await;
        // Closing politely spares the server a log line; a failure to do
        // so changes nothing.
        let _ = conn.close().await;
        result
    }

    async fn queries(
        &self,
        conn: &mut PgConnection,
        round: &mut Round,
        started: Instant,
    ) -> Result<Vec<Sample>, Failure> {
        let fail = |e: sqlx::Error| failure(&e);
        let (version, max_connections, in_recovery): (String, i32, bool) = sqlx::query_as(
            "SELECT current_setting('server_version'),
                    current_setting('max_connections')::int,
                    pg_is_in_recovery()",
        )
        .fetch_one(&mut *conn)
        .await
        .map_err(fail)?;
        let response = started.elapsed().as_secs_f64();
        let mut out = vec![
            Sample::metric("up", 1.0)
                .with_label("version", label(&version, 64))
                .with_label("role", if in_recovery { "replica" } else { "primary" }),
            Sample::metric("response_seconds", response),
        ];

        let (total, active, idle_tx, slow, longest_query, longest_tx): (
            i64,
            i64,
            i64,
            i64,
            Option<f64>,
            Option<f64>,
        ) = sqlx::query_as(
            "SELECT count(*),
                    count(*) FILTER (WHERE state = 'active'),
                    count(*) FILTER (WHERE state LIKE 'idle in transaction%'),
                    count(*) FILTER (WHERE state = 'active'
                                     AND now() - query_start > make_interval(secs => $1)),
                    max(extract(epoch FROM now() - query_start))
                        FILTER (WHERE state = 'active')::float8,
                    max(extract(epoch FROM now() - xact_start))::float8
             FROM pg_stat_activity
             WHERE backend_type = 'client backend' AND pid <> pg_backend_pid()",
        )
        .bind(f64::from(self.slow_after))
        .fetch_one(&mut *conn)
        .await
        .map_err(fail)?;
        let connections = total as f64 + 1.0;
        out.extend([
            Sample::metric("connections", connections),
            Sample::metric("connections_max", f64::from(max_connections)),
            Sample::metric("connections_active", active as f64),
            Sample::metric("connections_idle_in_transaction", idle_tx as f64),
            Sample::metric("slow_queries", slow as f64),
            Sample::metric(
                "longest_query_seconds",
                longest_query.unwrap_or(0.0).max(0.0),
            ),
            Sample::metric(
                "longest_transaction_seconds",
                longest_tx.unwrap_or(0.0).max(0.0),
            ),
        ]);
        out.extend(
            ratio(connections, f64::from(max_connections))
                .map(|r| Sample::metric("connections_ratio", r)),
        );

        let waiting: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_locks WHERE NOT granted")
            .fetch_one(&mut *conn)
            .await
            .map_err(fail)?;
        out.push(Sample::metric("locks_waiting", waiting as f64));

        let (hit, read, commits, rollbacks, deadlocks): Five = sqlx::query_as(
            "SELECT sum(blks_hit)::float8, sum(blks_read)::float8, sum(xact_commit)::float8,
                    sum(xact_rollback)::float8, sum(deadlocks)::float8
             FROM pg_stat_database",
        )
        .fetch_one(&mut *conn)
        .await
        .map_err(fail)?;
        let (hit, read) = (hit.unwrap_or(0.0), read.unwrap_or(0.0));
        out.extend(
            round
                .hit_ratio("blocks", hit, read)
                .and_then(|r| gauge("cache_hit_ratio", r)),
        );
        let transactions = commits.unwrap_or(0.0) + rollbacks.unwrap_or(0.0);
        out.extend(
            round
                .rate("transactions", transactions)
                .and_then(|r| gauge("transactions_per_second", r)),
        );
        out.extend(
            round
                .delta("deadlocks", deadlocks.unwrap_or(0.0))
                .and_then(|d| gauge("deadlocks", d)),
        );

        let sizes: Vec<(String, i64)> = sqlx::query_as(
            "SELECT datname, pg_database_size(oid) FROM pg_database
             WHERE datallowconn AND NOT datistemplate
               AND has_database_privilege(oid, 'CONNECT')
             ORDER BY 2 DESC LIMIT $1",
        )
        .bind(MAX_DATABASES)
        .fetch_all(&mut *conn)
        .await
        .map_err(fail)?;
        for (name, size) in sizes {
            out.push(
                Sample::metric("database_size_bytes", size as f64).with_instance(label(&name, 128)),
            );
        }

        if in_recovery {
            out.extend(standby(conn).await.map_err(fail)?);
        } else {
            out.extend(primary(conn).await.map_err(fail)?);
        }
        Ok(out)
    }
}

/// A primary's replicas: how many stream from it, and how far behind each
/// one replays.
async fn primary(conn: &mut PgConnection) -> Result<Vec<Sample>, sqlx::Error> {
    let rows: Vec<(String, Option<f64>, Option<f64>)> = sqlx::query_as(
        "SELECT coalesce(nullif(application_name, ''), host(client_addr), pid::text),
                pg_wal_lsn_diff(pg_current_wal_lsn(), replay_lsn)::float8,
                extract(epoch FROM replay_lag)::float8
         FROM pg_stat_replication
         ORDER BY 2 DESC NULLS LAST LIMIT $1",
    )
    .bind(MAX_REPLICAS)
    .fetch_all(&mut *conn)
    .await?;
    let mut out = vec![Sample::metric("replicas", rows.len() as f64)];
    for (name, bytes, seconds) in rows {
        let name = label(&name, 128);
        out.extend(
            bytes.map(|b| Sample::metric("replica_lag_bytes", b.max(0.0)).with_instance(&name)),
        );
        out.extend(
            seconds.map(|s| Sample::metric("replica_lag_seconds", s.max(0.0)).with_instance(&name)),
        );
    }
    Ok(out)
}

/// A standby: whether it receives the primary's WAL, and how old what it
/// replayed last is (0 when it replayed everything it received).
async fn standby(conn: &mut PgConnection) -> Result<Vec<Sample>, sqlx::Error> {
    let (delay, receiving): (Option<f64>, i64) = sqlx::query_as(
        "SELECT CASE WHEN pg_last_wal_receive_lsn() = pg_last_wal_replay_lsn() THEN 0
                     ELSE extract(epoch FROM now() - pg_last_xact_replay_timestamp())
                END::float8,
                (SELECT count(*) FROM pg_stat_wal_receiver WHERE status = 'streaming')",
    )
    .fetch_one(&mut *conn)
    .await?;
    let mut out = vec![Sample::metric("wal_receiver_up", receiving.min(1) as f64)];
    out.extend(delay.map(|d| Sample::metric("replication_delay_seconds", d.max(0.0))));
    Ok(out)
}

/// What a client error says about the server.
fn failure(error: &sqlx::Error) -> Failure {
    let reason = match error {
        sqlx::Error::Io(_) | sqlx::Error::PoolTimedOut => Reason::Connect,
        sqlx::Error::Tls(_) => Reason::Tls,
        sqlx::Error::Database(e) => match e.code().as_deref() {
            // invalid_password, invalid_authorization_specification
            Some("28P01" | "28000") => Reason::Login,
            // insufficient_privilege, invalid_catalog_name
            Some("42501" | "3D000") => Reason::Permission,
            _ => Reason::Protocol,
        },
        _ => Reason::Protocol,
    };
    Failure::new(reason, error.to_string())
}

#[async_trait::async_trait]
impl Sensor for Postgresql {
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
