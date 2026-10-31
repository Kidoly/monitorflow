//! The mirror tools sidecar (ADR-0067): runs `aptly`, `reposync`,
//! `createrepo_c` and `gpg` for the server, which shares `mirror_dir`
//! with this container. It needs no configuration, no database and no
//! secret of the server.

use std::path::PathBuf;

use clap::Parser;
use monitorflow_common::config::{LogConfig, MirrorConfig};
use monitorflow_common::logging;
use monitorflow_server::mirror::{tools, Layout};
use tokio::signal::unix::{signal, SignalKind};

#[derive(Parser, Debug)]
#[command(name = "monitorflow-mirror-tools", version = monitorflow_common::VERSION)]
struct Cli {
    /// The server's `mirror_dir`.
    mirror_dir: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    logging::init(&LogConfig::default());
    let mut terminate = signal(SignalKind::terminate())?;
    let stop = async move {
        tokio::select! {
            _ = terminate.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    };
    tools::serve(&Layout::new(cli.mirror_dir), MirrorConfig::default(), stop).await?;
    Ok(())
}
