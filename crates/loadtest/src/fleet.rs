//! Simulated agents, all in this process, each with its own identity on
//! disk and its own mTLS connection. They run the agent's own code for
//! everything that talks to the server: enrollment, the stream with its
//! reconnect backoff, the upload buffer, the command dispatcher with its
//! signature checks and journal, and `CertificateService::Renew`. Only the
//! host is simulated: sensors produce [`Host`] samples, and command
//! handlers answer after a short delay without running anything.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use monitorflow_agent::backoff::Backoff;
use monitorflow_agent::commands::{
    self, Dispatcher, DispatcherSettings, Handler, Handlers, Output,
};
use monitorflow_agent::identity::AgentIdentity;
use monitorflow_agent::inventory::Snapshot;
use monitorflow_agent::os::{HostInfo, OsFamily};
use monitorflow_agent::sensor_config::{self, SensorsEnd};
use monitorflow_agent::shutdown::{self, Shutdown, Trigger};
use monitorflow_agent::stream::{Agent, StreamSettings};
use monitorflow_agent::upload::{Uplink, UploadSettings};
use monitorflow_common::client::enroll::{self, EnrollSettings};
use monitorflow_common::client::renew::{Renewal, RenewalSettings};
use monitorflow_common::command;
use monitorflow_common::config::{AgentConfig, TlsPaths};
use monitorflow_common::inventory::Items;
use monitorflow_common::proto::v1::{Package, PackageReport, SensorConfigAck};
use monitorflow_common::redact::Secret;
use monitorflow_common::tls;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::{interval_at, Instant, MissedTickBehavior};

use crate::samples::Host;

/// Packages in each simulated package report, a small Debian install.
const PACKAGES: usize = 400;
/// One package in this many has an update waiting.
const UPGRADABLE_EVERY: usize = 20;

/// One simulated agent.
#[derive(Debug, Clone)]
pub struct Member {
    pub id: String,
    /// The server, or the relay it connects through.
    pub upstream_url: String,
    pub enroll_url: String,
    pub data_dir: PathBuf,
    pub token: String,
}

/// What the scenarios read and steer, shared by every agent.
pub struct Shared {
    start: Instant,
    /// While set, every host's CPU is above the default threshold.
    pub breach: AtomicBool,
    pub enrolled: AtomicU64,
    pub enroll_failed: AtomicU64,
    pub opens: AtomicU64,
    pub commands: AtomicU64,
    /// Per agent: milliseconds since start of the latest accepted Hello.
    opened_at: Vec<AtomicU64>,
    /// Per agent: milliseconds since start of the first batch sent after
    /// the latest Hello.
    first_batch_at: Vec<AtomicU64>,
    live: Vec<OnceLock<Live>>,
    /// Per agent: sensors of the configuration it runs now.
    running: Vec<AtomicU64>,
}

/// An agent past enrollment.
struct Live {
    uplink: Uplink,
    identity: watch::Sender<AgentIdentity>,
    paths: TlsPaths,
    upstream_url: String,
}

impl Shared {
    fn new(count: usize) -> Self {
        Self {
            start: Instant::now(),
            breach: AtomicBool::new(false),
            enrolled: AtomicU64::new(0),
            enroll_failed: AtomicU64::new(0),
            opens: AtomicU64::new(0),
            commands: AtomicU64::new(0),
            opened_at: (0..count).map(|_| AtomicU64::new(0)).collect(),
            first_batch_at: (0..count).map(|_| AtomicU64::new(0)).collect(),
            live: (0..count).map(|_| OnceLock::new()).collect(),
            running: (0..count).map(|_| AtomicU64::new(0)).collect(),
        }
    }

    /// Milliseconds since the fleet started; never 0, which means never.
    pub fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64 + 1
    }

    /// Per agent, the time its stream opened after `since_ms` (`None` when
    /// it did not yet) and the time its first batch went after that.
    pub fn since(&self, since_ms: u64) -> Vec<(Option<u64>, Option<u64>)> {
        self.opened_at
            .iter()
            .zip(&self.first_batch_at)
            .map(|(opened, batch)| {
                let opened = opened.load(Ordering::Relaxed);
                let batch = batch.load(Ordering::Relaxed);
                let opened = (opened > since_ms).then(|| opened - since_ms);
                let batch = (opened.is_some() && batch > since_ms).then(|| batch - since_ms);
                (opened, batch)
            })
            .collect()
    }

    /// Agents whose stream opened at least once.
    pub fn connected_once(&self) -> usize {
        self.opened_at
            .iter()
            .filter(|at| at.load(Ordering::Relaxed) > 0)
            .count()
    }

    /// Sensors the agents run now, summed, and the fewest one agent runs.
    pub fn sensors_running(&self) -> (u64, u64) {
        let counts = self.running.iter().map(|n| n.load(Ordering::Relaxed));
        let min = counts.clone().min().unwrap_or(0);
        (counts.sum(), min)
    }

    /// Samples queued and sent by every agent so far.
    pub fn samples(&self) -> (u64, u64) {
        self.live
            .iter()
            .filter_map(OnceLock::get)
            .map(|live| live.uplink.stats())
            .fold((0, 0), |(q, s), stats| (q + stats.queued, s + stats.sent))
    }
}

/// The running fleet.
pub struct Fleet {
    pub shared: Arc<Shared>,
    members: Vec<Member>,
    stop: Trigger,
    stopped: Shutdown,
    tasks: JoinSet<()>,
}

impl Fleet {
    /// Starts `members`, `per_second` new agents a second, like a fleet
    /// being installed. Returns at once; agents enroll in the background.
    pub fn start(members: Vec<Member>, per_second: u32, command_delay: Duration) -> Self {
        let shared = Arc::new(Shared::new(members.len()));
        let (stop, stopped) = shutdown::channel();
        let mut tasks = JoinSet::new();
        let pace = Duration::from_secs(1) / per_second.max(1);
        for (index, member) in members.iter().cloned().enumerate() {
            let (shared, stopped) = (shared.clone(), stopped.clone());
            let delay = pace * index as u32;
            tasks.spawn(async move {
                let mut wait = stopped.clone();
                tokio::select! {
                    () = tokio::time::sleep(delay) => {}
                    () = wait.wait() => return,
                }
                run_agent(index, member, shared, command_delay, stopped).await;
            });
        }
        tasks.spawn(watch_batches(shared.clone(), stopped.clone()));
        Self {
            shared,
            members,
            stop,
            stopped,
            tasks,
        }
    }

    pub fn len(&self) -> usize {
        self.members.len()
    }

    /// Renews every enrolled agent's certificate over `Renew`, at most
    /// `concurrency` at a time; each agent reconnects with its new
    /// certificate. Returns the successes and the failures.
    pub async fn renew_all(&self, concurrency: usize) -> (u64, u64) {
        let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency.max(1)));
        let mut renewals = JoinSet::new();
        for index in 0..self.members.len() {
            let (shared, semaphore) = (self.shared.clone(), semaphore.clone());
            renewals.spawn(async move {
                let Some(live) = shared.live[index].get() else {
                    return false;
                };
                let _permit = semaphore.acquire().await;
                let current = live.identity.borrow().clone();
                let settings = RenewalSettings::for_upstream(&live.upstream_url);
                let (renewal, _) = Renewal::new(settings, live.paths.clone(), current.clone());
                match renewal.renew(&current).await {
                    Ok(renewed) => {
                        live.identity.send_replace(renewed);
                        true
                    }
                    Err(error) => {
                        tracing::debug!(agent = index, %error, "renewal failed");
                        false
                    }
                }
            });
        }
        let (mut ok, mut failed) = (0, 0);
        while let Some(result) = renewals.join_next().await {
            match result {
                Ok(true) => ok += 1,
                _ => failed += 1,
            }
        }
        (ok, failed)
    }

    pub async fn stop(mut self) {
        self.stop.fire();
        let _ = tokio::time::timeout(Duration::from_secs(10), async {
            while self.tasks.join_next().await.is_some() {}
        })
        .await;
        self.tasks.abort_all();
        drop(self.stopped);
    }
}

/// One agent's life: enroll, then keep its stream, sensors and command
/// dispatcher running until the fleet stops.
async fn run_agent(
    index: usize,
    member: Member,
    shared: Arc<Shared>,
    command_delay: Duration,
    stop: Shutdown,
) {
    let config = AgentConfig {
        upstream_url: member.upstream_url.clone(),
        enroll_url: Some(member.enroll_url.clone()),
        data_dir: member.data_dir.clone(),
        enrollment_token: Some(Secret::new(member.token.clone())),
        ..AgentConfig::default()
    };
    let settings = EnrollSettings {
        hostname: member.id.clone(),
        ..EnrollSettings::agent(&config)
    };
    let identity = match enroll::ensure_identity(&settings, Backoff::default(), stop.clone()).await
    {
        Ok(identity) => identity,
        Err(enroll::EnrollError::Shutdown) => return,
        Err(error) => {
            shared.enroll_failed.fetch_add(1, Ordering::Relaxed);
            tracing::error!(agent = %member.id, %error, "enrollment failed");
            return;
        }
    };
    shared.enrolled.fetch_add(1, Ordering::Relaxed);
    let ca_certs = match tls::load_certs(&config.tls_paths().ca_cert) {
        Ok(certs) => certs,
        Err(error) => {
            tracing::error!(agent = %member.id, %error, "cannot read the CA certificate");
            return;
        }
    };

    let uplink = Uplink::new(UploadSettings::default());
    let (identity_tx, identity_rx) = watch::channel(identity.clone());
    let live = Live {
        uplink: uplink.clone(),
        identity: identity_tx,
        paths: config.tls_paths(),
        upstream_url: member.upstream_url.clone(),
    };
    let _ = shared.live[index].set(live);

    let (stream_end, sensors_end) = sensor_config::link();
    tokio::spawn(sensors(
        index,
        sensors_end,
        uplink.clone(),
        shared.clone(),
        stop.clone(),
    ));
    let mut handlers = Handlers::default();
    let simulated = Arc::new(Simulated {
        delay: command_delay,
        shared: shared.clone(),
    });
    for kind in [
        command::APPLY_UPDATES,
        command::CONFIGURE_SOURCES,
        command::REVERT_SOURCES,
        command::SHELL,
        command::SELF_UPDATE,
    ] {
        handlers.register(kind, simulated.clone());
    }
    let command_kinds = handlers.report();
    let (commands_stream, commands_end) = commands::link();
    let dispatcher = Dispatcher::new(
        identity.peer_id(),
        ca_certs,
        handlers,
        commands::Journal::in_dir(&member.data_dir),
        DispatcherSettings::default(),
    );
    tokio::spawn(dispatcher.run(commands_end, stop.clone()));
    let (reports_tx, reports) = watch::channel(None);
    reports_tx.send_replace(Some(package_report(index)));
    // The inventory (ADR-0187): the whole one on its first connection,
    // its hash on the next ones.
    let (inventory_tx, inventory) = watch::channel(Some(Arc::new(inventory(index))));

    let host = HostInfo {
        hostname: member.id.clone(),
        os_family: if index % 4 == 3 {
            OsFamily::Rhel
        } else {
            OsFamily::Debian
        },
        os_version: if index % 4 == 3 {
            "rocky 9.4"
        } else {
            "debian 12"
        }
        .into(),
        package_manager: true,
        hardware: Default::default(),
    };
    let opened = shared.clone();
    let agent = Agent::new(StreamSettings::from_config(&config), identity, host)
        .on_open(Arc::new(move || {
            opened.opens.fetch_add(1, Ordering::Relaxed);
            opened.opened_at[index].store(opened.now_ms(), Ordering::Relaxed);
        }))
        .follow(identity_rx)
        .with_sensors(stream_end)
        .with_upload(uplink)
        .with_commands(commands_stream)
        .with_command_kinds(command_kinds)
        .with_reports(reports)
        .with_inventory(inventory);
    if let Err(error) = agent.run(stop).await {
        tracing::error!(agent = %member.id, %error, "agent stopped");
    }
    // Keeps the report publisher, so the stream never sees it closed.
    drop(reports_tx);
    drop(inventory_tx);
}

/// Answers every command after `delay`, successfully, without running it.
struct Simulated {
    delay: Duration,
    shared: Arc<Shared>,
}

#[async_trait::async_trait]
impl Handler for Simulated {
    async fn run(&self, _payload: &[u8]) -> Output {
        tokio::time::sleep(self.delay).await;
        self.shared.commands.fetch_add(1, Ordering::Relaxed);
        Output {
            stdout: b"simulated by monitorflow-loadtest\n".to_vec(),
            ..Output::default()
        }
    }
}

/// Follows the sensor configuration from the server like the agent's
/// sensors task: one timer per sensor, first tick spread over its
/// interval, an acknowledgement per revision.
async fn sensors(
    index: usize,
    mut end: SensorsEnd,
    uplink: Uplink,
    shared: Arc<Shared>,
    mut stop: Shutdown,
) {
    let host = Arc::new(Host::nth(index));
    let mut running = JoinSet::new();
    loop {
        tokio::select! {
            changed = end.configs.changed() => if changed.is_err() { return },
            () = stop.wait() => return,
        }
        let Some(config) = end.configs.borrow_and_update().clone() else {
            continue;
        };
        running.abort_all();
        shared.running[index].store(config.sensors.len() as u64, Ordering::Relaxed);
        for (n, spec) in config.sensors.into_iter().enumerate() {
            let every = Duration::from_millis(spec.interval_ms.max(1_000));
            let offset = every.mul_f64(spread(index * 31 + n));
            let (host, uplink, shared) = (host.clone(), uplink.clone(), shared.clone());
            running.spawn(async move {
                let mut ticks = interval_at(Instant::now() + offset, every);
                ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
                loop {
                    ticks.tick().await;
                    let breach = shared.breach.load(Ordering::Relaxed);
                    for sample in host.collect(&spec, unix_ms(), breach) {
                        uplink.push(sample);
                    }
                }
            });
        }
        let ack = SensorConfigAck {
            revision: config.revision,
            errors: Vec::new(),
        };
        if end.acks.send(ack).await.is_err() {
            return;
        }
    }
}

/// Records when each agent's first batch after a reconnection went up.
async fn watch_batches(shared: Arc<Shared>, mut stop: Shutdown) {
    let mut sent = vec![0u64; shared.live.len()];
    let mut ticks = tokio::time::interval(Duration::from_millis(200));
    loop {
        tokio::select! {
            _ = ticks.tick() => {}
            () = stop.wait() => return,
        }
        let now = shared.now_ms();
        for (index, live) in shared.live.iter().enumerate() {
            let Some(live) = live.get() else { continue };
            let total = live.uplink.stats().sent;
            if total > sent[index] {
                sent[index] = total;
                let opened = shared.opened_at[index].load(Ordering::Relaxed);
                if shared.first_batch_at[index].load(Ordering::Relaxed) < opened {
                    shared.first_batch_at[index].store(now, Ordering::Relaxed);
                }
            }
        }
    }
}

/// A value in [0, 1) from `n`, the same on every run.
fn spread(n: usize) -> f64 {
    let mut x = n as u64 ^ 0x9e37_79b9_7f4a_7c15;
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    (x ^ (x >> 31)) as f64 / (u64::MAX as f64 + 1.0)
}

/// The package report agent `index` sends when it connects.
fn package_report(index: usize) -> PackageReport {
    let packages = (0..PACKAGES)
        .map(|n| Package {
            name: format!("package-{n:03}"),
            version: format!("1.{}.{}-1", n % 7, index % 3),
            arch: "amd64".into(),
            upgradable_version: if n % UPGRADABLE_EVERY == 0 {
                format!("1.{}.{}-2", n % 7, index % 3)
            } else {
                String::new()
            },
            ..Package::default()
        })
        .collect();
    PackageReport {
        packages,
        reboot_required: false,
        collected_at_unix_ms: unix_ms(),
        windows_update_source: None,
        storage: None,
        third_party: Vec::new(),
        managers: Vec::new(),
    }
}

/// The inventory agent `index` has: a system, two disks, a network card
/// and 120 services, about the size of a small Debian server's.
fn inventory(index: usize) -> Snapshot {
    let mut items = Items::new();
    items.push(
        "system",
        "system",
        [
            ("manufacturer", "QEMU".to_owned()),
            ("model", "Standard PC (Q35 + ICH9, 2009)".to_owned()),
            ("serial", format!("LOADTEST-{index:06}")),
            ("bios_version", "1.16.3".to_owned()),
            ("os", "Debian GNU/Linux 12 (bookworm)".to_owned()),
            ("memory_bytes", (4u64 << 30).to_string()),
        ],
    );
    for disk in ["sda", "sdb"] {
        items.push(
            "disk",
            disk,
            [
                ("model", "QEMU HARDDISK".to_owned()),
                ("size_bytes", (32u64 << 30).to_string()),
            ],
        );
    }
    items.push(
        "nic",
        "ens18",
        [(
            "mac",
            format!("bc:24:11:00:{:02x}:{:02x}", index / 256 % 256, index % 256),
        )],
    );
    for n in 0..120 {
        items.push(
            "service",
            format!("service-{n:03}.service"),
            [("start", "enabled".to_owned())],
        );
    }
    Snapshot::new(items.finish(), unix_ms())
}

fn unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spread_is_stable_and_in_range() {
        let values: Vec<f64> = (0..1000).map(spread).collect();
        assert!(values.iter().all(|v| (0.0..1.0).contains(v)));
        assert_eq!(spread(42), spread(42));
        // Spread out: every tenth of the range gets some.
        for tenth in 0..10 {
            let low = f64::from(tenth) / 10.0;
            assert!(
                values.iter().any(|v| (low..low + 0.1).contains(v)),
                "{tenth}"
            );
        }
    }

    #[test]
    fn package_reports_look_like_a_small_install() {
        let report = package_report(7);
        assert_eq!(report.packages.len(), PACKAGES);
        let upgradable = report
            .packages
            .iter()
            .filter(|p| !p.upgradable_version.is_empty())
            .count();
        assert_eq!(upgradable, PACKAGES / UPGRADABLE_EVERY);
    }

    #[test]
    fn inventories_differ_by_agent() {
        let (a, b) = (inventory(1), inventory(2));
        assert_eq!(a.inventory.items.len(), 124);
        assert_ne!(a.hash, b.hash);
    }

    #[test]
    fn times_are_relative_to_a_mark() {
        let shared = Shared::new(3);
        shared.opened_at[0].store(100, Ordering::Relaxed);
        shared.first_batch_at[0].store(150, Ordering::Relaxed);
        shared.opened_at[1].store(40, Ordering::Relaxed);
        assert_eq!(
            shared.since(50),
            [(Some(50), Some(100)), (None, None), (None, None)]
        );
        assert_eq!(shared.connected_once(), 2);
    }

    #[test]
    fn running_sensors_add_up_over_the_fleet() {
        let shared = Shared::new(3);
        shared.running[0].store(50, Ordering::Relaxed);
        shared.running[1].store(48, Ordering::Relaxed);
        assert_eq!(shared.sensors_running(), (98, 0));
        shared.running[2].store(50, Ordering::Relaxed);
        assert_eq!(shared.sensors_running(), (148, 48));
    }
}
