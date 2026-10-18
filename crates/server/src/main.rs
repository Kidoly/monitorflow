//! MonitorFlow server entry point.

use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use monitorflow_common::config::{self, ServerConfig};
use monitorflow_common::logging;
use monitorflow_common::tls::Identity;
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
