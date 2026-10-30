//! `monitorflow-updater`: the compose deployment's updater (ADR-0056).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context};
use clap::{Parser, Subcommand};
use monitorflow_common::config::LogConfig;
use monitorflow_common::release::trusted_keys;
use monitorflow_common::server_update::Request;
use monitorflow_updater::env_file::EnvFile;
use monitorflow_updater::host::{self, Compose};
use monitorflow_updater::project::Project;
use monitorflow_updater::source::{self, Source};
use monitorflow_updater::update::{Reporter, Updater};
use monitorflow_updater::{restore, serve, Lock, Settings};
use tokio::signal::unix::{signal, SignalKind};

#[derive(Parser, Debug)]
#[command(name = "monitorflow-updater", version = monitorflow_common::VERSION, about = "MonitorFlow server updater")]
struct Cli {
    /// Without a command, serve the server's requests.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Update now from a directory of release files, whose images are
    /// already loaded: the offline path.
    Update {
        #[arg(long)]
        dir: PathBuf,
    },
    /// Recreate the updater service with the image in `.env`; run in a
    /// one-off container by the updater it replaces.
    ReplaceSelf,
    /// Replace the database and the server's data with a backup's, then
    /// start everything: what `deploy/restore.sh` runs (ADR-0058).
    Restore { file: PathBuf },
}

/// The compose labels of this container.
async fn own_labels() -> anyhow::Result<HashMap<String, String>> {
    let id = std::env::var("HOSTNAME").context("HOSTNAME is not set")?;
    let output = tokio::process::Command::new("docker")
        .args(["inspect", "--format", "{{json .Config.Labels}}", &id])
        .output()
        .await
        .context("running docker")?;
    if !output.status.success() {
        bail!(
            "docker inspect {id}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

async fn compose(settings: &Settings) -> anyhow::Result<Compose> {
    let project = Project::from_labels(&own_labels().await?, &settings.deploy_dir)?;
    project.link()?;
    Ok(Compose {
        docker: "docker".into(),
        project,
        timeout: settings.timeout,
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    monitorflow_common::logging::init(&LogConfig::default());
    let settings = Settings::from_env().map_err(anyhow::Error::msg)?;
    let keys = trusted_keys()?.to_vec();
    let compose = compose(&settings).await?;

    if let Some(Command::ReplaceSelf) = cli.command {
        host::recreate_updater(&compose).await?;
        return Ok(());
    }
    settings.prepare_dirs().context("preparing the volumes")?;
    if let Some(Command::Restore { file }) = &cli.command {
        let _lock = Lock::acquire(&settings.state_dir)?;
        let work = std::env::temp_dir().join(format!("monitorflow-restore-{}", std::process::id()));
        std::fs::create_dir_all(&work)?;
        let result = restore::restore(
            &compose,
            file,
            &settings.serverdata_dir,
            &work,
            &mut |line| println!("{line}"),
        )
        .await;
        let _ = std::fs::remove_dir_all(&work);
        if let Err(e) = result {
            eprintln!("restore failed: {e}");
            std::process::exit(1);
        }
        return Ok(());
    }

    let http = source::Http {
        client: reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(30 * 60))
            .build()?,
        source: settings.source().to_owned(),
        keys: keys.clone(),
    };
    let dir_source;
    let source: &dyn Source = match &cli.command {
        Some(Command::Update { dir }) => {
            dir_source = source::Dir {
                dir: dir.clone(),
                keys,
            };
            &dir_source
        }
        _ => &http,
    };
    let mut updater = Updater {
        host: &compose,
        source,
        env: EnvFile::new(compose.project.local(&compose.project.env_file)),
        update_dir: settings.update_dir.clone(),
        server_update_dir: host::SERVER_UPDATE_DIR.into(),
        state_dir: settings.state_dir.clone(),
        backup_dir: settings.backup_dir.clone(),
        serverdata_dir: settings.serverdata_dir.clone(),
        reporter: Reporter::open(&settings.update_dir),
    };

    match cli.command {
        Some(Command::Update { dir }) => {
            let candidate = source::Dir {
                dir,
                keys: Vec::from(trusted_keys()?),
            }
            .read()?;
            let version = candidate.release.version;
            let request = Request {
                id: format!("cli-{}", chrono::Utc::now().format("%Y%m%d%H%M%S")),
                version: version.to_string(),
                requested_by: "command line".into(),
                requested_at: chrono::Utc::now(),
            };
            let _lock = Lock::acquire(&settings.state_dir)?;
            let state = updater.update(&request, &version).await;
            let run = updater
                .reporter
                .status
                .update
                .clone()
                .expect("an update ran");
            for step in &run.steps {
                println!("{}  {}", step.at.format("%H:%M:%S"), step.message);
            }
            println!(
                "{state:?}{}",
                run.error.map(|e| format!(": {e}")).unwrap_or_default()
            );
            if !matches!(
                state,
                monitorflow_common::server_update::UpdateState::Succeeded
            ) {
                std::process::exit(1);
            }
        }
        Some(Command::ReplaceSelf | Command::Restore { .. }) => unreachable!("handled above"),
        None => {
            tracing::info!(source = settings.source(), "serving update requests");
            let mut term = signal(SignalKind::terminate())?;
            let stop = async move {
                tokio::select! {
                    _ = term.recv() => {}
                    _ = tokio::signal::ctrl_c() => {}
                }
            };
            serve(&mut updater, &settings, stop).await;
        }
    }
    Ok(())
}
