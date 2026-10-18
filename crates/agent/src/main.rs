//! MonitorFlow agent entry point.

use clap::Parser;
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
    // Agent logic is added by the Phase 1 tasks in docs/roadmap.md.
    Ok(())
}
