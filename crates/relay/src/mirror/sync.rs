//! Copies the `current` snapshot of one upstream mirror (ADR-0034).
//!
//! The snapshot is built in `<family>/<mirror>/.sync-<snapshot>/`, hidden
//! from the listener: first the signed files (`Release`, `repomd.xml`),
//! then the index files they list, then the packages those list, each
//! checked against its size and checksum. A file is taken, in this order,
//! from the staging directory (an interrupted sync, re-hashed), from an
//! earlier complete snapshot of the mirror with the same path, size and
//! checksum (hardlinked), or from the upstream. Once every file is there
//! the relay writes `.manifest`, renames the directory to `<snapshot>` and
//! swaps `current`. Any failure leaves `current` as it was.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use monitorflow_common::mirror::{
    is_snapshot_name, read_current, swap_current, Family, IndexMirror,
};
use serde::{Deserialize, Serialize};
use tokio::task::JoinSet;

use super::fetch::{hash_file, part_path, Expect, FetchError, Fetcher};
use super::meta::{Entry, MetaError};
use super::{apt, dnf};

/// List of a complete snapshot's files, hidden from the listener.
pub const MANIFEST: &str = ".manifest";

/// Largest signed root file (`Release`, `repomd.xml` and their
/// signatures), whose size nothing gives in advance.
const ROOT_LIMIT: u64 = 64 * 1024 * 1024;

/// What one sync did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    /// Files fetched from the upstream, and their bytes.
    pub downloaded: u64,
    pub bytes: u64,
    /// Files hardlinked from an earlier snapshot.
    pub linked: u64,
    /// Files left by an interrupted sync that matched.
    pub kept: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The snapshot was complete and `current` named it already.
    Unchanged,
    /// The snapshot was complete; `current` now names it.
    Swapped,
    /// The snapshot was copied and `current` now names it.
    Synced(Stats),
}

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error(transparent)]
    Fetch(#[from] FetchError),
    #[error(transparent)]
    Meta(#[from] MetaError),
    #[error("{0} is listed but missing upstream")]
    Missing(String),
    #[error("{0} is listed twice with different sizes or checksums")]
    Conflict(String),
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("sync task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> SyncError + '_ {
    move |source| SyncError::Io {
        path: path.to_owned(),
        source,
    }
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    files: Vec<Entry>,
}

/// Copies snapshots from one upstream into `root` (the relay's
/// `mirror_dir`).
#[derive(Debug, Clone)]
pub struct Syncer {
    fetcher: Fetcher,
    root: PathBuf,
    max_downloads: usize,
}

/// One snapshot being copied.
struct Run {
    fetcher: Fetcher,
    /// `<family>/<mirror>/<snapshot>` on the upstream.
    upstream: String,
    staging: PathBuf,
    /// Files of earlier complete snapshots, by path.
    reuse: HashMap<String, (Entry, PathBuf)>,
}

enum Got {
    Kept,
    Linked,
    Downloaded(u64),
    Missing,
}

impl Syncer {
    pub fn new(fetcher: Fetcher, root: PathBuf, max_downloads: usize) -> Self {
        Self {
            fetcher,
            root,
            max_downloads: max_downloads.max(1),
        }
    }

    /// `<root>/<family>/<mirror>`: the snapshots and `current`.
    pub fn mirror_dir(&self, family: Family, name: &str) -> PathBuf {
        self.root.join(family.as_str()).join(name)
    }

    /// Makes `current` of `mirror` name the snapshot the upstream's does,
    /// copying it first if needed.
    pub async fn sync(&self, mirror: &IndexMirror) -> Result<Outcome, SyncError> {
        let dir = self.mirror_dir(mirror.family, &mirror.name);
        let snapshot = mirror.current.clone();
        tokio::fs::create_dir_all(&dir).await.map_err(io(&dir))?;
        if tokio::fs::try_exists(dir.join(&snapshot).join(MANIFEST))
            .await
            .unwrap_or(false)
        {
            let (d, s) = (dir.clone(), snapshot.clone());
            return tokio::task::spawn_blocking(move || {
                if read_current(&d).as_deref() == Some(s.as_str()) {
                    return Ok(Outcome::Unchanged);
                }
                swap_current(&d, &s).map_err(io(&d))?;
                Ok(Outcome::Swapped)
            })
            .await?;
        }

        let (d, s) = (dir.clone(), snapshot.clone());
        let reuse = tokio::task::spawn_blocking(move || earlier_files(&d, &s)).await?;
        let run = Arc::new(Run {
            fetcher: self.fetcher.clone(),
            upstream: format!("{}/{}/{snapshot}", mirror.family, mirror.name),
            staging: dir.join(format!(".sync-{snapshot}")),
            reuse,
        });
        let mut files = BTreeMap::new();
        let mut stats = Stats::default();
        let packages = match mirror.family {
            Family::Apt => {
                let mut packages = Vec::new();
                for suite in &mirror.distributions {
                    let dists = apt::suite_dir(suite);
                    let roots = apt::ROOT_FILES.map(|f| format!("{dists}/{f}"));
                    run.roots(&roots, &mut files, &mut stats).await?;
                    let release = run.staging.join(&dists).join("Release");
                    let text = tokio::fs::read_to_string(&release)
                        .await
                        .map_err(io(&release))?;
                    let listed = apt::release(suite, &text)?;
                    self.obtain(&run, listed.clone(), true, &mut files, &mut stats)
                        .await?;
                    // Every variant the upstream serves is published, so
                    // the packages of each must be there.
                    for group in apt::package_lists(&listed) {
                        let served: Vec<String> = group
                            .into_iter()
                            .map(|e| e.path)
                            .filter(|p| files.contains_key(p))
                            .collect();
                        if served.is_empty() {
                            tracing::warn!(mirror = %mirror.name, suite, "a package list is listed but missing upstream in every compression");
                        }
                        for name in served {
                            let path = run.staging.join(&name);
                            packages.extend(
                                tokio::task::spawn_blocking(move || {
                                    apt::read_packages(&name, &path)
                                })
                                .await??,
                            );
                        }
                    }
                }
                packages
            }
            Family::Dnf => {
                run.roots(&dnf::ROOT_FILES, &mut files, &mut stats).await?;
                let repomd = run.staging.join(dnf::REPOMD);
                let metadata = tokio::task::spawn_blocking(move || {
                    let file = File::open(&repomd).map_err(io(&repomd))?;
                    Ok::<_, SyncError>(dnf::repomd(BufReader::new(file))?)
                })
                .await??;
                let listed = metadata.iter().map(|m| m.entry.clone()).collect();
                self.obtain(&run, listed, false, &mut files, &mut stats)
                    .await?;
                let primary = metadata
                    .into_iter()
                    .find(|m| m.kind == "primary")
                    .ok_or_else(|| SyncError::Missing("primary metadata".into()))?
                    .entry;
                let path = run.staging.join(&primary.path);
                tokio::task::spawn_blocking(move || dnf::primary(&primary.path, &path)).await??
            }
        };
        self.obtain(&run, packages, false, &mut files, &mut stats)
            .await?;

        let staging = run.staging.clone();
        tokio::task::spawn_blocking(move || publish(&dir, &staging, &snapshot, files)).await??;
        Ok(Outcome::Synced(stats))
    }

    /// Gets every entry into the staging directory, `max_downloads` at a
    /// time, and records it in `files`. Entries missing upstream are
    /// skipped if `optional`, else fail the sync.
    async fn obtain(
        &self,
        run: &Arc<Run>,
        entries: Vec<Entry>,
        optional: bool,
        files: &mut BTreeMap<String, Entry>,
        stats: &mut Stats,
    ) -> Result<(), SyncError> {
        let mut tasks = JoinSet::new();
        let mut queue = entries.into_iter();
        loop {
            while tasks.len() < self.max_downloads {
                let Some(entry) = queue.next() else { break };
                match files.get(&entry.path) {
                    Some(known) if *known == entry => continue,
                    Some(_) => return Err(SyncError::Conflict(entry.path)),
                    None => {}
                }
                files.insert(entry.path.clone(), entry.clone());
                let run = run.clone();
                tasks.spawn(async move {
                    let got = run.get(&entry).await;
                    (entry, got)
                });
            }
            // An error drops `tasks`, which aborts the downloads in flight.
            let Some(done) = tasks.join_next().await else {
                return Ok(());
            };
            let (entry, got) = done?;
            match got? {
                Got::Kept => stats.kept += 1,
                Got::Linked => stats.linked += 1,
                Got::Downloaded(bytes) => {
                    stats.downloaded += 1;
                    stats.bytes += bytes;
                }
                Got::Missing if optional => {
                    files.remove(&entry.path);
                }
                Got::Missing => return Err(SyncError::Missing(entry.path)),
            }
        }
    }
}

impl Run {
    /// Downloads the signed files; the first one is required.
    async fn roots(
        &self,
        paths: &[impl AsRef<str>],
        files: &mut BTreeMap<String, Entry>,
        stats: &mut Stats,
    ) -> Result<(), SyncError> {
        for (i, path) in paths.iter().enumerate() {
            let path = path.as_ref();
            let url = format!("{}/{path}", self.upstream);
            let dest = self.staging.join(path);
            match self
                .fetcher
                .file(&url, &dest, Expect::AtMost(ROOT_LIMIT))
                .await?
            {
                Some(got) => {
                    stats.downloaded += 1;
                    stats.bytes += got.size;
                    let entry = Entry {
                        path: path.to_owned(),
                        ..got
                    };
                    files.insert(entry.path.clone(), entry);
                }
                None if i == 0 => return Err(SyncError::Missing(path.to_owned())),
                None => {}
            }
        }
        Ok(())
    }

    async fn get(&self, entry: &Entry) -> Result<Got, SyncError> {
        let dest = self.staging.join(&entry.path);
        if let Ok(meta) = tokio::fs::symlink_metadata(&dest).await {
            if meta.is_file() && meta.len() == entry.size {
                let (path, algorithm) = (dest.clone(), entry.checksum.algorithm);
                let (_, sum) = tokio::task::spawn_blocking(move || hash_file(&path, algorithm))
                    .await?
                    .map_err(io(&dest))?;
                if sum == entry.checksum {
                    remove_part(&dest).await;
                    return Ok(Got::Kept);
                }
            }
            let removed = if meta.is_dir() {
                tokio::fs::remove_dir_all(&dest).await
            } else {
                tokio::fs::remove_file(&dest).await
            };
            removed.map_err(io(&dest))?;
        }
        if let Some((known, source)) = self.reuse.get(&entry.path) {
            if known == entry {
                if let Some(parent) = dest.parent() {
                    tokio::fs::create_dir_all(parent)
                        .await
                        .map_err(io(parent))?;
                }
                if tokio::fs::hard_link(source, &dest).await.is_ok() {
                    remove_part(&dest).await;
                    return Ok(Got::Linked);
                }
            }
        }
        let url = format!("{}/{}", self.upstream, entry.path);
        Ok(
            match self.fetcher.file(&url, &dest, Expect::Entry(entry)).await? {
                Some(got) => Got::Downloaded(got.size),
                None => Got::Missing,
            },
        )
    }
}

async fn remove_part(dest: &Path) {
    let _ = tokio::fs::remove_file(part_path(dest)).await;
}

/// Files of the complete snapshots in `dir` other than `except`, by path.
/// Unreadable manifests are skipped: their files are fetched again.
/// Blocking.
fn earlier_files(dir: &Path, except: &str) -> HashMap<String, (Entry, PathBuf)> {
    let mut files = HashMap::new();
    for snapshot in complete_snapshots(dir) {
        if snapshot == except {
            continue;
        }
        let base = dir.join(&snapshot);
        let Ok(manifest) = read_manifest(&base) else {
            continue;
        };
        for entry in manifest.files {
            let source = base.join(&entry.path);
            files.entry(entry.path.clone()).or_insert((entry, source));
        }
    }
    files
}

/// Names of the snapshots in `dir` that have a manifest. Blocking.
pub fn complete_snapshots(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|name| is_snapshot_name(name) && dir.join(name).join(MANIFEST).is_file())
        .collect()
}

fn read_manifest(snapshot: &Path) -> Result<Manifest, std::io::Error> {
    let bytes = std::fs::read(snapshot.join(MANIFEST))?;
    serde_json::from_slice(&bytes).map_err(std::io::Error::other)
}

/// Writes the manifest, renames `staging` to `<dir>/<snapshot>` and points
/// `current` at it. Blocking.
fn publish(
    dir: &Path,
    staging: &Path,
    snapshot: &str,
    files: BTreeMap<String, Entry>,
) -> Result<(), SyncError> {
    let manifest = Manifest {
        files: files.into_values().collect(),
    };
    let path = staging.join(MANIFEST);
    let json = serde_json::to_vec(&manifest).map_err(|e| io(&path)(e.into()))?;
    std::fs::write(&path, json).map_err(io(&path))?;
    let target = dir.join(snapshot);
    // Only a rename creates it, so without a manifest it is debris.
    match std::fs::remove_dir_all(&target) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(io(&target)(e)),
        _ => {}
    }
    std::fs::rename(staging, &target).map_err(io(&target))?;
    swap_current(dir, snapshot).map_err(io(dir))
}
