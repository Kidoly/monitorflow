//! The relay's mirror sync loop (ADR-0034): at start and then every
//! `mirror.sync_interval`, copy the public key, read the upstream's
//! `/index.json` and sync each listed mirror in turn, then prune its old
//! snapshots. After each mirror it reports what the relay serves to the
//! control stream (ADR-0037). Failures are logged with the time of the last success and
//! retried at the next cycle; they never stop the relay. A shutdown
//! interrupts a sync, which resumes from its staging directory later.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use monitorflow_common::client::shutdown::Shutdown;
use monitorflow_common::config::RelayConfig;
use monitorflow_common::mirror::{write_atomically, Family, Index, IndexMirror, Served};
use monitorflow_common::proto::v1::RelayMirrors;
use tokio::sync::watch;

use super::fetch::{io, FetchError, Fetcher};
use super::sync::{Outcome, Syncer};
use super::{report, retention};

/// Largest index and public key accepted.
const INDEX_LIMIT: u64 = 4 * 1024 * 1024;
const KEY_LIMIT: u64 = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct MirrorSettings {
    /// The upstream's mirror listener, `http://host:7480`.
    pub upstream_url: String,
    /// The relay's `mirror_dir`.
    pub root: PathBuf,
    pub interval: Duration,
    /// Mirror names to copy; empty is all.
    pub mirrors: Vec<String>,
    pub keep_snapshots: usize,
    pub max_downloads: usize,
    pub request_timeout: Duration,
    /// Largest snapshot copied, in bytes.
    pub max_snapshot_bytes: u64,
}

impl MirrorSettings {
    pub fn from_config(config: &RelayConfig) -> Self {
        let mirror = &config.mirror;
        Self {
            upstream_url: config.mirror_upstream_url(),
            root: config.mirror_dir(),
            interval: Duration::from_secs(mirror.sync_interval),
            mirrors: mirror.mirrors.clone(),
            keep_snapshots: mirror.keep_snapshots,
            max_downloads: mirror.max_downloads,
            request_timeout: Duration::from_secs(mirror.request_timeout),
            max_snapshot_bytes: mirror.max_snapshot_bytes,
        }
    }
}

/// What one cycle did, for tests and the log.
#[derive(Debug, Default)]
pub struct Report {
    /// Set when the index could not be read; nothing was synced.
    pub index_error: Option<String>,
    pub mirrors: Vec<(IndexMirror, Result<Outcome, String>)>,
}

pub struct MirrorTask {
    settings: MirrorSettings,
    fetcher: Fetcher,
    syncer: Syncer,
    served: Served,
    /// Last successful sync of each mirror.
    last_success: HashMap<(Family, String), Instant>,
    /// The relay's `mirror.public_url` and where to publish its reports.
    reports: Option<(String, watch::Sender<RelayMirrors>)>,
}

impl MirrorTask {
    pub fn new(settings: MirrorSettings) -> Result<Self, reqwest::Error> {
        let fetcher = Fetcher::new(&settings.upstream_url, settings.request_timeout)?;
        let syncer = Syncer::new(
            fetcher.clone(),
            settings.root.clone(),
            settings.max_downloads,
        )
        .with_max_snapshot_bytes(settings.max_snapshot_bytes);
        Ok(Self {
            served: Served::under(&settings.root),
            settings,
            fetcher,
            syncer,
            last_success: HashMap::new(),
            reports: None,
        })
    }

    /// Publishes what the relay serves, reachable at `public_url`, on
    /// `reports` at start and after each mirror.
    pub fn with_reports(
        mut self,
        public_url: String,
        reports: watch::Sender<RelayMirrors>,
    ) -> Self {
        self.reports = Some((public_url, reports));
        self
    }

    /// Cycles until `shutdown` fires.
    pub async fn run(mut self, mut shutdown: Shutdown) {
        tracing::info!(upstream = %self.settings.upstream_url, interval = ?self.settings.interval, "mirror sync started");
        // Snapshots kept from before a restart are served right away.
        self.report().await;
        loop {
            tokio::select! {
                _ = self.cycle() => {}
                () = shutdown.wait() => break,
            }
            tokio::select! {
                () = tokio::time::sleep(self.settings.interval) => {}
                () = shutdown.wait() => break,
            }
        }
        tracing::info!("mirror sync stopped");
    }

    /// One pass over the upstream's index.
    pub async fn cycle(&mut self) -> Report {
        if let Err(error) = self.copy_key().await {
            tracing::warn!(%error, "cannot copy the mirror signing key");
        }
        let index = match self.index().await {
            Ok(index) => index,
            Err(error) => {
                tracing::warn!(upstream = %self.settings.upstream_url, %error, "cannot read the upstream mirror index");
                return Report {
                    index_error: Some(error),
                    ..Report::default()
                };
            }
        };
        let mut report = Report::default();
        for mirror in index.mirrors {
            if !self.settings.mirrors.is_empty() && !self.settings.mirrors.contains(&mirror.name) {
                continue;
            }
            let result = self.sync(&mirror).await;
            report.mirrors.push((mirror, result));
            self.report().await;
        }
        report
    }

    /// Scans the served tree and publishes it if it changed.
    async fn report(&self) {
        let Some((url, reports)) = &self.reports else {
            return;
        };
        let (served, url) = (self.served.clone(), url.clone());
        match tokio::task::spawn_blocking(move || report::scan(&served, &url)).await {
            Ok(Ok(scanned)) => {
                reports.send_if_modified(|last| {
                    let changed = *last != scanned;
                    *last = scanned;
                    changed
                });
            }
            Ok(Err(error)) => tracing::warn!(%error, "cannot list the served mirrors"),
            Err(error) => tracing::warn!(%error, "mirror listing task failed"),
        }
    }

    async fn index(&self) -> Result<Index, String> {
        let bytes = self
            .fetcher
            .bytes("index.json", INDEX_LIMIT)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("the upstream has no index.json; is it older than the relay?")?;
        Index::parse(&bytes).map_err(|e| e.to_string())
    }

    /// Copies `/keys/monitorflow-archive.asc` if the upstream has one.
    async fn copy_key(&self) -> Result<(), FetchError> {
        let path = self.served.public_key.clone();
        let Some(key) = self
            .fetcher
            .bytes("keys/monitorflow-archive.asc", KEY_LIMIT)
            .await?
        else {
            return Ok(());
        };
        if tokio::fs::read(&path).await.ok().as_deref() == Some(key.as_slice()) {
            return Ok(());
        }
        let written = path.clone();
        tokio::task::spawn_blocking(move || {
            if let Some(keys) = written.parent() {
                std::fs::create_dir_all(keys)?;
            }
            write_atomically(&written, &key)
        })
        .await
        .map_err(std::io::Error::other)
        .and_then(|r| r)
        .map_err(io(&path))?;
        tracing::info!(path = %path.display(), "mirror signing key updated");
        Ok(())
    }

    async fn sync(&mut self, mirror: &IndexMirror) -> Result<Outcome, String> {
        let key = (mirror.family, mirror.name.clone());
        let started = Instant::now();
        let outcome = match self.syncer.sync(mirror).await {
            Ok(outcome) => outcome,
            Err(error) => {
                let last_success = match self.last_success.get(&key) {
                    Some(at) => format!("{}s ago", at.elapsed().as_secs()),
                    None => "never since start".to_owned(),
                };
                tracing::warn!(family = %mirror.family, mirror = %mirror.name, snapshot = %mirror.current, %error, %last_success, "mirror sync failed; current left as it was");
                return Err(error.to_string());
            }
        };
        self.last_success.insert(key, Instant::now());
        match &outcome {
            Outcome::Unchanged => {
                tracing::debug!(family = %mirror.family, mirror = %mirror.name, snapshot = %mirror.current, "mirror up to date")
            }
            Outcome::Swapped => {
                tracing::info!(family = %mirror.family, mirror = %mirror.name, snapshot = %mirror.current, "mirror current moved to a held snapshot")
            }
            Outcome::Synced(stats) => {
                tracing::info!(family = %mirror.family, mirror = %mirror.name, snapshot = %mirror.current, downloaded = stats.downloaded, bytes = stats.bytes, linked = stats.linked, kept = stats.kept, elapsed = ?started.elapsed(), "mirror synced")
            }
        }
        let dir = self.syncer.mirror_dir(mirror.family, &mirror.name);
        let keep = self.settings.keep_snapshots;
        match tokio::task::spawn_blocking(move || retention::prune(&dir, keep)).await {
            Ok(Ok(removed)) if !removed.is_empty() => {
                tracing::info!(mirror = %mirror.name, ?removed, "old mirror snapshots removed")
            }
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                tracing::warn!(mirror = %mirror.name, %error, "cannot remove old mirror snapshots")
            }
            Err(error) => tracing::warn!(%error, "mirror retention task failed"),
        }
        Ok(outcome)
    }
}
