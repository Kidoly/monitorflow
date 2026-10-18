//! MonitorFlow server entry point.

use clap::Parser;
use monitorflow_common::config::{self, ServerConfig};
use monitorflow_common::logging;

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
    // Server logic is added by the Phase 1 tasks in docs/roadmap.md.
    Ok(())
}
