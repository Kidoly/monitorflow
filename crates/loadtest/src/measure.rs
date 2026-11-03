//! What the run measures: CPU and memory of the server and relays from
//! `/proc`, PostgreSQL connections and statements, VictoriaMetrics
//! ingestion, and established gRPC connections, sampled every few seconds.

use std::time::Duration;

use sqlx::PgPool;
use tokio::sync::watch;
use tokio::time::Instant;

/// CPU time and resident memory of one process.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ProcStats {
    /// User and system time, in clock ticks.
    pub cpu_ticks: u64,
    pub rss_bytes: u64,
    pub threads: u64,
}

/// Fields 14 and 15 (utime, stime) and 20 (threads) of `/proc/<pid>/stat`.
/// The command name in field 2 may hold spaces, so fields count from the
/// last `)`.
pub fn parse_stat(stat: &str) -> Option<(u64, u64)> {
    let rest = &stat[stat.rfind(')')? + 2..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // `rest` starts at field 3.
    let field = |n: usize| fields.get(n - 3)?.parse::<u64>().ok();
    Some((field(14)? + field(15)?, field(20)?))
}

/// `VmRSS` of `/proc/<pid>/status`, in bytes.
pub fn parse_rss(status: &str) -> Option<u64> {
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

pub fn proc_stats(pid: u32) -> Option<ProcStats> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let (cpu_ticks, threads) = parse_stat(&stat)?;
    Some(ProcStats {
        cpu_ticks,
        rss_bytes: parse_rss(&status)?,
        threads,
    })
}

/// Connections in state ESTABLISHED whose local port is `port`, from the
/// text of `/proc/net/tcp` or `/proc/net/tcp6`.
pub fn parse_established(table: &str, port: u16) -> usize {
    let wanted = format!(":{port:04X}");
    table
        .lines()
        .skip(1)
        .filter(|line| {
            let mut fields = line.split_whitespace();
            let local = fields.nth(1).unwrap_or_default();
            let state = fields.nth(1).unwrap_or_default();
            local.ends_with(&wanted) && state == "01"
        })
        .count()
}

/// Established connections to a listener on `port` of this host.
pub fn established(port: u16) -> usize {
    ["/proc/net/tcp", "/proc/net/tcp6"]
        .iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .map(|table| parse_established(&table, port))
        .sum()
}

/// Sum of `vm_rows_inserted_total` in VictoriaMetrics' own metrics: every
/// row it accepted, whatever the protocol.
pub fn parse_vm_rows(metrics: &str) -> f64 {
    metrics
        .lines()
        .filter(|line| line.starts_with("vm_rows_inserted_total"))
        .filter_map(|line| line.rsplit(' ').next()?.parse::<f64>().ok())
        .sum()
}

/// VictoriaMetrics as the server reaches it: plain HTTP, or TLS from the
/// server's CA with basic auth (ADR-0087).
#[derive(Clone)]
pub struct Store {
    client: reqwest::Client,
    base: String,
    password: Option<String>,
}

impl Store {
    pub fn plain(base: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            base: base.to_owned(),
            password: None,
        }
    }

    /// Trusts `ca_pem` alone and logs in as the server does.
    pub fn tls(base: &str, ca_pem: &[u8], password: &str) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .tls_built_in_root_certs(false)
            .add_root_certificate(reqwest::Certificate::from_pem(ca_pem)?)
            .build()?;
        Ok(Self {
            client,
            base: base.to_owned(),
            password: Some(password.trim().to_owned()),
        })
    }

    async fn get(&self, path: &str) -> Option<String> {
        let mut request = self.client.get(format!("{}{path}", self.base));
        if let Some(password) = &self.password {
            request = request.basic_auth("monitorflow", Some(password));
        }
        let response = request.send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        response.text().await.ok()
    }
}

pub async fn vm_rows(store: &Store) -> Option<f64> {
    Some(parse_vm_rows(&store.get("/metrics").await?))
}

/// One reading of everything.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// Seconds since the sampler started.
    pub at: f64,
    /// Percent of one core, since the previous snapshot.
    pub server_cpu: f64,
    pub server_rss: u64,
    pub relay_cpu: f64,
    pub relay_rss: u64,
    pub loadtest_cpu: f64,
    pub pg_connections: i64,
    pub pg_active: i64,
    /// Rows a second VictoriaMetrics accepted since the previous snapshot.
    pub vm_rows_per_s: f64,
    pub grpc_connections: usize,
}

/// Processes to watch; the server's PID changes when it restarts.
#[derive(Debug, Clone, Default)]
pub struct Watched {
    pub server: Option<u32>,
    pub relays: Vec<u32>,
}

/// The readings of the previous snapshot, to compute rates.
struct Previous {
    at: Instant,
    server: Option<ProcStats>,
    relays: Vec<ProcStats>,
    own: ProcStats,
    vm_rows: f64,
}

/// Samples every `every` until the sender of `watched` is dropped.
pub async fn sample(
    every: Duration,
    mut watched: watch::Receiver<Watched>,
    pool: PgPool,
    victoriametrics: Store,
    grpc_port: u16,
    out: tokio::sync::mpsc::UnboundedSender<Snapshot>,
) {
    let start = Instant::now();
    let ticks = rustix::param::clock_ticks_per_second() as f64;
    let mut ticker = tokio::time::interval(every);
    let mut last: Option<Previous> = None;
    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            changed = watched.changed() => {
                if changed.is_err() { return }
                continue;
            }
        }
        let now = Instant::now();
        let current = watched.borrow().clone();
        let server = current.server.and_then(proc_stats);
        let relays: Vec<ProcStats> = current
            .relays
            .iter()
            .filter_map(|p| proc_stats(*p))
            .collect();
        let own = proc_stats(std::process::id()).unwrap_or_default();
        let rows = vm_rows(&victoriametrics).await.unwrap_or_default();
        let (pg_connections, pg_active) = pg_connections(&pool).await.unwrap_or_default();
        let mut snapshot = Snapshot {
            at: now.duration_since(start).as_secs_f64(),
            server_rss: server.map_or(0, |s| s.rss_bytes),
            relay_rss: relays.iter().map(|s| s.rss_bytes).sum(),
            pg_connections,
            pg_active,
            grpc_connections: established(grpc_port),
            ..Snapshot::default()
        };
        if let Some(previous) = &last {
            let dt = now.duration_since(previous.at).as_secs_f64();
            let percent = |a: u64, b: u64| a.saturating_sub(b) as f64 / ticks / dt * 100.0;
            if let (Some(a), Some(b)) = (server, previous.server) {
                snapshot.server_cpu = percent(a.cpu_ticks, b.cpu_ticks);
            }
            if relays.len() == previous.relays.len() {
                let sum = |r: &[ProcStats]| r.iter().map(|s| s.cpu_ticks).sum::<u64>();
                snapshot.relay_cpu = percent(sum(&relays), sum(&previous.relays));
            }
            snapshot.loadtest_cpu = percent(own.cpu_ticks, previous.own.cpu_ticks);
            snapshot.vm_rows_per_s = (rows - previous.vm_rows).max(0.0) / dt;
            if out.send(snapshot).is_err() {
                return;
            }
        }
        last = Some(Previous {
            at: now,
            server,
            relays,
            own,
            vm_rows: rows,
        });
    }
}

/// Connections of every client to the server's database, and how many
/// run a statement now (the sampler's own excluded).
pub async fn pg_connections(pool: &PgPool) -> Result<(i64, i64), sqlx::Error> {
    sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE state = 'active')
         FROM pg_stat_activity
         WHERE datname = current_database() AND pid <> pg_backend_pid()",
    )
    .fetch_one(pool)
    .await
}

/// One statement of `pg_stat_statements`.
#[derive(Debug, Clone)]
pub struct Statement {
    pub query: String,
    pub calls: i64,
    pub total_ms: f64,
    pub mean_ms: f64,
    pub max_ms: f64,
}

/// Starts `pg_stat_statements` over, so a phase sees only its own.
pub async fn reset_statements(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_stat_statements_reset()")
        .execute(pool)
        .await?;
    Ok(())
}

/// The statements of the server's database that took the most time.
pub async fn top_statements(pool: &PgPool, limit: i64) -> Result<Vec<Statement>, sqlx::Error> {
    let rows: Vec<(String, i64, f64, f64, f64)> = sqlx::query_as(
        "SELECT s.query, s.calls, s.total_exec_time, s.mean_exec_time, s.max_exec_time
         FROM pg_stat_statements s JOIN pg_database d ON d.oid = s.dbid
         WHERE d.datname = current_database()
           AND s.query NOT ILIKE '%pg_stat%'
         ORDER BY s.total_exec_time DESC
         LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(query, calls, total_ms, mean_ms, max_ms)| Statement {
            query,
            calls,
            total_ms,
            mean_ms,
            max_ms,
        })
        .collect())
}

/// Percentiles of a set of durations or values.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Spread {
    pub count: usize,
    pub p50: f64,
    pub p95: f64,
    pub max: f64,
}

impl Spread {
    pub fn of(mut values: Vec<f64>) -> Self {
        if values.is_empty() {
            return Self::default();
        }
        values.sort_by(f64::total_cmp);
        let at = |q: f64| values[((values.len() - 1) as f64 * q).round() as usize];
        Self {
            count: values.len(),
            p50: at(0.5),
            p95: at(0.95),
            max: values[values.len() - 1],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_fields_count_from_the_command_name() {
        let stat = "4242 (monitorflow server) S 1 4242 4242 0 -1 4194560 2000 0 0 0 \
                    150 25 0 0 20 0 17 0 12345 1000000 5000 18446744073709551615";
        assert_eq!(parse_stat(stat), Some((175, 17)));
        assert_eq!(parse_stat("garbage"), None);
    }

    #[test]
    fn rss_is_read_in_bytes() {
        let status =
            "Name:\tmonitorflow-ser\nVmPeak:\t  999 kB\nVmRSS:\t   2048 kB\nThreads:\t17\n";
        assert_eq!(parse_rss(status), Some(2048 * 1024));
        assert_eq!(parse_rss("Name: x\n"), None);
    }

    #[test]
    fn established_counts_one_local_port() {
        let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 1
   1: 0100007F:1F90 0100007F:D431 01 00000000:00000000 00:00000000 00000000  1000        0 2
   2: 0100007F:D431 0100007F:1F90 01 00000000:00000000 00:00000000 00000000  1000        0 3
   3: 0100007F:1F90 0100007F:D432 06 00000000:00000000 00:00000000 00000000  1000        0 4";
        // 0x1F90 is 8080: the listener (0A) and the TIME_WAIT (06) do not
        // count, nor the client side of the loopback connection.
        assert_eq!(parse_established(table, 8080), 1);
        assert_eq!(parse_established(table, 54321), 1);
    }

    #[test]
    fn vm_rows_add_every_protocol() {
        let metrics = "# HELP x\nvm_rows_inserted_total{type=\"promremotewrite\"} 1500\n\
                       vm_rows_inserted_total{type=\"influx\"} 0\nvm_rows_added_to_storage_total 9\n";
        assert_eq!(parse_vm_rows(metrics), 1500.0);
    }

    #[test]
    fn spreads_pick_nearest_ranks() {
        let spread = Spread::of((1..=100).map(f64::from).collect());
        assert_eq!(spread.count, 100);
        assert_eq!(spread.p50, 51.0);
        assert_eq!(spread.p95, 95.0);
        assert_eq!(spread.max, 100.0);
        assert_eq!(Spread::of(Vec::new()), Spread::default());
    }
}
