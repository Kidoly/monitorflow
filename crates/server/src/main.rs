//! MonitorFlow server entry point.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use monitorflow_common::config::{self, ServerConfig};
use monitorflow_common::logging;
use monitorflow_common::redact::Secret;
use monitorflow_common::tls::{Identity, PeerRole};
use monitorflow_server::alerting::{AlertStore, Alerts, EngineSettings, LogNotifier};
use monitorflow_server::api::users::GrantScope;
use monitorflow_server::api::{
    self, customers, users, ApiSettings, ApiState, EnrollmentSettings, Role,
};
use monitorflow_server::ca::{CaOptions, CertificateAuthority};
use monitorflow_server::campaign::{self, Runner};
use monitorflow_server::commands::{CommandStore, Commands, DispatcherSettings};
use monitorflow_server::crl::{self, CrlPublisher};
use monitorflow_server::db;
use monitorflow_server::enroll::{self, server_cert, token, Enroller, FailureLimiter};
use monitorflow_server::grpc::{
    self, AgentStreamService, CrlService, DbInventory, DbRelayPolicy, Registry, StreamSettings,
};
use monitorflow_server::ingest::{Ingest, JsonLines, RemoteWrite, Writer, WriterSettings};
use monitorflow_server::mirror::{
    self, CommandRunner, JobKind, Layout, MirrorSpec, Mirrors, Trigger,
};
use monitorflow_server::remediation::context::Backends;
use monitorflow_server::remediation::runner::{self, Remediation};
use monitorflow_server::remediation::snapshot::{ProxmoxSnapshotter, SnapshotHook};
use monitorflow_server::renew::Renewer;
use monitorflow_server::sensor_config::{SensorConfigStore, SensorConfigs, WatcherSettings};
use monitorflow_server::vm_query::VmQuery;
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
    /// Manage enrollment tokens.
    #[command(subcommand)]
    Token(TokenCommand),
    /// Manage customers (ADR-0026).
    #[command(subcommand)]
    Customer(CustomerCommand),
    /// Manage package mirrors (ADR-0030).
    #[command(subcommand)]
    Mirror(MirrorCommand),
    /// Agent releases served for self-updates (ADR-0053).
    #[command(subcommand)]
    AgentRelease(AgentReleaseCommand),
}

#[derive(Subcommand, Debug)]
enum AgentReleaseCommand {
    /// Copy a release from a directory of release files into the cache,
    /// once its signature verifies.
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
    /// Create a one-time enrollment token and print it, once, on stdout.
    Create(CreateToken),
}

#[derive(Args, Debug)]
struct CreateToken {
    /// Role of the peer to enroll.
    #[arg(long, value_parser = ["agent", "relay"])]
    role: String,
    /// Peer ID written in the certificate: 1 to 64 of [A-Za-z0-9._-].
    #[arg(long)]
    id: String,
    /// Name of the site the peer belongs to.
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
    /// For a relay: a DNS name or IP address its agents connect to. Repeat
    /// for several; they go in the relay's certificate (ADR-0025).
    #[arg(long = "server-name", value_name = "NAME")]
    server_names: Vec<String>,
    /// For a relay that connects through another relay: the peer ID of
    /// that relay, its parent (ADR-0028).
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
    use monitorflow_server::agent_releases::ReleaseCache;

    let cache = ReleaseCache::in_mirror_dir(&config.mirror_dir());
    match command {
        AgentReleaseCommand::Import { dir } => {
            let keys = monitorflow_common::release::trusted_keys()?;
            let version = cache.import(&dir, keys)?;
            println!("imported {version} into {}", cache.dir().display());
        }
        AgentReleaseCommand::List => {
            for version in cache.versions()? {
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
    match cli.command {
        Some(Command::Token(TokenCommand::Create(args))) => {
            return create_token(&config, args).await
        }
        Some(Command::AgentRelease(command)) => return agent_release_command(&config, command),
        Some(Command::User { .. } | Command::Customer(_) | Command::Mirror(_)) | None => {}
    }
    logging::init(&config.log);
    tracing::info!(config = %cli.config.display(), protocol = monitorflow_common::PROTOCOL_VERSION, "starting server");
    // Secrets are redacted by their Debug impls.
    tracing::info!(effective = ?config, "configuration loaded");

    let pool = db::connect(&config.database_url).await?;
    db::migrate(&pool).await?;

    match cli.command {
        Some(Command::User { command }) => return user_command(&pool, command).await,
        Some(Command::Customer(command)) => return customer_command(&pool, command).await,
        Some(Command::Mirror(command)) => return mirror_command(&config, pool, command).await,
        Some(Command::Token(_) | Command::AgentRelease(_)) | None => {}
    }

    let ca = Arc::new(open_ca(&config).await?);
    let names = server_cert::parse_names(&config.server_names);
    let tls_paths = config.tls_paths();
    let (issuer, paths) = (ca.clone(), tls_paths.clone());
    tokio::task::spawn_blocking(move || server_cert::ensure(&issuer, &paths, &names))
        .await?
        .context("preparing the server certificate")?;
    let identity = Identity::load(&tls_paths).context("loading the TLS identity")?;

    // Every new mTLS connection uses the latest config from the publisher,
    // which carries a fresh CRL.
    let (crl, tls_config) = CrlPublisher::start(ca.clone(), identity.clone()).await?;
    let crl = Arc::new(crl);
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
    // Storage writers (ADR-0013). Streams only push to their queues.
    let writer_settings = WriterSettings::from_config(&config.ingest);
    let metrics = Writer::spawn(
        RemoteWrite::new(&config.victoriametrics_url).context("VictoriaMetrics client")?,
        writer_settings.clone(),
    );
    let events = Writer::spawn(
        JsonLines::new(&config.victorialogs_url).context("VictoriaLogs client")?,
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
    // Update campaigns (ADR-0033) queue through the same dispatcher, and
    // pin snapshots with the mirrors' key and public URL (ADR-0038).
    let campaigns = Runner::new(pool.clone())
        .with_commands(commands.clone())
        .with_sources(config.mirror_public_url.clone(), mirrors.clone())
        .with_agent_releases(monitorflow_server::agent_releases::Sources::from_config(
            &config,
        ));
    let service = AgentStreamService::new(
        registry.clone(),
        Arc::new(DbRelayPolicy::new(pool.clone())),
        StreamSettings::from_config(&config),
    )
    .with_inventory(Arc::new(DbInventory::new(pool.clone())))
    .with_sensor_configs(sensor_configs)
    .with_commands(commands.clone())
    .with_alerts(alerts)
    .with_ingest(Ingest {
        metrics: Some(metrics.queue()),
        events: Some(events.queue()),
    });
    let api_state = ApiState::new(pool.clone(), registry, ApiSettings::from_config(&config))
        .with_metrics(
            VmQuery::new(&config.victoriametrics_url).context("VictoriaMetrics query client")?,
        )
        .with_enrollment(EnrollmentSettings::from_config(
            &config,
            enroll::ca_fingerprint(&ca),
        ))
        .with_crl(crl.clone())
        .with_commands(commands.clone())
        .with_mirrors(mirrors.clone())
        .with_campaigns(campaigns.clone());
    let remediation = remediation_loop(&config, pool.clone(), commands.clone()).await?;
    let auto_update_pool = pool.clone();
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
            enroll::serve(
                enroll_listener,
                identity.server_config_without_client_auth()?,
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
            campaigns.clone().run(campaign::POLL, until_stopped()).await;
            Ok(())
        },
        async {
            monitorflow_server::agent_auto_update::run(
                auto_update_pool.clone(),
                monitorflow_server::agent_releases::Sources::from_config(&config),
                commands.clone(),
                monitorflow_server::agent_auto_update::POLL,
                until_stopped(),
            )
            .await;
            Ok(())
        },
        async {
            if let Some(remediation) = remediation {
                remediation.run(until_stopped()).await;
            }
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
    pool: sqlx::PgPool,
    commands: Commands,
) -> anyhow::Result<Option<Remediation>> {
    let Some(provider) = monitorflow_ai::from_config(&config.ai)
        .await
        .context("AI provider")?
    else {
        tracing::info!("AI remediation disabled ([ai] enabled = false)");
        return Ok(None);
    };
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
            VmQuery::new(&config.victoriametrics_url).context("VictoriaMetrics query client")?,
        ),
        logs: Some(VmQuery::new(&config.victorialogs_url).context("VictoriaLogs query client")?),
    };
    let remediation = Remediation::new(
        provider,
        &config.ai.model,
        commands,
        backends,
        runner::Settings::from_config(&config.remediation),
    );
    Ok(Some(remediation.with_hook(Arc::new(SnapshotHook::new(
        Arc::new(snapshots),
        "proxmox",
    )))))
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
            let runner = CommandRunner::new(&config.mirror, mirrors.layout().gnupg());
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
    let runner = Arc::new(CommandRunner::new(&config.mirror, layout.gnupg()));
    match mirror::signing::export(runner.as_ref(), &layout).await {
        Ok(Some(key)) => tracing::info!(%key, "mirror signing key exported"),
        Ok(None) => tracing::info!(
            "no mirror signing key; snapshots fail until `mirror key generate` or `mirror key import`"
        ),
        Err(error) => tracing::warn!(%error, "cannot export the mirror signing key"),
    }
    Ok(Mirrors::new(pool, runner, layout))
}

/// `token create`: no logging, so stdout holds only the token.
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
        "Enrollment token for monitorflow://{role}/{} on site `{}`, valid until {}. It is not shown again.",
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
