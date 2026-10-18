//! MonitorFlow server entry point.

use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use monitorflow_common::config::{self, ServerConfig};
use monitorflow_common::logging;
use monitorflow_common::tls::Identity;
use monitorflow_server::api::{self, ApiSettings, ApiState};
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
