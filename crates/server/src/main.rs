//! MonitorFlow server entry point.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use monitorflow_common::config::{self, ServerConfig};
use monitorflow_common::logging;
use monitorflow_common::redact::Secret;
use monitorflow_common::tls::{Identity, PeerRole};
use monitorflow_server::alerting::offline::{self, OfflineSettings};
use monitorflow_server::alerting::{AlertStore, Alerts, EngineSettings, LogNotifier};
use monitorflow_server::api::server_update::ServerUpdateSettings;
use monitorflow_server::api::users::GrantScope;
use monitorflow_server::api::{
    self, customers, setup, users, ApiSettings, ApiState, EnrollmentSettings, Role,
};
use monitorflow_server::auto_update::AutoUpdate;
use monitorflow_server::ca::{CaOptions, CertificateAuthority};
use monitorflow_server::ca_rotation::{self, Rotator};
use monitorflow_server::campaign::{self, Runner};
use monitorflow_server::cluster::{self, Cluster, Links};
use monitorflow_server::commands::{CommandStore, Commands, DispatcherSettings};
use monitorflow_server::crl::{self, CrlPublisher};
use monitorflow_server::db;
use monitorflow_server::enroll::{self, server_cert, token, Enroller, FailureLimiter};
use monitorflow_server::grpc::{
    self, AgentStreamService, CrlService, DbInventory, DbRelayPolicy, Registry, StreamSettings,
};
use monitorflow_server::ingest::{Ingest, JsonLines, RemoteWrite, Writer, WriterSettings};
use monitorflow_server::mirror::{
    self, JobKind, Layout, MirrorSpec, Mirrors, ToolsRunner, Trigger,
};
use monitorflow_server::notify;
use monitorflow_server::relay_commands::RelayCommands;
use monitorflow_server::release_check;
use monitorflow_server::remediation::context::Backends;
use monitorflow_server::remediation::runner::{self, Remediation};
use monitorflow_server::remediation::snapshot::{
    self, LocalSnapshots, ProxmoxSnapshotter, SnapshotHook,
};
use monitorflow_server::remediation::switch;
use monitorflow_server::renew::Renewer;
use monitorflow_server::sensor_config::{SensorConfigStore, SensorConfigs, WatcherSettings};
use monitorflow_server::storage_tls::{self, StorageAuth};
use monitorflow_server::vm_query::{VmQuery, QUERY_TIMEOUT};
use tokio::net::TcpListener;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::watch;

#[derive(Parser, Debug)]
// The version is the build's, which the updater reads (ADR-0056).
#[command(name = "monitorflow-server", version = monitorflow_common::VERSION, about = "MonitorFlow server")]
struct Cli {
    /// Path to the TOML configuration file.
    #[arg(
        long,
        env = "MONITORFLOW_CONFIG",
        default_value = "/etc/monitorflow/server.toml",
        global = true
    )]
    config: std::path::PathBuf,
    /// Without a command, run the server.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Manage web interface users.
    User {
        #[command(subcommand)]
        command: UserCommand,
    },
    /// Manage single-use tokens.
    #[command(subcommand)]
    Token(TokenCommand),
    /// Manage customers.
    #[command(subcommand)]
    Customer(CustomerCommand),
    /// Manage package mirrors.
    #[command(subcommand)]
    Mirror(MirrorCommand),
    /// Agent releases served for self-updates and installs.
    #[command(subcommand)]
    AgentRelease(AgentReleaseCommand),
    /// Print a new one-time link that creates the first administrator in
    /// the browser, replacing the previous one. Works only while no user
    /// exists.
    SetupLink,
    /// Issue the certificate and password of VictoriaMetrics and
    /// VictoriaLogs into `storage_tls_dir` if needed, then exit. The
    /// compose file runs it before starting them (ADR-0087).
    StorageCerts,
    /// Send a query to VictoriaMetrics and VictoriaLogs as the server does,
    /// and say whether each answered.
    StorageCheck,
}

#[derive(Subcommand, Debug)]
enum AgentReleaseCommand {
    /// Copy a release from a directory of release files into the cache,
    /// once its signature verifies. Reads only that directory: works
    /// offline.
    Import {
        #[arg(long)]
        dir: std::path::PathBuf,
    },
    /// List the cached versions, newest first.
    List,
}

#[derive(Subcommand, Debug)]
enum MirrorCommand {
    /// Declare a mirror; nothing is downloaded until it syncs.
    Add(AddMirror),
    /// List mirrors with their current snapshot and last run.
    List,
    /// Sync a mirror now, in this process, and print the run's output.
    /// With auto-snapshot, the sync ends with a snapshot.
    Sync {
        #[arg(long)]
        name: String,
    },
    /// Snapshot what the last sync fetched and move `current` to it.
    Snapshot {
        #[arg(long)]
        name: String,
        /// Snapshot name; the UTC time (20261023T090000Z) by default.
        #[arg(long)]
        snapshot: Option<String>,
    },
    /// Remove the snapshots nothing needs now and free their disk space.
    /// The server does this by itself every hour.
    Prune,
    /// Delete a mirror and its packages, here and on relays. Refused while
    /// an update campaign or a server uses it.
    Delete {
        #[arg(long)]
        name: String,
    },
    /// Manage the key signing published repositories.
    #[command(subcommand)]
    Key(KeyCommand),
}

#[derive(Args, Debug)]
struct AddMirror {
    /// 1 to 64 of a-z, 0-9, `.`, `_` and `-`; part of the mirror's URLs.
    #[arg(long)]
    name: String,
    #[arg(long, value_parser = ["apt", "dnf"])]
    family: String,
    /// http or https URL of the upstream repository, without credentials.
    #[arg(long)]
    url: String,
    /// apt suite, like bookworm.
    #[arg(long, default_value = "")]
    distribution: String,
    /// apt component; repeat for several.
    #[arg(long = "component", value_name = "COMPONENT")]
    components: Vec<String>,
    /// Architecture to mirror; repeat for several. All by default.
    #[arg(long = "arch", value_name = "ARCH")]
    architectures: Vec<String>,
    /// Automatic sync interval like 6h or 1d (at least 15m), or `manual`.
    #[arg(long, default_value = "1d", value_parser = parse_interval)]
    interval: Interval,
    /// Do not publish a snapshot at the end of each sync.
    #[arg(long)]
    no_auto_snapshot: bool,
}

#[derive(Subcommand, Debug)]
enum KeyCommand {
    /// Create the signing key (RSA 4096, no passphrase).
    Generate {
        #[arg(long, default_value = mirror::signing::DEFAULT_UID)]
        uid: String,
    },
    /// Import an existing secret key from an OpenPGP file.
    Import {
        #[arg(long)]
        file: std::path::PathBuf,
    },
}

/// Seconds between automatic syncs; `None` for manual only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Interval(Option<i32>);

/// `manual`, or a number of seconds with an `s`, `m`, `h` or `d` suffix.
fn parse_interval(s: &str) -> Result<Interval, String> {
    if s == "manual" {
        return Ok(Interval(None));
    }
    let (number, unit) = s.split_at(s.len().saturating_sub(1));
    let unit = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        _ => return Err("expected `manual` or a duration like 6h or 1d".into()),
    };
    number
        .parse::<i32>()
        .ok()
        .and_then(|n| n.checked_mul(unit))
        .filter(|&secs| secs >= mirror::model::MIN_SYNC_INTERVAL_S)
        .map(|secs| Interval(Some(secs)))
        .ok_or_else(|| "expected a duration of at least 15m, like 6h or 1d".into())
}

#[derive(Subcommand, Debug)]
enum CustomerCommand {
    /// Create a customer.
    Add {
        #[arg(long)]
        name: String,
    },
    /// List customers with their number of sites.
    List,
}

/// Where a role applies; global without either flag.
#[derive(Args, Debug)]
struct GrantArgs {
    /// viewer, operator, approver or administrator (alias: admin).
    #[arg(long, value_parser = parse_role)]
    role: Role,
    /// Grant on this customer and all its sites.
    #[arg(long, conflicts_with = "site")]
    customer: Option<String>,
    /// Grant on this site only.
    #[arg(long)]
    site: Option<String>,
}

#[derive(Subcommand, Debug)]
enum TokenCommand {
    /// Create a single-use token and print it, once, on stdout.
    Create(CreateToken),
}

#[derive(Args, Debug)]
struct CreateToken {
    /// What the token installs.
    #[arg(long, value_parser = ["agent", "relay"])]
    role: String,
    /// Name of the agent or relay: 1 to 64 of [A-Za-z0-9._-].
    #[arg(long)]
    id: String,
    /// Name of the site it belongs to.
    #[arg(long)]
    site: String,
    /// Create the site if it does not exist.
    #[arg(long)]
    create_site: bool,
    /// Customer of the site; with --create-site, the customer it is
    /// created under. Defaults to the site's own customer, or `default`.
    #[arg(long)]
    customer: Option<String>,
    /// Validity, like 90s, 15m, 1h or 7d (at most 7d).
    #[arg(long, default_value = "1h", value_parser = token::parse_ttl)]
    ttl: Duration,
    /// For a relay: an address its agents connect to, a DNS name or an IP
    /// address. Repeat for several; they go in the relay's certificate.
    #[arg(long = "server-name", value_name = "NAME")]
    server_names: Vec<String>,
    /// For a relay that connects through another relay: the name of that
    /// relay, its parent.
    #[arg(long, value_name = "RELAY_ID")]
    parent_relay: Option<String>,
}

#[derive(Subcommand, Debug)]
enum UserCommand {
    /// Create a user with one role, global unless --customer or --site.
    /// The password is read from the terminal, or from the first line of
    /// stdin when it is not a terminal.
    Add {
        #[arg(long)]
        username: String,
        #[command(flatten)]
        grant: GrantArgs,
    },
    /// Grant one more role to an existing user.
    Grant {
        #[arg(long)]
        username: String,
        #[command(flatten)]
        grant: GrantArgs,
    },
}

/// Needs no database: the cache is a directory of the mirror listener.
fn agent_release_command(
    config: &ServerConfig,
    command: AgentReleaseCommand,
) -> anyhow::Result<()> {
    use monitorflow_server::agent_releases::{self, ReleaseCache};

    let cache = ReleaseCache::in_mirror_dir(&config.mirror_dir());
    match command {
        AgentReleaseCommand::Import { dir } => {
            let keys = monitorflow_common::release::trusted_keys()?;
            let imported = cache.import(&dir, keys)?;
            println!(
                "imported {} into {}",
                imported.version,
                cache.dir().display()
            );
            for name in &imported.missing {
                println!("note: {name} was not in {}", dir.display());
            }
            if imported.missing.contains(&agent_releases::INSTALLER) {
                println!(
                    "note: without install.sh, Linux agents cannot be installed from this server"
                );
            }
        }
        AgentReleaseCommand::List => {
            let versions = cache.versions()?;
            if versions.is_empty() {
                eprintln!(
                    "no agent release is cached in {}; import one with \
                     `monitorflow-server agent-release import --dir DIR`",
                    cache.dir().display()
                );
            }
            for version in versions {
                println!("{version}");
            }
        }
    }
    Ok(())
}

fn parse_role(s: &str) -> Result<Role, String> {
    match s {
        "admin" => Ok(Role::Administrator),
        other => other.parse(),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config: ServerConfig = config::load(&cli.config)?;
    if cli.command.is_some() {
        logging::init_cli();
    }
    match cli.command {
        Some(Command::Token(TokenCommand::Create(args))) => {
            return create_token(&config, args).await
        }
        Some(Command::AgentRelease(command)) => return agent_release_command(&config, command),
        Some(Command::StorageCerts) => return storage_certs_command(&config).await,
        Some(Command::StorageCheck) => return storage_check_command(&config).await,
        Some(
            Command::User { .. } | Command::Customer(_) | Command::Mirror(_) | Command::SetupLink,
        ) => {}
        None => {
            logging::init(&config.log);
            tracing::info!(config = %cli.config.display(), protocol = monitorflow_common::PROTOCOL_VERSION, "starting server");
            // Secrets are redacted by their Debug impls.
            tracing::info!(effective = ?config, "configuration loaded");
        }
    }

    let pool = db::connect(&config.database_url).await?;
    db::migrate(&pool).await?;

    match cli.command {
        Some(Command::User { command }) => return user_command(&pool, command).await,
        Some(Command::Customer(command)) => return customer_command(&pool, command).await,
        Some(Command::Mirror(command)) => return mirror_command(&config, pool, command).await,
        Some(Command::SetupLink) => return setup_link_command(&config, &pool).await,
        Some(
            Command::Token(_)
            | Command::AgentRelease(_)
            | Command::StorageCerts
            | Command::StorageCheck,
        )
        | None => {}
    }
    announce_setup(&config, &pool).await?;

    let ca = Arc::new(open_ca(&config).await?);
    let names = server_cert::parse_names(&config.server_names);
    let tls_paths = config.tls_paths();
    let (issuer, paths, cert_names) = (ca.clone(), tls_paths.clone(), names.clone());
    tokio::task::spawn_blocking(move || server_cert::ensure(&issuer, &paths, &cert_names))
        .await?
        .context("preparing the server certificate")?;
    let identity = Identity::load(&tls_paths).context("loading the TLS identity")?;

    // Every new mTLS connection uses the latest config from the publisher,
    // which carries a fresh CRL.
    let (crl, tls_config) = CrlPublisher::start(ca.clone(), identity).await?;
    let crl = Arc::new(crl);
    // Changes of certificate authority (ADR-0088).
    let rotator = Rotator::new(pool.clone(), ca.clone(), crl.clone(), tls_paths, names);
    let listener = TcpListener::bind(config.grpc_listen_addr)
        .await
        .with_context(|| format!("binding gRPC listener on {}", config.grpc_listen_addr))?;
    let enroll_listener = TcpListener::bind(config.enroll_listen_addr)
        .await
        .with_context(|| format!("binding enroll listener on {}", config.enroll_listen_addr))?;
    let api_listener = TcpListener::bind(config.api_listen_addr)
        .await
        .with_context(|| format!("binding API listener on {}", config.api_listen_addr))?;
    let mirror_listener = TcpListener::bind(config.mirror_listen_addr)
        .await
        .with_context(|| format!("binding mirror listener on {}", config.mirror_listen_addr))?;
    let mirrors = open_mirrors(&config, pool.clone()).await?;
    // TLS and a password on the links to the stores (ADR-0087).
    let storage = match &config.storage_tls_dir {
        Some(dir) => Some(
            storage_tls::start(ca.clone(), dir.clone(), &storage_urls(&config))
                .await
                .context("preparing TLS to VictoriaMetrics and VictoriaLogs")?,
        ),
        None => None,
    };
    // The stores' certificate moves with the CA too.
    let rotator = match &config.storage_tls_dir {
        Some(dir) => rotator.with_storage(dir.clone(), storage_tls::names(&storage_urls(&config))?),
        None => rotator,
    };
    // Storage writers (ADR-0013). Streams only push to their queues.
    let writer_settings = WriterSettings::from_config(&config.ingest);
    let metrics = Writer::spawn(
        RemoteWrite::with_auth(&config.victoriametrics_url, storage.as_ref())
            .context("VictoriaMetrics client")?,
        writer_settings.clone(),
    );
    let events = Writer::spawn(
        JsonLines::with_auth(&config.victorialogs_url, storage.as_ref())
            .context("VictoriaLogs client")?,
        writer_settings,
    );
    // Threshold engine (ADR-0017): open incidents and pending timers are
    // reloaded before any stream can deliver samples.
    let alerts = Alerts::start(
        AlertStore::new(pool.clone()),
        EngineSettings::default(),
        Arc::new(LogNotifier),
    )
    .await
    .context("loading the threshold engine state")?;
    let registry = Registry::default();
    let sensor_configs = SensorConfigs::spawn(
        SensorConfigStore::new(pool.clone()),
        registry.clone(),
        WatcherSettings::default(),
    );
    let commands = Commands::spawn(
        CommandStore::new(pool.clone()),
        registry.clone(),
        ca.clone(),
        DispatcherSettings::default(),
    );
    // Deletes local snapshots past their time (ADR-0094).
    tokio::spawn(monitorflow_server::remediation::snapshot::local::run_retention(commands.clone()));
    // Relays' own self-updates (ADR-0055).
    let relay_commands = RelayCommands::spawn(pool.clone(), registry.clone(), ca.clone());
    // One of maybe several instances on this database (ADR-0089).
    let cluster = Cluster::join(
        pool.clone(),
        registry.clone(),
        Links {
            commands: Some(commands.clone()),
            relay_commands: Some(relay_commands.clone()),
            crl: Some(crl.clone()),
        },
        cluster::Settings::default(),
    )
    .await
    .context("joining the server instances")?;
    let offline_pool = pool.clone();
    let offline_registry = registry.clone();
    // Settings of the web interface that apply without a restart (ADR-0069).
    let api_settings = ApiSettings::from_config(&config);
    api_settings
        .live
        .refresh(&pool)
        .await
        .context("reading the server settings")?;
    api_settings.live.clone().spawn(pool.clone());
    // Update campaigns (ADR-0033) queue through the same dispatcher, and
    // pin snapshots with the mirrors' key and public URL (ADR-0038).
    let campaigns = Runner::new(pool.clone())
        .with_commands(commands.clone())
        .with_sources(api_settings.live.clone(), mirrors.clone())
        .with_agent_releases(api_settings.agent_releases.clone())
        .with_proxmox(config.proxmox.enabled);
    let service = AgentStreamService::new(
        registry.clone(),
        Arc::new(DbRelayPolicy::new(pool.clone())),
        StreamSettings::from_config(&config),
    )
    .with_inventory(Arc::new(DbInventory::new(pool.clone())))
    .with_sensor_configs(sensor_configs)
    .with_commands(commands.clone())
    .with_relay_commands(relay_commands.clone())
    .with_alerts(alerts)
    .with_ingest(Ingest {
        metrics: Some(metrics.queue()),
        events: Some(events.queue()),
    });
    let auto_update = AutoUpdate::new(pool.clone(), api_settings.agent_releases.clone())
        .with_enabled_override(config.auto_update.enabled)
        .with_registry(registry.clone())
        .with_relay_commands(relay_commands.clone());
    let api_state = ApiState::new(pool.clone(), registry, api_settings)
        .with_metrics(
            VmQuery::with_auth(&config.victoriametrics_url, QUERY_TIMEOUT, storage.as_ref())
                .context("VictoriaMetrics query client")?,
        )
        .with_logs(
            VmQuery::with_auth(&config.victorialogs_url, QUERY_TIMEOUT, storage.as_ref())
                .context("VictoriaLogs query client")?,
        )
        .with_enrollment(EnrollmentSettings::from_config(
            &config,
            enroll::ca_fingerprint(&ca),
        ))
        .with_crl(crl.clone())
        .with_ca_rotation(rotator.clone())
        .with_commands(commands.clone())
        .with_relay_commands(relay_commands.clone())
        .with_mirrors(mirrors.clone())
        .with_campaigns(campaigns.clone());
    let remediation =
        remediation_loop(&config, storage.as_ref(), pool.clone(), commands.clone()).await?;
    let remediation_pool = pool.clone();
    // Whether Proxmox can snapshot each mapped guest (ADR-0093); `None`
    // with `[proxmox]` off.
    let proxmox_checks = if config.proxmox.enabled {
        Some(
            ProxmoxSnapshotter::from_config(pool.clone(), &config.proxmox)
                .await
                .context("Proxmox")?,
        )
    } else {
        None
    };
    // Notifications (ADR-0060): the outbox the database triggers fill.
    let notify_pool = pool.clone();
    // The daily look for a newer release (ADR-0061).
    let check_pool = pool.clone();
    let renewer = Renewer::new(pool.clone(), ca.clone());
    let enroller = Enroller::new(pool, ca, FailureLimiter::default());

    // One signal stops every listener and the CRL timer.
    let (stop, stopped) = watch::channel(false);
    let signal = shutdown_signal()?;
    tokio::spawn(async move {
        signal.await;
        stop.send_replace(true);
    });
    let until_stopped = || {
        let mut stopped = stopped.clone();
        async move {
            let _ = stopped.wait_for(|&stop| stop).await;
        }
    };
    tracing::info!(
        grpc = %config.grpc_listen_addr,
        enroll = %config.enroll_listen_addr,
        api = %config.api_listen_addr,
        mirrors = %config.mirror_listen_addr,
        "listening"
    );
    let served = tokio::try_join!(
        async {
            grpc::serve_all(
                listener,
                tls_config,
                service,
                Some(renewer),
                Some(CrlService::new(crl.subscribe())),
                until_stopped(),
            )
            .await
            .context("gRPC server")
        },
        async {
            enroll::serve_following(
                enroll_listener,
                crl.enroll_config(),
                enroller,
                until_stopped(),
            )
            .await
            .context("enroll server")
        },
        async {
            api::serve(api_listener, api_state, until_stopped())
                .await
                .context("API server")
        },
        async {
            mirror::http::serve(mirror_listener, mirrors.layout().clone(), until_stopped())
                .await
                .context("mirror server")
        },
        async {
            mirror::scheduler::run(
                mirrors.clone(),
                config.mirror.max_concurrent_syncs,
                mirror::scheduler::POLL,
                until_stopped(),
            )
            .await;
            Ok(())
        },
        async {
            mirror::retention::run(mirrors.clone(), mirror::retention::EVERY, until_stopped())
                .await;
            Ok(())
        },
        async {
            campaigns.clone().run(campaign::POLL, until_stopped()).await;
            Ok(())
        },
        async {
            cluster.clone().run(until_stopped()).await;
            Ok(())
        },
        // The loops below need the whole fleet's view, or group and pace
        // what they send: the leader runs them (ADR-0089).
        async {
            cluster::lead(cluster.leadership(), stopped.clone(), |until| {
                // Servers and relays that stop reporting (ADR-0072).
                offline::run(
                    offline_pool.clone(),
                    offline_registry.clone(),
                    OfflineSettings::default(),
                    until,
                )
            })
            .await;
            Ok(())
        },
        async {
            cluster::lead(cluster.leadership(), stopped.clone(), |until| {
                auto_update.clone().run(
                    commands.clone(),
                    Duration::from_secs(config.auto_update.interval),
                    until,
                )
            })
            .await;
            Ok(())
        },
        async {
            match remediation {
                Some(remediation) => remediation.run(until_stopped()).await,
                // Off in the configuration: the web interface decides (ADR-0064).
                None => {
                    switch::supervise(
                        remediation_pool.clone(),
                        runner::POLL,
                        |wanted| {
                            start_from_settings(
                                &config,
                                storage.as_ref(),
                                remediation_pool.clone(),
                                commands.clone(),
                                wanted,
                            )
                        },
                        until_stopped(),
                    )
                    .await;
                }
            }
            Ok(())
        },
        async {
            if let Some(checks) = &proxmox_checks {
                cluster::lead(cluster.leadership(), stopped.clone(), |until| {
                    snapshot::check::run(checks.clone(), until)
                })
                .await;
            }
            Ok(())
        },
        async {
            let dispatcher = notify::Dispatcher::new(notify_pool, config.web_base_url())
                .with_status_dir(config.server_update_dir.clone());
            cluster::lead(cluster.leadership(), stopped.clone(), |until| {
                dispatcher.clone().run(notify::dispatcher::POLL, until)
            })
            .await;
            Ok(())
        },
        async {
            let settings = ServerUpdateSettings::from_config(&config);
            cluster::lead(cluster.leadership(), stopped.clone(), |until| {
                release_check::run(
                    check_pool.clone(),
                    settings.clone(),
                    release_check::AFTER_START,
                    until,
                )
            })
            .await;
            Ok(())
        },
        async {
            crl.run(
                crl::refresh_interval(&CaOptions::default()),
                until_stopped(),
            )
            .await;
            Ok(())
        },
        async {
            rotator.run(ca_rotation::POLL, until_stopped()).await;
            Ok(())
        },
    );
    // Streams are closed by now; flush what they queued, even after an error.
    let deadline = Duration::from_secs(config.ingest.shutdown_timeout);
    tokio::join!(metrics.shutdown(deadline), events.shutdown(deadline));
    served?;
    tracing::info!("server stopped");
    Ok(())
}

/// The AI remediation loop (ADR-0043), when `[ai]` is enabled.
async fn remediation_loop(
    config: &ServerConfig,
    storage: Option<&StorageAuth>,
    pool: sqlx::PgPool,
    commands: Commands,
) -> anyhow::Result<Option<Remediation>> {
    let Some(provider) = monitorflow_ai::from_config(&config.ai)
        .await
        .context("AI provider")?
    else {
        tracing::info!("AI remediation not in the configuration: set in the web interface");
        return Ok(None);
    };
    let model = config.ai.model.clone();
    build_remediation(config, storage, provider, &model, pool, commands)
        .await
        .map(Some)
}

/// The AI remediation loop on the settings of the web interface.
async fn start_from_settings(
    config: &ServerConfig,
    storage: Option<&StorageAuth>,
    pool: sqlx::PgPool,
    commands: Commands,
    wanted: switch::Wanted,
) -> anyhow::Result<Remediation> {
    let provider = wanted
        .connection(&config.ai)
        .provider()
        .context("AI provider")?;
    build_remediation(config, storage, provider, &wanted.model, pool, commands).await
}

async fn build_remediation(
    config: &ServerConfig,
    storage: Option<&StorageAuth>,
    provider: Arc<dyn monitorflow_ai::AiProvider>,
    model: &str,
    pool: sqlx::PgPool,
    commands: Commands,
) -> anyhow::Result<Remediation> {
    // Snapshots before change-tier commands (ADR-0046); without `[proxmox]`
    // every server records that rollback is limited.
    let snapshots = ProxmoxSnapshotter::from_config(pool.clone(), &config.proxmox)
        .await
        .context("Proxmox snapshots")?;
    if config.proxmox.enabled {
        tracing::info!(
            clusters = config.proxmox.clusters.len(),
            "Proxmox snapshots before remediation changes"
        );
    }
    let backends = Backends {
        pool,
        metrics: Some(
            VmQuery::with_auth(&config.victoriametrics_url, QUERY_TIMEOUT, storage)
                .context("VictoriaMetrics query client")?,
        ),
        logs: Some(
            VmQuery::with_auth(&config.victorialogs_url, QUERY_TIMEOUT, storage)
                .context("VictoriaLogs query client")?,
        ),
    };
    let commands_for_hook = commands.clone();
    let remediation = Remediation::new(
        provider,
        model,
        commands,
        backends,
        runner::Settings::from_config(&config.remediation),
    );
    // Servers without a Proxmox VM get local LVM or ZFS snapshots from
    // their agent where `/` allows it (ADR-0094).
    let hook = SnapshotHook::new(Arc::new(snapshots), "proxmox")
        .with_local(LocalSnapshots::new(commands_for_hook));
    Ok(remediation.with_hook(Arc::new(hook)))
}

async fn user_command(pool: &sqlx::PgPool, command: UserCommand) -> anyhow::Result<()> {
    match command {
        UserCommand::Add { username, grant } => {
            users::validate_username(&username)?;
            let scope = grant_scope(pool, &grant).await?;
            let password = tokio::task::spawn_blocking(read_password).await??;
            let id = users::create_user_with(pool, &username, &password, grant.role, scope, "cli")
                .await?;
            println!("created user {username} ({id}) with role {}", grant.role);
            // Like the setup page, so the first administrator can add
            // servers at once (ADR-0071).
            if grant.role == Role::Administrator && scope == GrantScope::Global {
                let mut tx = pool.begin().await?;
                if setup::ensure_default_site(&mut tx, ("system", "cli"))
                    .await?
                    .is_some()
                {
                    println!("created site {}", setup::DEFAULT_SITE);
                }
                tx.commit().await?;
            }
        }
        UserCommand::Grant { username, grant } => {
            let id = users::user_id(pool, &username)
                .await?
                .with_context(|| format!("user `{username}` does not exist"))?;
            let scope = grant_scope(pool, &grant).await?;
            if users::grant(pool, id, grant.role, scope, "cli").await? {
                println!("granted {} to {username}", grant.role);
            } else {
                println!("{username} already has this grant");
            }
        }
    }
    Ok(())
}

/// While no user exists, logs a fresh setup link at startup (ADR-0071).
async fn announce_setup(config: &ServerConfig, pool: &sqlx::PgPool) -> anyhow::Result<()> {
    if let Some(token) = setup::issue(pool).await.context("issuing the setup link")? {
        // The one secret the server logs: single use, short-lived, and it
        // grants nothing once an account exists (ADR-0071).
        tracing::warn!(
            setup_url = %token.setup_url(&config.web_base_url()),
            valid_hours = setup::TTL.as_secs() / 3600,
            "no user yet: open the setup link to create the administrator"
        );
    }
    Ok(())
}

async fn setup_link_command(config: &ServerConfig, pool: &sqlx::PgPool) -> anyhow::Result<()> {
    match setup::issue(pool).await? {
        Some(token) => {
            println!(
                "Open this link to create the administrator (valid {} hours, once):",
                setup::TTL.as_secs() / 3600
            );
            println!("{}", token.setup_url(&config.web_base_url()));
        }
        None => println!(
            "Setup is done: a user exists. Sign in at {}/",
            config.web_base_url()
        ),
    }
    Ok(())
}

async fn grant_scope(pool: &sqlx::PgPool, grant: &GrantArgs) -> anyhow::Result<GrantScope> {
    if let Some(name) = &grant.customer {
        return Ok(GrantScope::Customer(customer_id(pool, name).await?));
    }
    if let Some(name) = &grant.site {
        let site = token::site_id(pool, name, false)
            .await?
            .with_context(|| format!("site `{name}` does not exist"))?;
        return Ok(GrantScope::Site(site));
    }
    Ok(GrantScope::Global)
}

async fn customer_id(pool: &sqlx::PgPool, name: &str) -> anyhow::Result<uuid::Uuid> {
    customers::customer_id(pool, name)
        .await?
        .with_context(|| format!("customer `{name}` does not exist; create it with `customer add`"))
}

async fn customer_command(pool: &sqlx::PgPool, command: CustomerCommand) -> anyhow::Result<()> {
    match command {
        CustomerCommand::Add { name } => {
            let id = customers::create_customer(pool, &name, ("system", "cli")).await?;
            println!("created customer {name} ({id})");
        }
        CustomerCommand::List => {
            for customer in customers::list_all(pool).await? {
                println!(
                    "{}\t{}\t{} sites",
                    customer.id, customer.name, customer.site_count
                );
            }
        }
    }
    Ok(())
}

async fn mirror_command(
    config: &ServerConfig,
    pool: sqlx::PgPool,
    command: MirrorCommand,
) -> anyhow::Result<()> {
    let mirrors = open_mirrors(config, pool.clone()).await?;
    let find = |name: String| {
        let pool = pool.clone();
        async move {
            mirror::store::by_name(&pool, &name)
                .await?
                .with_context(|| format!("mirror `{name}` does not exist"))
        }
    };
    let (mirror, kind) = match command {
        MirrorCommand::Add(args) => {
            let spec = MirrorSpec {
                name: args.name,
                family: args.family.parse().map_err(anyhow::Error::msg)?,
                upstream_url: args.url,
                distribution: args.distribution,
                components: args.components,
                architectures: args.architectures,
                sync_interval_s: args.interval.0,
                auto_snapshot: !args.no_auto_snapshot,
            };
            let created = mirror::store::create(&pool, &spec, ("system", "cli")).await?;
            println!("created mirror {} ({})", created.name, created.id);
            return Ok(());
        }
        MirrorCommand::List => {
            for m in mirror::store::list(&pool).await? {
                let current = mirror::store::latest_published(&pool, m.id).await?;
                let last = mirror::store::runs(&pool, m.id, 1).await?.pop();
                let interval = m
                    .sync_interval_s
                    .map_or("manual".to_owned(), |s| format!("{s}s"));
                println!(
                    "{}\t{}\t{}\t{}\tcurrent={}\tlast={}",
                    m.name,
                    m.family,
                    m.upstream_url,
                    interval,
                    current.map_or("-".to_owned(), |s| s.name),
                    last.map_or("-".to_owned(), |r| format!(
                        "{:?} {:?} {}",
                        r.kind,
                        r.state,
                        r.started_at.format("%Y-%m-%d %H:%M:%S UTC")
                    ))
                    .to_lowercase(),
                );
            }
            return Ok(());
        }
        MirrorCommand::Key(command) => {
            let runner = ToolsRunner::new(&config.mirror, mirrors.layout());
            let key = match command {
                KeyCommand::Generate { uid } => {
                    mirror::signing::generate(&runner, mirrors.layout(), &uid).await?
                }
                KeyCommand::Import { file } => {
                    mirror::signing::import(&runner, mirrors.layout(), &file).await?
                }
            };
            println!(
                "signing key {key}; public key at {}",
                mirrors.layout().public_key().display()
            );
            return Ok(());
        }
        MirrorCommand::Prune => {
            let removed = mirror::retention::prune(&mirrors).await?;
            println!("removed {removed} snapshots");
            return Ok(());
        }
        MirrorCommand::Delete { name } => {
            let mirror = find(name).await?;
            mirror::removal::request(&pool, mirror.id, ("system", "cli")).await?;
            mirror::retention::prune(&mirrors).await?;
            match mirror::store::get(&pool, mirror.id).await? {
                None => println!("deleted mirror {}", mirror.name),
                Some(left) => anyhow::bail!(
                    "mirror {} is not fully deleted, the server retries within the hour: {}",
                    left.name,
                    left.delete_error.as_deref().unwrap_or("unknown error")
                ),
            }
            return Ok(());
        }
        MirrorCommand::Sync { name } => (find(name).await?, JobKind::Sync),
        MirrorCommand::Snapshot { name, snapshot } => {
            (find(name).await?, JobKind::Snapshot(snapshot))
        }
    };
    let job = mirrors
        .begin(&mirror, kind, Trigger::Manual, Some("cli"))
        .await?;
    mirror::jobs::audit_request(&pool, ("system", "cli"), &mirror, job.run()).await?;
    let run = job.execute().await;
    print!("{}", run.output);
    match run.error {
        None => {
            println!("run {} succeeded", run.id);
            Ok(())
        }
        Some(error) => anyhow::bail!("run {} failed: {error}", run.id),
    }
}

/// Prompts twice without echo on a terminal; reads one line otherwise.
fn read_password() -> anyhow::Result<Secret> {
    use std::io::{BufRead, IsTerminal};
    if std::io::stdin().is_terminal() {
        let first = rpassword::prompt_password("Password: ")?;
        let second = rpassword::prompt_password("Repeat password: ")?;
        anyhow::ensure!(first == second, "passwords do not match");
        Ok(Secret::new(first))
    } else {
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        Ok(Secret::new(line.trim_end_matches(['\r', '\n'])))
    }
}

/// Opens `<data_dir>/ca/`, creating the CA on first use.
fn storage_urls(config: &ServerConfig) -> [&str; 2] {
    [&config.victoriametrics_url, &config.victorialogs_url]
}

async fn storage_certs_command(config: &ServerConfig) -> anyhow::Result<()> {
    let dir = config
        .storage_tls_dir
        .clone()
        .context("storage_tls_dir is not set")?;
    let names = storage_tls::names(&storage_urls(config))?;
    let ca = open_ca(config).await?;
    let outcome =
        tokio::task::spawn_blocking(move || storage_tls::ensure(&ca, &dir, &names)).await??;
    match outcome {
        storage_tls::Outcome::Issued => println!("storage certificate issued"),
        storage_tls::Outcome::Kept => println!("storage certificate already valid"),
    }
    Ok(())
}

async fn storage_check_command(config: &ServerConfig) -> anyhow::Result<()> {
    let storage = match &config.storage_tls_dir {
        Some(dir) => {
            let ca_cert = config
                .data_dir
                .join("ca")
                .join(monitorflow_server::ca::CERT_FILE);
            let dir = dir.clone();
            let auth = tokio::task::spawn_blocking(move || {
                let ca = std::fs::read_to_string(&ca_cert)
                    .with_context(|| format!("reading {}", ca_cert.display()))?;
                anyhow::Ok(StorageAuth::load(ca, &dir)?)
            })
            .await??;
            Some(auth)
        }
        None => None,
    };
    let checks = [
        (
            "VictoriaMetrics",
            &config.victoriametrics_url,
            "/api/v1/query",
            vec![("query", "1".to_owned())],
        ),
        (
            "VictoriaLogs",
            &config.victorialogs_url,
            "/select/logsql/query",
            vec![("query", "*".to_owned()), ("limit", "1".to_owned())],
        ),
    ];
    let mut failed = false;
    for (name, url, path, params) in checks {
        let client = VmQuery::with_auth(url, QUERY_TIMEOUT, storage.as_ref())?;
        let tls = if storage.is_some() {
            "TLS, signed by the built-in CA"
        } else {
            "no TLS"
        };
        match client.get_raw(path, &params).await {
            Ok(_) => println!("{name} answers ({tls})"),
            Err(e) => {
                println!("{name} does not answer: {e}");
                failed = true;
            }
        }
    }
    anyhow::ensure!(!failed, "a store does not answer");
    Ok(())
}

async fn open_ca(config: &ServerConfig) -> anyhow::Result<CertificateAuthority> {
    let dir = config.data_dir.join("ca");
    tokio::task::spawn_blocking(move || {
        CertificateAuthority::open_or_create(&dir, CaOptions::default())
    })
    .await?
    .context("opening the certificate authority")
}

/// Prepares `mirror_dir` and exports the public signing key if there is
/// one. Missing tools are not fatal: mirrors are optional.
async fn open_mirrors(config: &ServerConfig, pool: sqlx::PgPool) -> anyhow::Result<Mirrors> {
    let layout = Layout::new(config.mirror_dir());
    let prepared = layout.clone();
    tokio::task::spawn_blocking(move || prepared.prepare())
        .await?
        .with_context(|| format!("preparing {}", layout.root().display()))?;
    let runner = Arc::new(ToolsRunner::new(&config.mirror, &layout));
    match mirror::signing::export(runner.as_ref(), &layout).await {
        Ok(Some(key)) => tracing::info!(%key, "mirror signing key exported"),
        Ok(None) => tracing::info!(
            "no mirror signing key; snapshots fail until `mirror key generate` or `mirror key import`"
        ),
        Err(error) => tracing::warn!(%error, "cannot export the mirror signing key"),
    }
    Ok(Mirrors::new(pool, runner, layout))
}

/// `token create`: stdout holds only the token.
async fn create_token(config: &ServerConfig, args: CreateToken) -> anyhow::Result<()> {
    let ca = open_ca(config).await?;
    let pool = db::connect(&config.database_url).await?;
    db::migrate(&pool).await?;
    let site_id = match &args.customer {
        Some(name) => {
            let customer = customer_id(&pool, name).await?;
            customers::site_of_customer(&pool, &args.site, customer, args.create_site).await?
        }
        None => token::site_id(&pool, &args.site, args.create_site).await?,
    };
    let site_id = site_id.with_context(|| {
        format!(
            "site `{}` does not exist; pass --create-site to create it",
            args.site
        )
    })?;
    let role = if args.role == "relay" {
        PeerRole::Relay
    } else {
        PeerRole::Agent
    };
    let request = token::TokenRequest {
        role,
        peer_id: args.id,
        site_id,
        ttl: args.ttl,
        creator: token::Creator::Cli,
        server_names: args.server_names,
        parent_relay: args.parent_relay,
    };
    let created = token::create(&pool, enroll::ca_fingerprint(&ca), &request).await?;
    println!("{}", created.token.to_secret_string().expose());
    eprintln!(
        "Single-use token for {role} {} on site `{}`, valid until {}. It is not shown again.",
        request.peer_id,
        args.site,
        created.expires_at.format("%Y-%m-%d %H:%M:%S UTC")
    );
    Ok(())
}

/// Resolves on the first SIGINT or SIGTERM.
fn shutdown_signal() -> anyhow::Result<impl std::future::Future<Output = ()>> {
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    Ok(async move {
        let name = tokio::select! {
            _ = interrupt.recv() => "SIGINT",
            _ = terminate.recv() => "SIGTERM",
        };
        tracing::info!(signal = name, "shutting down");
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intervals() {
        assert_eq!(parse_interval("manual"), Ok(Interval(None)));
        assert_eq!(parse_interval("15m"), Ok(Interval(Some(900))));
        assert_eq!(parse_interval("6h"), Ok(Interval(Some(21_600))));
        assert_eq!(parse_interval("1d"), Ok(Interval(Some(86_400))));
        for bad in ["", "10m", "1w", "h", "-1d", "99999999d"] {
            assert!(parse_interval(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn mirror_commands_parse() {
        let cli = Cli::try_parse_from([
            "monitorflow-server",
            "mirror",
            "add",
            "--name",
            "debian-12",
            "--family",
            "apt",
            "--url",
            "https://deb.debian.org/debian",
            "--distribution",
            "bookworm",
            "--component",
            "main",
            "--component",
            "contrib",
            "--arch",
            "amd64",
            "--interval",
            "manual",
        ])
        .unwrap();
        let Some(Command::Mirror(MirrorCommand::Add(add))) = cli.command else {
            panic!("not mirror add");
        };
        assert_eq!(add.components, ["main", "contrib"]);
        assert_eq!(add.interval, Interval(None));
        assert!(!add.no_auto_snapshot);
        let cli = Cli::try_parse_from(["monitorflow-server", "mirror", "snapshot", "--name", "d"])
            .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Mirror(MirrorCommand::Snapshot {
                snapshot: None,
                ..
            }))
        ));
        assert!(Cli::try_parse_from(["monitorflow-server", "mirror", "key", "import"]).is_err());
    }
}
