//! `monitorflow-loadtest`: runs the real server (and relays) against a
//! fleet of simulated agents in this process, through a fixed sequence of
//! phases, and writes what it measured as Markdown. `scripts/load-test.sh`
//! starts the stores and runs it; see `docs/load-test.md`.
//!
//! Phases: enrollment and first connection, steady state, a command to
//! every server, a certificate renewal of every agent, a server restart
//! (reconnection storm) and an alert storm through notifications.

mod api;
mod fleet;
mod hooks;
mod measure;
mod report;
mod samples;
mod stack;

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use clap::Parser;
use serde_json::json;
use sqlx::PgPool;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

use crate::api::Api;
use crate::fleet::{Fleet, Member};
use crate::measure::{Snapshot, Spread, Watched};
use crate::report::Phase;
use crate::stack::{Relay, Server};

const ADMIN: &str = "loadtest";
/// The threshold the alert storm crosses, set on every site of the run.
const STORM_METRIC: &str = "monitorflow_cpu_usage_ratio";

#[derive(Parser, Debug)]
#[command(
    name = "monitorflow-loadtest",
    about = "Load test of a MonitorFlow server"
)]
struct Cli {
    /// Simulated agents.
    #[arg(long, default_value_t = 100)]
    agents: usize,
    /// Share of the agents that connect through relays.
    #[arg(long, default_value_t = 0.2)]
    relay_share: f64,
    /// Relays, each with its own site.
    #[arg(long, default_value_t = 1)]
    relays: usize,
    /// New agents started per second.
    #[arg(long, default_value_t = 50)]
    ramp_rate: u32,
    /// Seconds of steady state.
    #[arg(long, default_value_t = 300)]
    steady: u64,
    /// Seconds a simulated command takes.
    #[arg(long, default_value_t = 2)]
    command_seconds: u64,
    /// Phases to run after the steady state, in this order.
    #[arg(
        long,
        value_delimiter = ',',
        default_value = "commands,renewal,restart,alerts"
    )]
    phases: Vec<String>,
    /// PostgreSQL database the server uses; created if missing. Its server
    /// needs `pg_stat_statements` in `shared_preload_libraries`.
    #[arg(long, env = "LOADTEST_DATABASE_URL")]
    database_url: String,
    #[arg(long, env = "LOADTEST_VICTORIAMETRICS_URL")]
    victoriametrics_url: String,
    #[arg(long, env = "LOADTEST_VICTORIALOGS_URL")]
    victorialogs_url: String,
    /// The stores' certificate and password from `monitorflow-server
    /// storage-certs` (ADR-0087); the store URLs are then https.
    #[arg(long, env = "LOADTEST_STORAGE_TLS_DIR")]
    storage_tls_dir: Option<PathBuf>,
    /// `monitorflow-server` and `monitorflow-relay`; next to this binary by
    /// default.
    #[arg(long)]
    server_bin: Option<PathBuf>,
    #[arg(long)]
    relay_bin: Option<PathBuf>,
    /// Emptied, then holds configurations, identities and logs.
    #[arg(long, default_value = "target/loadtest")]
    work_dir: PathBuf,
    /// The report; `<work_dir>/report.md` by default.
    #[arg(long)]
    report: Option<PathBuf>,
    /// Fail when an agent did not connect, or a phase missed its goal.
    #[arg(long)]
    strict: bool,
    /// Seconds to keep everything running after the last phase, to look
    /// at the web interface with the whole fleet; the API's address is
    /// printed, the administrator's password is in
    /// `<work_dir>/admin-password`.
    #[arg(long, default_value_t = 0)]
    hold: u64,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    monitorflow_common::logging::init_cli();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(cli))
}

/// Everything a phase needs.
struct Run {
    cli: Cli,
    server: Server,
    relays: Vec<Relay>,
    api: Api,
    pool: PgPool,
    fleet: Fleet,
    sites: Vec<String>,
    watched: watch::Sender<Watched>,
    started: Instant,
    phases: Vec<Phase>,
    /// Goals a phase missed.
    misses: Vec<String>,
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    let sibling = |name: &str| -> anyhow::Result<PathBuf> {
        Ok(std::env::current_exe()?.with_file_name(name))
    };
    let server_bin = cli
        .server_bin
        .clone()
        .map_or_else(|| sibling("monitorflow-server"), Ok)?;
    let relay_bin = cli
        .relay_bin
        .clone()
        .map_or_else(|| sibling("monitorflow-relay"), Ok)?;
    let work = std::path::absolute(&cli.work_dir)?;
    empty_work_dir(&work)?;
    std::fs::create_dir_all(work.join("agents"))?;

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&cli.database_url)
        .await
        .context("connecting to the database")?;
    sqlx::query("CREATE EXTENSION IF NOT EXISTS pg_stat_statements")
        .execute(&pool)
        .await
        .context("pg_stat_statements; start PostgreSQL with it in shared_preload_libraries")?;

    let mut server = Server::prepare(
        &server_bin,
        &work,
        &cli.database_url,
        &cli.victoriametrics_url,
        &cli.victorialogs_url,
        cli.storage_tls_dir.as_deref(),
    )?;
    server.start().await?;
    let victoriametrics = match &cli.storage_tls_dir {
        Some(dir) => {
            let ca = std::fs::read(work.join("server/ca/ca-cert.pem"))?;
            let password = std::fs::read_to_string(dir.join("password"))?;
            measure::Store::tls(&cli.victoriametrics_url, &ca, &password)?
        }
        None => measure::Store::plain(&cli.victoriametrics_url),
    };
    let password = format!("loadtest-{}", std::process::id());
    server.add_admin(ADMIN, &password).await?;
    let api = Api::login(&server.ports.api_url(), ADMIN, &password).await?;
    let customer = api.default_customer().await?;

    // Sites: one for direct agents, one per relay.
    let direct = api.create_site(&customer, "loadtest-direct").await?;
    let mut sites = vec![direct.clone()];
    let mut relays = Vec::new();
    for n in 0..cli.relays {
        let id = format!("loadtest-relay-{n}");
        let name = format!("loadtest-site-{n}");
        let site = api.create_site(&customer, &name).await?;
        let token = server.relay_token(&id, &name).await?;
        relays.push(Relay::start(&relay_bin, &work, &id, &token, &server.ports).await?);
        sites.push(site);
    }

    let behind_relays = if relays.is_empty() {
        0
    } else {
        (cli.agents as f64 * cli.relay_share).round() as usize
    };
    tracing::info!(agents = cli.agents, behind_relays, "creating tokens");
    let (mut members, mut member_sites) = (Vec::with_capacity(cli.agents), Vec::new());
    for index in 0..cli.agents {
        let id = format!("lt-{index:05}");
        // Every n-th agent goes through a relay, spread over the relays.
        let relay = (index < behind_relays).then(|| index % relays.len());
        let (site, upstream) = match relay {
            Some(r) => (&sites[r + 1], relays[r].url()),
            None => (&direct, server.ports.grpc_url()),
        };
        member_sites.push(site.clone());
        members.push(Member {
            token: String::new(),
            data_dir: work.join("agents").join(&id),
            id,
            upstream_url: upstream,
            enroll_url: server.ports.enroll_url(),
        });
    }
    create_tokens(&api, &mut members, &member_sites).await?;

    let (watched, watching) = watch::channel(Watched {
        server: server.pid(),
        relays: relays.iter().filter_map(|r| r.process.pid()).collect(),
    });
    let (snapshots_tx, mut snapshots_rx) = mpsc::unbounded_channel();
    let sampler = tokio::spawn(measure::sample(
        Duration::from_secs(5),
        watching,
        pool.clone(),
        victoriametrics,
        server.ports.grpc.port(),
        snapshots_tx,
    ));
    let started = Instant::now();
    measure::reset_statements(&pool).await?;
    let fleet = Fleet::start(
        members,
        cli.ramp_rate,
        Duration::from_secs(cli.command_seconds),
    );
    let mut run = Run {
        cli,
        server,
        relays,
        api,
        pool,
        fleet,
        sites,
        watched,
        started,
        phases: Vec::new(),
        misses: Vec::new(),
    };

    let outcome = run.phases().await;
    if let Err(error) = &outcome {
        tracing::error!(%error, "run stopped early");
    }
    if run.cli.hold > 0 {
        // The password goes to a file only the user reads, never the output.
        let file = work.join("admin-password");
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut out = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&file)?;
            writeln!(out, "{password}")?;
        }
        eprintln!(
            "holding for {} s: API at {}, user {ADMIN}, password in {}",
            run.cli.hold,
            run.server.ports.api_url(),
            file.display()
        );
        tokio::time::sleep(Duration::from_secs(run.cli.hold)).await;
    }
    let Run {
        cli,
        mut server,
        relays,
        fleet,
        watched,
        phases,
        misses,
        ..
    } = run;
    fleet.stop().await;
    for mut relay in relays {
        let _ = relay.process.stop(Duration::from_secs(10)).await;
    }
    server.stop().await?;
    drop(watched);
    let _ = sampler.await;
    let mut snapshots: Vec<Snapshot> = Vec::new();
    while let Ok(snapshot) = snapshots_rx.try_recv() {
        snapshots.push(snapshot);
    }

    let setup = vec![
        ("Agents".to_owned(), cli.agents.to_string()),
        (
            "Through relays".to_owned(),
            format!("{behind_relays} over {} relays", cli.relays),
        ),
        ("Steady state".to_owned(), format!("{} s", cli.steady)),
        ("Host".to_owned(), host_description()),
    ];
    let text = report::render(
        &format!("Load test: {} agents", cli.agents),
        &setup,
        &phases,
        &snapshots,
    );
    let report_path = cli.report.clone().unwrap_or_else(|| work.join("report.md"));
    std::fs::write(&report_path, &text)?;
    println!("{text}");
    eprintln!("report written to {}", report_path.display());
    outcome?;
    if cli.strict && !misses.is_empty() {
        bail!("missed: {}", misses.join("; "));
    }
    Ok(())
}

/// Single-use tokens through the API, 16 at a time, one per member, for
/// the site of the same index.
async fn create_tokens(api: &Api, members: &mut [Member], sites: &[String]) -> anyhow::Result<()> {
    let requests = members
        .iter()
        .zip(sites)
        .map(|(member, site)| {
            let body =
                json!({"role": "agent", "peer_id": member.id, "site_id": site, "ttl_s": 86_400});
            ("/api/tokens".to_owned(), body)
        })
        .collect();
    for (member, created) in members.iter_mut().zip(post_all(api, requests).await?) {
        member.token = created["token"]
            .as_str()
            .context("token missing")?
            .to_owned();
    }
    Ok(())
}

/// POSTs every request, 16 at a time; the responses in order.
async fn post_all(
    api: &Api,
    requests: Vec<(String, serde_json::Value)>,
) -> anyhow::Result<Vec<serde_json::Value>> {
    let semaphore = Arc::new(tokio::sync::Semaphore::new(16));
    let mut set = tokio::task::JoinSet::new();
    for (index, (path, body)) in requests.into_iter().enumerate() {
        let (api, semaphore) = (api.clone(), semaphore.clone());
        set.spawn(async move {
            let _permit = semaphore.acquire().await;
            (index, api.post(&path, &body).await)
        });
    }
    let mut responses = Vec::new();
    while let Some(joined) = set.join_next().await {
        let (index, response) = joined?;
        responses.push((index, response?));
    }
    responses.sort_by_key(|(index, _)| *index);
    Ok(responses.into_iter().map(|(_, r)| r).collect())
}

/// Empties `work`, except the server's data and the stores' certificate,
/// which `scripts/load-test.sh` issued before the stores started.
fn empty_work_dir(work: &std::path::Path) -> anyhow::Result<()> {
    let Ok(entries) = std::fs::read_dir(work) else {
        return Ok(());
    };
    for entry in entries {
        let path = entry?.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if matches!(name, "server" | "storage-tls" | "storage-certs.toml") {
            continue;
        }
        if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        }
        .with_context(|| format!("emptying {}", work.display()))?;
    }
    Ok(())
}

/// Open incidents of the storm's metric.
async fn open_incidents(pool: &PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*) FROM incidents WHERE state = 'open' AND metric = $1")
        .bind(STORM_METRIC)
        .fetch_one(pool)
        .await
}

/// CPU model, cores and memory of this host.
fn host_description() -> String {
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let model = cpuinfo
        .lines()
        .find_map(|l| l.strip_prefix("model name"))
        .and_then(|l| l.split(':').nth(1))
        .unwrap_or("unknown CPU")
        .trim()
        .to_owned();
    let cores = std::thread::available_parallelism().map_or(0, |n| n.get());
    let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let mem_gib = meminfo
        .lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))
        .and_then(|l| l.split_whitespace().next()?.parse::<f64>().ok())
        .map_or(0.0, |kib| kib / 1024.0 / 1024.0);
    format!("{model}, {cores} threads, {mem_gib:.0} GiB; server, stores and load generator on the same host")
}

impl Run {
    fn now(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    fn begin(&self, name: &str) -> Phase {
        tracing::info!(phase = name, "phase starting");
        Phase {
            name: name.to_owned(),
            from: self.now(),
            ..Phase::default()
        }
    }

    async fn end(&mut self, mut phase: Phase) -> anyhow::Result<()> {
        phase.to = self.now();
        phase.statements = measure::top_statements(&self.pool, 12).await?;
        measure::reset_statements(&self.pool).await?;
        tracing::info!(phase = %phase.name, "phase done");
        self.phases.push(phase);
        Ok(())
    }

    fn miss(&mut self, phase: &mut Phase, what: String) {
        tracing::warn!(phase = %phase.name, %what, "goal missed");
        phase.result("Missed", &what);
        self.misses.push(format!("{}: {what}", phase.name));
    }

    async fn phases(&mut self) -> anyhow::Result<()> {
        self.connect().await?;
        self.steady().await?;
        for name in self.cli.phases.clone() {
            match name.as_str() {
                "commands" => self.commands().await?,
                "renewal" => self.renewal().await?,
                "restart" => self.restart().await?,
                "alerts" => self.alerts().await?,
                other => bail!("unknown phase `{other}`"),
            }
        }
        Ok(())
    }

    /// Servers the API shows online.
    async fn online(&self) -> anyhow::Result<usize> {
        let servers = self.api.servers().await?;
        Ok(servers.iter().filter(|s| s["status"] == "online").count())
    }

    /// Polls `check` every second until it returns `Some`, or `limit`.
    async fn until<T, F, Fut>(&self, limit: Duration, mut check: F) -> anyhow::Result<Option<T>>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<Option<T>>>,
    {
        let deadline = Instant::now() + limit;
        loop {
            if let Some(value) = check().await? {
                return Ok(Some(value));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    /// Enrollment and first connection of the whole fleet.
    async fn connect(&mut self) -> anyhow::Result<()> {
        let mut phase = self.begin("Enrollment and first connection");
        let total = self.fleet.len();
        let start = Instant::now();
        let ramp = Duration::from_secs_f64(total as f64 / f64::from(self.cli.ramp_rate.max(1)));
        let shared = self.fleet.shared.clone();
        let connected = self
            .until(ramp + Duration::from_secs(180), || {
                let shared = shared.clone();
                async move { Ok((shared.connected_once() == total).then_some(())) }
            })
            .await?;
        let elapsed = start.elapsed().as_secs_f64();
        phase.result("Ramp", format!("{} agents a second", self.cli.ramp_rate));
        phase.result(
            "Agents connected",
            format!("{} of {total} in {elapsed:.0} s", shared.connected_once()),
        );
        phase.result(
            "Enrollment failures",
            shared.enroll_failed.load(Ordering::Relaxed),
        );
        if connected.is_none() {
            let what = format!(
                "only {} of {total} agents connected",
                shared.connected_once()
            );
            self.miss(&mut phase, what);
        }
        let api_online = self
            .until(Duration::from_secs(60), || async {
                let online = self.online().await?;
                Ok((online == total).then_some(online))
            })
            .await?;
        phase.result(
            "Online in the API",
            match api_online {
                Some(n) => format!("{n} after {:.0} s", start.elapsed().as_secs_f64()),
                None => format!(
                    "{} after {:.0} s",
                    self.online().await?,
                    start.elapsed().as_secs_f64()
                ),
            },
        );
        self.end(phase).await
    }

    /// The fleet reporting at the real intervals; web pages timed at the end.
    async fn steady(&mut self) -> anyhow::Result<()> {
        let mut phase = self.begin("Steady state");
        let (queued, sent) = self.fleet.shared.samples();
        tokio::time::sleep(Duration::from_secs(self.cli.steady)).await;
        let (queued_after, sent_after) = self.fleet.shared.samples();
        let seconds = self.cli.steady.max(1) as f64;
        phase.result(
            "Samples collected by the agents",
            format!("{:.0} a second", (queued_after - queued) as f64 / seconds),
        );
        phase.result(
            "Samples sent",
            format!("{:.0} a second", (sent_after - sent) as f64 / seconds),
        );
        self.time_pages(&mut phase).await?;
        self.end(phase).await
    }

    /// Times the API calls of the fleet overview and the incident list.
    async fn time_pages(&self, phase: &mut Phase) -> anyhow::Result<()> {
        const RUNS: usize = 20;
        let (mut fleet, mut incidents) = (Vec::new(), Vec::new());
        for _ in 0..RUNS {
            fleet.push(self.api.fleet_page().await?.as_secs_f64() * 1000.0);
            incidents.push(self.api.incident_page().await?.as_secs_f64() * 1000.0);
        }
        phase.millis("Fleet overview API calls", Spread::of(fleet));
        phase.millis("Incident list API calls", Spread::of(incidents));
        Ok(())
    }

    /// `apply_updates` to every server, answered after `command_seconds`.
    async fn commands(&mut self) -> anyhow::Result<()> {
        let mut phase = self.begin("A command to every server");
        let servers = self.api.servers().await?;
        let ids: Vec<String> = servers
            .iter()
            .filter_map(|s| s["id"].as_str().map(str::to_owned))
            .collect();
        let start = Instant::now();
        let requests = ids
            .iter()
            .map(|id| {
                let body = json!({"kind": "apply_updates", "security_only": true});
                (format!("/api/servers/{id}/commands"), body)
            })
            .collect();
        post_all(&self.api, requests).await?;
        let queued_in = start.elapsed().as_secs_f64();
        let total = ids.len() as i64;
        let pool = self.pool.clone();
        let done = self
            .until(Duration::from_secs(300), || {
                let pool = pool.clone();
                async move {
                    let finished: i64 = sqlx::query_scalar(
                        "SELECT count(*) FROM commands WHERE state IN ('succeeded', 'failed', 'expired', 'rejected', 'cancelled')",
                    )
                    .fetch_one(&pool)
                    .await?;
                    Ok((finished >= total).then_some(()))
                }
            })
            .await?;
        let elapsed = start.elapsed().as_secs_f64();
        let latencies: Vec<(f64, String)> = sqlx::query_as(
            "SELECT extract(epoch FROM finished_at - issued_at)::float8, state
             FROM commands WHERE finished_at IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await?;
        let failed = latencies.iter().filter(|(_, s)| s != "succeeded").count();
        phase.result(
            "Commands queued through the API",
            format!("{total} in {queued_in:.1} s"),
        );
        phase.seconds(
            "Queued to result recorded",
            Spread::of(latencies.into_iter().map(|(s, _)| s).collect()),
        );
        phase.result("Not succeeded", failed);
        phase.result(
            "Run by the agents",
            self.fleet.shared.commands.load(Ordering::Relaxed),
        );
        if done.is_none() {
            self.miss(
                &mut phase,
                format!("not every command finished in {elapsed:.0} s"),
            );
        } else {
            phase.result("All finished after", format!("{elapsed:.1} s"));
        }
        self.end(phase).await
    }

    /// Every agent renews its certificate, then reconnects with it.
    async fn renewal(&mut self) -> anyhow::Result<()> {
        let mut phase = self.begin("Certificate renewal of every agent");
        let mark = self.fleet.shared.now_ms();
        let since = chrono::Utc::now();
        let start = Instant::now();
        let (ok, failed) = self.fleet.renew_all(50).await;
        phase.result(
            "Renewed",
            format!(
                "{ok}, {failed} failed, in {:.1} s, 50 at a time",
                start.elapsed().as_secs_f64()
            ),
        );
        // The server renews a peer at most once an hour (ADR-0010): a run
        // shorter than that measures the refusals only.
        if ok > 0 {
            self.reconnected(&mut phase, mark, since, Duration::from_secs(120))
                .await?;
        } else {
            phase.result(
                "Note",
                "the server refuses a renewal within an hour of the last certificate (ADR-0010); this measured the refused calls",
            );
        }
        self.end(phase).await
    }

    /// Waits until the server shows every agent on a stream opened after
    /// `since` (agents behind a relay keep their stream to the relay, which
    /// opens a new one upstream), and until every agent that reopened its
    /// own stream after `mark` sent a batch on it.
    async fn reconnected(
        &mut self,
        phase: &mut Phase,
        mark: u64,
        since: chrono::DateTime<chrono::Utc>,
        limit: Duration,
    ) -> anyhow::Result<()> {
        let total = self.fleet.len();
        let connected_after = |servers: &[serde_json::Value]| -> Vec<f64> {
            servers
                .iter()
                .filter(|s| s["status"] == "online")
                .filter_map(|s| {
                    s["connected_at"]
                        .as_str()?
                        .parse::<chrono::DateTime<chrono::Utc>>()
                        .ok()
                })
                .filter(|at| *at >= since)
                .map(|at| (at - since).as_seconds_f64())
                .collect()
        };
        let all = self
            .until(limit, || async {
                let servers = self.api.servers().await?;
                Ok((connected_after(&servers).len() == total).then_some(()))
            })
            .await?;
        let servers = self.api.servers().await?;
        phase.seconds(
            "Streams open again at the server",
            Spread::of(connected_after(&servers)),
        );
        if all.is_none() {
            let missing = total - connected_after(&servers).len();
            self.miss(phase, format!("{missing} agents were not connected again"));
        }
        // Agents that reopened their own stream: until their next batch.
        let shared = self.fleet.shared.clone();
        let _ = self
            .until(Duration::from_secs(120), || {
                let shared = shared.clone();
                async move {
                    let since = shared.since(mark);
                    Ok(since
                        .iter()
                        .all(|(o, b)| o.is_none() || b.is_some())
                        .then_some(()))
                }
            })
            .await?;
        let since = shared.since(mark);
        let seconds = |ms: u64| ms as f64 / 1000.0;
        let opened: Vec<f64> = since.iter().filter_map(|(o, _)| o.map(seconds)).collect();
        let batches: Vec<f64> = since.iter().filter_map(|(_, b)| b.map(seconds)).collect();
        phase.seconds("Direct agents: stream reopened", Spread::of(opened));
        phase.seconds("Direct agents: first batch sent", Spread::of(batches));
        Ok(())
    }

    /// SIGTERM to the server, start it again, every agent reconnects.
    async fn restart(&mut self) -> anyhow::Result<()> {
        let mut phase = self.begin("Server restart (reconnection storm)");
        let stop = Instant::now();
        self.server.stop().await?;
        phase.result(
            "Stopped in",
            format!("{:.1} s", stop.elapsed().as_secs_f64()),
        );
        let mark = self.fleet.shared.now_ms();
        let since = chrono::Utc::now();
        let start = Instant::now();
        self.server.start().await?;
        self.watched.send_modify(|w| w.server = self.server.pid());
        phase.result(
            "Ready again in",
            format!("{:.1} s", start.elapsed().as_secs_f64()),
        );
        self.reconnected(&mut phase, mark, since, Duration::from_secs(300))
            .await?;
        self.end(phase).await
    }

    /// Every server crosses a CPU threshold at once; incidents open and
    /// notifications go to a webhook, then everything resolves.
    async fn alerts(&mut self) -> anyhow::Result<()> {
        let mut phase = self.begin("Alert storm");
        let hooks = hooks::Receiver::start().await?;
        self.api
            .post(
                "/api/notifications/channels",
                &json!({"kind": "webhook", "settings": {"url": hooks.url()}}),
            )
            .await?;
        for site in &self.sites {
            self.api
                .post(
                    &format!("/api/sites/{site}/thresholds"),
                    &json!({
                        "metric": STORM_METRIC,
                        "instance": "total",
                        "comparison": ">",
                        "critical": 0.9,
                        "duration_s": 60,
                    }),
                )
                .await?;
        }
        // The engine reads new thresholds within 10 s.
        tokio::time::sleep(Duration::from_secs(15)).await;
        let total = self.fleet.len() as i64;
        let pool = self.pool.clone();
        let start = Instant::now();
        self.fleet.shared.breach.store(true, Ordering::Relaxed);
        let opened = self
            .until(Duration::from_secs(400), || async {
                let open = open_incidents(&pool).await?;
                Ok((open >= total).then_some(()))
            })
            .await?;
        let open = open_incidents(&pool).await?;
        phase.result(
            "Incidents open",
            format!("{open} of {total}, {:.0} s after the breach started (sensors every 60 s, threshold 60 s)", start.elapsed().as_secs_f64()),
        );
        if opened.is_none() {
            self.miss(&mut phase, format!("{open} of {total} incidents opened"));
        }
        let delivered = self
            .until(Duration::from_secs(900), || async {
                Ok((hooks.count() as i64 >= open).then_some(()))
            })
            .await?;
        phase.result(
            "Webhook requests for the openings",
            format!(
                "{} after {:.0} s",
                hooks.count(),
                start.elapsed().as_secs_f64()
            ),
        );
        if delivered.is_none() {
            self.miss(
                &mut phase,
                format!("{} of {open} webhooks arrived", hooks.count()),
            );
        }
        let latencies: Vec<f64> = sqlx::query_scalar(
            "SELECT extract(epoch FROM sent_at - created_at)::float8 FROM notification_outbox
             WHERE event = 'incident.opened' AND sent_at IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await?;
        phase.seconds(
            "Outbox event to sent (incident.opened)",
            Spread::of(latencies),
        );

        // Back to normal: every incident resolves.
        let resolving = Instant::now();
        let before = hooks.count();
        self.fleet.shared.breach.store(false, Ordering::Relaxed);
        let resolved = self
            .until(Duration::from_secs(300), || async {
                let open = open_incidents(&pool).await?;
                Ok((open == 0).then_some(()))
            })
            .await?;
        phase.result(
            "Resolved",
            match resolved {
                Some(()) => format!("all after {:.0} s", resolving.elapsed().as_secs_f64()),
                None => "not all".to_owned(),
            },
        );
        let _ = self
            .until(Duration::from_secs(900), || async {
                Ok((hooks.count() - before >= open as usize).then_some(()))
            })
            .await?;
        phase.result(
            "Webhook requests for the resolutions",
            format!(
                "{} after {:.0} s",
                hooks.count() - before,
                resolving.elapsed().as_secs_f64()
            ),
        );
        let latencies: Vec<f64> = sqlx::query_scalar(
            "SELECT extract(epoch FROM sent_at - created_at)::float8 FROM notification_outbox
             WHERE event = 'incident.resolved' AND sent_at IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await?;
        phase.seconds(
            "Outbox event to sent (incident.resolved)",
            Spread::of(latencies),
        );
        self.time_pages(&mut phase).await?;
        self.end(phase).await
    }
}
