//! MonitorFlow server entry point.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use monitorflow_common::config::{self, ServerConfig};
use monitorflow_common::logging;
use monitorflow_common::tls::{Identity, PeerRole};
use monitorflow_server::ca::{CaOptions, CertificateAuthority};
use monitorflow_server::db;
use monitorflow_server::enroll::{self, token};
use monitorflow_server::grpc::{
    self, AgentStreamService, DenyRelayedAgents, Registry, StreamSettings,
};
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config: ServerConfig = config::load(&cli.config)?;
    match cli.command {
        Some(Command::Token(TokenCommand::Create(args))) => {
            return create_token(&config, args).await
        }
        None => {}
    }
    logging::init(&config.log);
    tracing::info!(config = %cli.config.display(), protocol = monitorflow_common::PROTOCOL_VERSION, "starting server");
    // Secrets are redacted by their Debug impls.
    tracing::info!(effective = ?config, "configuration loaded");

    let identity = Identity::load(&config.tls).context("loading the TLS identity")?;
    // No CRL yet. The CA will send configs with fresh CRLs through this
    // channel; each new connection uses the latest one.
    let (_tls_updates, tls_config) = watch::channel(identity.server_config(Vec::new())?);

    let listener = TcpListener::bind(config.grpc_listen_addr)
        .await
        .with_context(|| format!("binding gRPC listener on {}", config.grpc_listen_addr))?;
    // Relays may only speak for themselves until site assignments exist.
    let service = AgentStreamService::new(
        Registry::default(),
        Arc::new(DenyRelayedAgents),
        StreamSettings::from_config(&config),
    );
    tracing::info!(addr = %config.grpc_listen_addr, "gRPC listening");
    grpc::serve(listener, tls_config, service, shutdown_signal()?).await?;
    tracing::info!("server stopped");
    Ok(())
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
