//! MonitorFlow server entry point.

use std::sync::Arc;

use anyhow::Context;
use clap::{Parser, Subcommand};
use monitorflow_common::config::{self, ServerConfig};
use monitorflow_common::logging;
use monitorflow_common::redact::Secret;
use monitorflow_common::tls::Identity;
use monitorflow_server::api::{self, users, ApiSettings, ApiState, Role};
use monitorflow_server::db;
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
        default_value = "/etc/monitorflow/server.toml"
    )]
    config: std::path::PathBuf,
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

    let identity = Identity::load(&config.tls).context("loading the TLS identity")?;
    // No CRL yet. The CA will send configs with fresh CRLs through this
    // channel; each new connection uses the latest one.
    let (_tls_updates, tls_config) = watch::channel(identity.server_config(Vec::new())?);

    let listener = TcpListener::bind(config.grpc_listen_addr)
        .await
        .with_context(|| format!("binding gRPC listener on {}", config.grpc_listen_addr))?;
    let api_listener = TcpListener::bind(config.api_listen_addr)
        .await
        .with_context(|| format!("binding API listener on {}", config.api_listen_addr))?;
    let registry = Registry::default();
    // Relays may only speak for themselves until site assignments exist.
    let service = AgentStreamService::new(
        registry.clone(),
        Arc::new(DenyRelayedAgents),
        StreamSettings::from_config(&config),
    );
    let api_state = ApiState::new(pool, registry, ApiSettings::from_config(&config));

    // One signal stops both listeners.
    let (stop, stopped) = watch::channel(false);
    let signal = shutdown_signal()?;
    tokio::spawn(async move {
        signal.await;
        let _ = stop.send(true);
    });
    let wait = |mut rx: watch::Receiver<bool>| async move {
        let _ = rx.wait_for(|stop| *stop).await;
    };

    tracing::info!(addr = %config.grpc_listen_addr, "gRPC listening");
    tracing::info!(addr = %config.api_listen_addr, "API listening");
    tokio::try_join!(
        async {
            grpc::serve(listener, tls_config, service, wait(stopped.clone()))
                .await
                .context("gRPC server")
        },
        async {
            api::serve(api_listener, api_state, wait(stopped.clone()))
                .await
                .context("API server")
        },
    )?;
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
