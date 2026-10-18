//! MonitorFlow agent entry point.

use anyhow::Context;
use clap::Parser;
use monitorflow_agent::backoff::Backoff;
use monitorflow_agent::enroll::{self, EnrollError};
use monitorflow_agent::renew::{Renewal, RenewalSettings};
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
    tracing::info!(peer = %identity.id, not_after = ?identity.not_after, "identity loaded");
    // The renewal task logs an expired certificate with the steps to enroll
    // again; the stream keeps retrying in case the operator does.
    let (renewal, updates) = Renewal::new(
        RenewalSettings::from_config(&config),
        config.tls_paths(),
        identity.clone(),
    );
    tokio::spawn(renewal.run(stop.clone()));
    Agent::new(StreamSettings::from_config(&config), identity, host)
        .follow(updates)
        .run(stop)
        .await
        .context("cannot connect upstream")
}
