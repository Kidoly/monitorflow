//! The MonitorFlow updater (ADR-0056): the only container of the compose
//! deployment with the Docker socket. It installs a server release when
//! the server asks for one through the update volume, after verifying it
//! with the release keys built in, and reports through `status.json`.
//!
//! The update volume's root belongs to root; the server writes only in
//! its `inbox/` subdirectory, so nothing it plants can redirect what the
//! updater writes.

pub mod env_file;
pub mod host;
pub mod project;
pub mod source;
pub mod update;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use monitorflow_common::server_update::{
    read_small, Request, HEARTBEAT, MAX_STATE_FILE, REQUEST_FILE,
};

use crate::update::Updater;

/// Subdirectory of the update volume the server writes requests in.
pub const INBOX: &str = "inbox";
/// The server's user in its image (`deploy/docker/server.Dockerfile`).
pub const SERVER_UID: u32 = 10001;
/// How often the inbox is read.
const POLL: Duration = Duration::from_secs(2);
/// How often the running version is asked again between updates.
const RUNNING_REFRESH: Duration = Duration::from_secs(300);

/// Settings, from the environment of the updater's container.
#[derive(Debug, Clone)]
pub struct Settings {
    pub update_dir: PathBuf,
    pub state_dir: PathBuf,
    pub backup_dir: PathBuf,
    /// Where compose mounts the project directory.
    pub deploy_dir: PathBuf,
    pub release_url: Option<String>,
    pub agent_download_url: String,
    /// How long the new containers may take to become healthy.
    pub timeout: Duration,
}

impl Settings {
    pub fn from_env() -> Result<Self, String> {
        let var = |key: &str| std::env::var(key).ok().filter(|v| !v.is_empty());
        let path = |key: &str, default: &str| PathBuf::from(var(key).unwrap_or(default.into()));
        let timeout = match var("MONITORFLOW_UPDATE_TIMEOUT") {
            None => 600,
            Some(text) => text
                .parse::<u64>()
                .ok()
                .filter(|s| (30..=3600).contains(s))
                .ok_or("MONITORFLOW_UPDATE_TIMEOUT must be 30 to 3600 seconds")?,
        };
        let check_url = |key: &str, url: &str| {
            if monitorflow_common::config::valid_mirror_url(url) {
                Ok(())
            } else {
                Err(format!(
                    "{key} must be an http or https URL without user, query or fragment"
                ))
            }
        };
        let release_url = var("MONITORFLOW_RELEASE_URL");
        if let Some(url) = &release_url {
            check_url("MONITORFLOW_RELEASE_URL", url)?;
        }
        let agent_download_url = var("MONITORFLOW_AGENT_DOWNLOAD_URL")
            .unwrap_or(monitorflow_common::config::DEFAULT_AGENT_DOWNLOAD_URL.into());
        check_url("MONITORFLOW_AGENT_DOWNLOAD_URL", &agent_download_url)?;
        Ok(Self {
            update_dir: path("MONITORFLOW_UPDATE_DIR", host::SERVER_UPDATE_DIR),
            state_dir: path(
                "MONITORFLOW_UPDATER_STATE_DIR",
                "/var/lib/monitorflow/updater",
            ),
            backup_dir: path("MONITORFLOW_BACKUP_DIR", "/backups"),
            deploy_dir: path("MONITORFLOW_DEPLOY_DIR", "/deploy"),
            release_url,
            agent_download_url,
            timeout: Duration::from_secs(timeout),
        })
    }

    pub fn source(&self) -> &str {
        monitorflow_common::server_update::source(
            self.release_url.as_deref(),
            &self.agent_download_url,
        )
    }

    /// Creates the volumes' directories: the update volume and the private
    /// ones for root, the inbox for the server's user.
    pub fn prepare_dirs(&self) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        for (dir, mode) in [
            (&self.update_dir, 0o755),
            (&self.state_dir, 0o700),
            (&self.backup_dir, 0o700),
        ] {
            std::fs::create_dir_all(dir)?;
            std::os::unix::fs::chown(dir, Some(0), Some(0))?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode))?;
        }
        let inbox = self.update_dir.join(INBOX);
        if std::fs::symlink_metadata(&inbox).is_ok_and(|m| !m.is_dir()) {
            std::fs::remove_file(&inbox)?;
        }
        std::fs::create_dir_all(&inbox)?;
        std::os::unix::fs::lchown(&inbox, Some(SERVER_UID), Some(SERVER_UID))?;
        std::fs::set_permissions(&inbox, std::fs::Permissions::from_mode(0o700))
    }
}

/// An exclusive lock on the state volume: one update at a time, between
/// the service and a `docker compose run updater update`.
pub struct Lock(#[allow(dead_code)] std::fs::File);

impl Lock {
    pub fn acquire(state_dir: &Path) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(state_dir.join("lock"))?;
        rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)?;
        Ok(Self(file))
    }
}

/// Takes the request out of the inbox, if there is one. `Err` carries why
/// a request was refused before any update began.
pub fn take_request(
    update_dir: &Path,
) -> Option<Result<(Request, monitorflow_common::release::Version), String>> {
    let path = update_dir.join(INBOX).join(REQUEST_FILE);
    let bytes = match read_small(&path, MAX_STATE_FILE) {
        Ok(None) => return None,
        Ok(Some(bytes)) => Ok(bytes),
        Err(e) => Err(format!("cannot read the request: {e}")),
    };
    // Consumed once, whatever it holds.
    let _ = std::fs::remove_file(&path);
    Some(bytes.and_then(|bytes| Request::parse(&bytes)))
}

/// Serves requests until `stop` resolves.
pub async fn serve(
    updater: &mut Updater<'_>,
    settings: &Settings,
    stop: impl std::future::Future<Output = ()>,
) {
    tokio::pin!(stop);
    {
        let _lock = Lock::acquire(&settings.state_dir);
        if let Some(state) = updater.recover().await {
            tracing::info!(?state, "interrupted update finished");
        }
    }
    let running = updater.running().await;
    updater.reporter.set_running(running);
    let (mut last_beat, mut last_running) = (Instant::now(), Instant::now());
    loop {
        match take_request(&settings.update_dir) {
            None => {}
            Some(Err(error)) => tracing::warn!(%error, "request refused"),
            Some(Ok((request, version))) => {
                match Lock::acquire(&settings.state_dir) {
                    Ok(_lock) => {
                        updater.update(&request, &version).await;
                    }
                    Err(e) => tracing::error!(error = %e, "cannot lock the state volume"),
                }
                last_running = Instant::now() - RUNNING_REFRESH;
            }
        }
        if last_running.elapsed() >= RUNNING_REFRESH {
            let running = updater.running().await;
            updater.reporter.set_running(running);
            (last_beat, last_running) = (Instant::now(), Instant::now());
        } else if last_beat.elapsed() >= HEARTBEAT {
            updater.reporter.write();
            last_beat = Instant::now();
        }
        tokio::select! {
            () = &mut stop => return,
            () = tokio::time::sleep(POLL) => {}
        }
    }
}
