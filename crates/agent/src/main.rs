//! MonitorFlow agent entry point.

use std::time::SystemTime;

use anyhow::Context;
use clap::Parser;
use monitorflow_agent::{identity, os};
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

    let paths = config.tls_paths();
    let identity = tokio::task::spawn_blocking(move || identity::load(&paths))
        .await?
        .context("no usable agent identity")?;
    let Some(identity) = identity else {
        // Enrollment is added by the next roadmap step.
        anyhow::bail!("not enrolled yet");
    };
    let now = SystemTime::now();
    if identity.is_expired(now) {
        tracing::error!(peer = %identity.id, "certificate has expired; remove the identity files and enroll again");
    } else if identity.renewal_due(now) {
        tracing::warn!(peer = %identity.id, "certificate is close to expiry and renewal is not implemented yet");
    }
    tracing::info!(peer = %identity.id, "identity loaded");
    // The upstream stream is added by the next roadmap step.
    Ok(())
}
