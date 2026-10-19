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
use monitorflow_server::crl::{self, CrlPublisher};
use monitorflow_server::db;
use monitorflow_server::enroll::{self, server_cert, token, Enroller, FailureLimiter};
use monitorflow_server::grpc::{
    self, AgentStreamService, CrlService, DbInventory, DbRelayPolicy, Registry, StreamSettings,
};
use monitorflow_server::ingest::{Ingest, JsonLines, RemoteWrite, Writer, WriterSettings};
use monitorflow_server::renew::Renewer;
use monitorflow_server::sensor_config::{SensorConfigStore, SensorConfigs, WatcherSettings};
use monitorflow_server::vm_query::VmQuery;
use tokio::net::TcpListener;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::watch;

#[derive(Parser, Debug)]
#[command(name = "monitorflow-server", version, about = "MonitorFlow server")]
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
        Some(Command::User { .. } | Command::Customer(_)) | None => {}
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
        Some(Command::Token(_)) | None => {}
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
    let listener = TcpListener::bind(config.grpc_listen_addr)
        .await
        .with_context(|| format!("binding gRPC listener on {}", config.grpc_listen_addr))?;
    let enroll_listener = TcpListener::bind(config.enroll_listen_addr)
        .await
        .with_context(|| format!("binding enroll listener on {}", config.enroll_listen_addr))?;
    let api_listener = TcpListener::bind(config.api_listen_addr)
        .await
        .with_context(|| format!("binding API listener on {}", config.api_listen_addr))?;
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
    let service = AgentStreamService::new(
        registry.clone(),
        Arc::new(DbRelayPolicy::new(pool.clone())),
        StreamSettings::from_config(&config),
    )
    .with_inventory(Arc::new(DbInventory::new(pool.clone())))
    .with_sensor_configs(sensor_configs)
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
        ));
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
