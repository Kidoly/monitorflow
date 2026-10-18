//! MonitorFlow agent entry point.

use std::time::SystemTime;

use anyhow::Context;
use clap::Parser;
use monitorflow_agent::backoff::Backoff;
use monitorflow_agent::enroll::{self, EnrollError};
use monitorflow_agent::stream::{Agent, StreamSettings};
use monitorflow_agent::{os, shutdown};
use monitorflow_common::config::{self, AgentConfig};
use monitorflow_common::logging;

#[derive(Parser, Debug)]
#[command(name = "monitorflow-agent", version, about = "MonitorFlow agent")]
struct Cli {
    /// Path to the TOML configuration file.
    #[arg(
        long,
        env = "MONITORFLOW_CONFIG",
        default_value = "/etc/monitorflow/agent.toml"
    )]
    config: std::path::PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config: AgentConfig = config::load(&cli.config)?;
    logging::init(&config.log);
    tracing::info!(config = %cli.config.display(), protocol = monitorflow_common::PROTOCOL_VERSION, "starting agent");
    // Secrets are redacted by their Debug impls.
    tracing::info!(effective = ?config, "configuration loaded");

    // Before enrolling, so an unsupported host never spends a token.
    let host = os::detect().context("cannot start on this host")?;
    tracing::info!(hostname = %host.hostname, os_family = %host.os_family, os_version = %host.os_version, "host detected");

    let (trigger, stop) = shutdown::channel();
    shutdown::on_signals(trigger).context("cannot install signal handlers")?;
    let identity = match enroll::ensure_identity(&config, Backoff::default(), stop.clone()).await {
        Ok(identity) => identity,
        Err(EnrollError::Shutdown) => return Ok(()),
        Err(error) => return Err(error).context("no usable agent identity"),
    };
    let now = SystemTime::now();
    if identity.is_expired(now) {
        tracing::error!(peer = %identity.id, "certificate has expired; remove the identity files and enroll again");
    } else if identity.renewal_due(now) {
        tracing::warn!(peer = %identity.id, "certificate is close to expiry and renewal is not implemented yet");
    }
    tracing::info!(peer = %identity.id, "identity loaded");
    Agent::new(StreamSettings::from_config(&config), identity, host)
        .run(stop)
        .await
        .context("cannot connect upstream")
}
