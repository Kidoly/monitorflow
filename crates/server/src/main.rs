//! MonitorFlow server entry point.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use monitorflow_common::config::{self, ServerConfig};
use monitorflow_common::logging;
use monitorflow_common::redact::Secret;
use monitorflow_common::tls::{Identity, PeerRole};
use monitorflow_server::api::{self, users, ApiSettings, ApiState, Role};
use monitorflow_server::ca::{CaOptions, CertificateAuthority};
use monitorflow_server::crl::{self, CrlPublisher};
use monitorflow_server::db;
use monitorflow_server::enroll::{self, server_cert, token, Enroller, FailureLimiter};
use monitorflow_server::grpc::{
    self, AgentStreamService, DbInventory, DbRelayPolicy, Registry, StreamSettings,
};
use monitorflow_server::ingest::{Ingest, JsonLines, RemoteWrite, Writer, WriterSettings};
use monitorflow_server::renew::Renewer;
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
    /// Validity, like 90s, 15m, 1h or 7d (at most 7d).
    #[arg(long, default_value = "1h", value_parser = token::parse_ttl)]
    ttl: Duration,
}

#[derive(Subcommand, Debug)]
enum UserCommand {
    /// Create a user with a global role. The password is read from the
    /// terminal, or from the first line of stdin when it is not a terminal.
    Add {
        #[arg(long)]
        username: String,
        /// viewer, operator, approver or administrator (alias: admin).
        #[arg(long, value_parser = parse_role)]
        role: Role,
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
        Some(Command::User { .. }) | None => {}
    }
    logging::init(&config.log);
    tracing::info!(config = %cli.config.display(), protocol = monitorflow_common::PROTOCOL_VERSION, "starting server");
    // Secrets are redacted by their Debug impls.
    tracing::info!(effective = ?config, "configuration loaded");

    let pool = db::connect(&config.database_url).await?;
    db::migrate(&pool).await?;

    if let Some(Command::User {
        command: UserCommand::Add { username, role },
    }) = cli.command
    {
        return add_user(&pool, &username, role).await;
    }

    let ca = Arc::new(open_ca(&config).await?);
    let names = server_cert::parse_names(&config.server_names);
    let (issuer, paths) = (ca.clone(), config.tls.clone());
    tokio::task::spawn_blocking(move || server_cert::ensure(&issuer, &paths, &names))
        .await?
        .context("preparing the server certificate")?;
    let identity = Identity::load(&config.tls).context("loading the TLS identity")?;

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
    let registry = Registry::default();
    let service = AgentStreamService::new(
        registry.clone(),
        Arc::new(DbRelayPolicy::new(pool.clone())),
        StreamSettings::from_config(&config),
    )
    .with_inventory(Arc::new(DbInventory::new(pool.clone())))
    .with_ingest(Ingest {
        metrics: Some(metrics.queue()),
        events: Some(events.queue()),
    });
    let api_state = ApiState::new(pool.clone(), registry, ApiSettings::from_config(&config));
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
            grpc::serve_with_renewer(
                listener,
                tls_config,
                service,
                Some(renewer),
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

async fn add_user(pool: &sqlx::PgPool, username: &str, role: Role) -> anyhow::Result<()> {
    users::validate_username(username)?;
    let password = tokio::task::spawn_blocking(read_password).await??;
    let id = users::create_user(pool, username, &password, role, "cli").await?;
    println!("created user {username} ({id}) with role {role}");
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
    let site_id = token::site_id(&pool, &args.site, args.create_site)
        .await?
        .with_context(|| {
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
