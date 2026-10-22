//! Copies the `current` snapshot of one upstream mirror (ADR-0034), or
//! a snapshot pinned by an update campaign, which `current` keeps naming
//! the newest (ADR-0038).
//!
//! The snapshot is built in `<family>/<mirror>/.sync-<snapshot>/`, hidden
//! from the listener; the staging directory of another snapshot, left by
//! a failed sync, is renamed to it rather than left to fill the disk, so
//! a mirror has at most one. First the signed files (`Release`,
//! `repomd.xml`), then the index files they list, then the packages those
//! list, each checked against its size and checksum, the whole bounded by
//! `max_snapshot_bytes`. A file is taken, in this order, from the staging
//! directory (an interrupted sync, re-hashed), from an earlier complete
//! snapshot of the mirror with the same path, size and checksum
//! (hardlinked), or from the upstream. Once every file is there the relay
//! removes what the snapshot does not list, writes `.manifest` (with each
//! file's SHA-256), renames the directory to `<snapshot>` and swaps
//! `current`. Any failure leaves `current` as it was.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use monitorflow_common::config::DEFAULT_MIRROR_MAX_SNAPSHOT_BYTES;
use monitorflow_common::mirror::{
    is_snapshot_name, read_current, swap_current, write_atomically, Family, IndexMirror,
    PinnedSnapshot,
};
use serde::{Deserialize, Serialize};
use tokio::task::JoinSet;

use super::fetch::{hash_file, io, part_path, Expect, FetchError, Fetcher, IoError};
use super::meta::{Algorithm, Checksum, Entry, MetaError};
use super::{apt, dnf};

/// List of a complete snapshot's files, hidden from the listener.
pub const MANIFEST: &str = ".manifest";

/// Prefix of staging directories.
const STAGING: &str = ".sync-";

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
    #[error("the snapshot lists more than {limit} bytes (mirror.max_snapshot_bytes)")]
    TooLarge { limit: u64 },
    #[error(transparent)]
    Io(#[from] IoError),
    #[error("sync task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    files: Vec<Stored>,
}

/// A file of a manifest: path, size and SHA-256 (ADR-0034), and the
/// metadata's checksum when it uses another algorithm. Manifests written
/// before `listed` existed hold the metadata's checksum in `checksum`,
/// whatever its algorithm.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Stored {
    #[serde(flatten)]
    file: Entry,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    listed: Option<Checksum>,
}

impl Stored {
    /// `listed` is the metadata's entry, `sha256` the file's SHA-256.
    fn new(listed: &Entry, sha256: Checksum) -> Self {
        Self {
            file: Entry {
                path: listed.path.clone(),
                size: listed.size,
                checksum: sha256,
            },
            listed: (listed.checksum.algorithm != Algorithm::Sha256)
                .then(|| listed.checksum.clone()),
        }
    }

    fn checksum(&self, algorithm: Algorithm) -> Option<&Checksum> {
        [Some(&self.file.checksum), self.listed.as_ref()]
            .into_iter()
            .flatten()
            .find(|c| c.algorithm == algorithm)
    }
}

/// What a sync has gathered so far.
#[derive(Default)]
struct Progress {
    /// Every file the metadata lists, as listed.
    listed: BTreeMap<String, Entry>,
    /// The files in the staging directory, for the manifest.
    done: BTreeMap<String, Stored>,
    stats: Stats,
}

/// Copies snapshots from one upstream into `root` (the relay's
/// `mirror_dir`).
#[derive(Debug, Clone)]
pub struct Syncer {
    fetcher: Fetcher,
    root: PathBuf,
    max_downloads: usize,
    max_snapshot_bytes: u64,
}

/// One snapshot being copied.
struct Run {
    fetcher: Fetcher,
    /// `<family>/<mirror>/<snapshot>` on the upstream.
    upstream: String,
    staging: PathBuf,
    /// Files of earlier complete snapshots, by path.
    reuse: HashMap<String, (Stored, PathBuf)>,
}

/// How a file got into the staging directory, with its SHA-256.
enum Got {
    Kept(Checksum),
    Linked(Checksum),
    Downloaded(Entry),
    Missing,
}

impl Syncer {
    pub fn new(fetcher: Fetcher, root: PathBuf, max_downloads: usize) -> Self {
        Self {
            fetcher,
            root,
            max_downloads: max_downloads.max(1),
            max_snapshot_bytes: DEFAULT_MIRROR_MAX_SNAPSHOT_BYTES,
        }
    }

    /// Fails a sync whose snapshot lists more than `bytes` in all.
    pub fn with_max_snapshot_bytes(mut self, bytes: u64) -> Self {
        self.max_snapshot_bytes = bytes;
        self
    }

    /// `<root>/<family>/<mirror>`: the snapshots and `current`.
    pub fn mirror_dir(&self, family: Family, name: &str) -> PathBuf {
        self.root.join(family.as_str()).join(name)
    }

    /// Makes `current` of `mirror` name the snapshot the upstream's does,
    /// copying it first if needed.
    pub async fn sync(&self, mirror: &IndexMirror) -> Result<Outcome, SyncError> {
        self.copy(mirror, true).await
    }

    /// Copies the pinned snapshot `pinned` of `mirror` if the relay does
    /// not hold it yet, leaving `current` alone: `Unchanged` when held.
    pub async fn keep(
        &self,
        mirror: &IndexMirror,
        pinned: &PinnedSnapshot,
    ) -> Result<Outcome, SyncError> {
        self.copy(&mirror.at(pinned), false).await
    }

    /// Copies the snapshot `mirror.current` names, then points `current`
    /// at it if `swap`.
    async fn copy(&self, mirror: &IndexMirror, swap: bool) -> Result<Outcome, SyncError> {
        let dir = self.mirror_dir(mirror.family, &mirror.name);
        let snapshot = mirror.current.clone();
        tokio::fs::create_dir_all(&dir).await.map_err(io(&dir))?;
        if tokio::fs::try_exists(dir.join(&snapshot).join(MANIFEST))
            .await
            .unwrap_or(false)
        {
            if !swap {
                return Ok(Outcome::Unchanged);
            }
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

        let staging = dir.join(format!("{STAGING}{snapshot}"));
        let (d, s, st) = (dir.clone(), snapshot.clone(), staging.clone());
        let reuse = tokio::task::spawn_blocking(move || {
            adopt_staging(&d, &st)?;
            Ok::<_, SyncError>(earlier_files(&d, &s))
        })
        .await??;
        let run = Arc::new(Run {
            fetcher: self.fetcher.clone(),
            upstream: format!("{}/{}/{snapshot}", mirror.family, mirror.name),
            staging,
            reuse,
        });
        let mut progress = Progress::default();
        let packages = match mirror.family {
            Family::Apt => {
                let mut packages = Vec::new();
                for suite in &mirror.distributions {
                    let dists = apt::suite_dir(suite);
                    let roots = apt::ROOT_FILES.map(|f| format!("{dists}/{f}"));
                    run.roots(&roots, &mut progress).await?;
                    let release = run.staging.join(&dists).join("Release");
                    let text = tokio::fs::read_to_string(&release)
                        .await
                        .map_err(io(&release))?;
                    let listed = apt::release(suite, &text)?;
                    self.obtain(&run, listed.clone(), true, &mut progress)
                        .await?;
                    // Every variant the upstream serves is published, so
                    // the packages of each must be there.
                    for group in apt::package_lists(&listed) {
                        let served: Vec<String> = group
                            .into_iter()
                            .map(|e| e.path)
                            .filter(|p| progress.done.contains_key(p))
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
                run.roots(&dnf::ROOT_FILES, &mut progress).await?;
                let repomd = run.staging.join(dnf::REPOMD);
                let metadata = tokio::task::spawn_blocking(move || {
                    let file = File::open(&repomd).map_err(io(&repomd))?;
                    Ok::<_, SyncError>(dnf::repomd(BufReader::new(file))?)
                })
                .await??;
                let listed = metadata.iter().map(|m| m.entry.clone()).collect();
                self.obtain(&run, listed, false, &mut progress).await?;
                let primary = metadata
                    .into_iter()
                    .find(|m| m.kind == "primary")
                    .ok_or_else(|| SyncError::Missing("primary metadata".into()))?
                    .entry;
                let path = run.staging.join(&primary.path);
                tokio::task::spawn_blocking(move || dnf::primary(&primary.path, &path)).await??
            }
        };
        self.obtain(&run, packages, false, &mut progress).await?;

        let staging = run.staging.clone();
        let stats = progress.stats;
        tokio::task::spawn_blocking(move || {
            publish(&dir, &staging, &snapshot, progress.done, swap)
        })
        .await??;
        Ok(Outcome::Synced(stats))
    }

    /// Gets every entry into the staging directory, `max_downloads` at a
    /// time, and records it in `progress`. Entries missing upstream are
    /// skipped if `optional`, else fail the sync. Fails before fetching
    /// anything if the snapshot would list more than `max_snapshot_bytes`.
    async fn obtain(
        &self,
        run: &Arc<Run>,
        entries: Vec<Entry>,
        optional: bool,
        progress: &mut Progress,
    ) -> Result<(), SyncError> {
        let mut todo = Vec::new();
        for entry in entries {
            match progress.listed.get(&entry.path) {
                Some(known) if *known == entry => continue,
                Some(_) => return Err(SyncError::Conflict(entry.path)),
                None => {}
            }
            progress.listed.insert(entry.path.clone(), entry.clone());
            todo.push(entry);
        }
        let total = progress
            .listed
            .values()
            .fold(0u64, |total, e| total.saturating_add(e.size));
        if total > self.max_snapshot_bytes {
            return Err(SyncError::TooLarge {
                limit: self.max_snapshot_bytes,
            });
        }
        let stats = &mut progress.stats;
        let mut tasks = JoinSet::new();
        let mut queue = todo.into_iter();
        loop {
            while tasks.len() < self.max_downloads {
                let Some(entry) = queue.next() else { break };
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
            let sha256 = match got? {
                Got::Kept(sha256) => {
                    stats.kept += 1;
                    sha256
                }
                Got::Linked(sha256) => {
                    stats.linked += 1;
                    sha256
                }
                Got::Downloaded(got) => {
                    stats.downloaded += 1;
                    stats.bytes += got.size;
                    got.checksum
                }
                Got::Missing if optional => {
                    progress.listed.remove(&entry.path);
                    continue;
                }
                Got::Missing => return Err(SyncError::Missing(entry.path)),
            };
            let stored = Stored::new(&entry, sha256);
            progress.done.insert(entry.path, stored);
        }
    }
}

impl Run {
    /// Downloads the signed files; the first one is required.
    async fn roots(
        &self,
        paths: &[impl AsRef<str>],
        progress: &mut Progress,
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
                    progress.stats.downloaded += 1;
                    progress.stats.bytes += got.size;
                    let entry = Entry {
                        path: path.to_owned(),
                        ..got
                    };
                    let stored = Stored::new(&entry, entry.checksum.clone());
                    progress.listed.insert(entry.path.clone(), entry);
                    progress.done.insert(stored.file.path.clone(), stored);
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
                if let Some(sha256) = verify(&dest, entry).await.map_err(io(&dest))? {
                    remove_part(&dest).await;
                    return Ok(Got::Kept(sha256));
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
            if let Some(sha256) = reusable(known, source, entry).await {
                if let Some(parent) = dest.parent() {
                    tokio::fs::create_dir_all(parent)
                        .await
                        .map_err(io(parent))?;
                }
                if tokio::fs::hard_link(source, &dest).await.is_ok() {
                    remove_part(&dest).await;
                    return Ok(Got::Linked(sha256));
                }
            }
        }
        let url = format!("{}/{}", self.upstream, entry.path);
        Ok(
            match self.fetcher.file(&url, &dest, Expect::Entry(entry)).await? {
                Some(got) => Got::Downloaded(got),
                None => Got::Missing,
            },
        )
    }
}

/// The SHA-256 of the file at `path` if it matches `entry`.
async fn verify(path: &Path, entry: &Entry) -> std::io::Result<Option<Checksum>> {
    let (path, algorithm) = (path.to_owned(), entry.checksum.algorithm);
    let (size, listed, sha256) = tokio::task::spawn_blocking(move || hash_file(&path, algorithm))
        .await
        .map_err(std::io::Error::other)??;
    Ok((size == entry.size && listed == entry.checksum).then_some(sha256))
}

/// The SHA-256 of `source`, a file of an earlier snapshot, if it matches
/// `entry`. Compared through the manifest when it holds a checksum with
/// the metadata's algorithm and the SHA-256; otherwise (older manifests,
/// a repository that changed algorithm) the local file is hashed, which
/// still beats fetching it again.
async fn reusable(known: &Stored, source: &Path, entry: &Entry) -> Option<Checksum> {
    if known.file.size != entry.size {
        return None;
    }
    match known.checksum(entry.checksum.algorithm) {
        Some(sum) if *sum != entry.checksum => return None,
        Some(_) => {
            if let Some(sha256) = known.checksum(Algorithm::Sha256) {
                return Some(sha256.clone());
            }
        }
        None => {}
    }
    verify(source, entry).await.ok().flatten()
}

async fn remove_part(dest: &Path) {
    let _ = tokio::fs::remove_file(part_path(dest)).await;
}

/// Makes `staging` the only staging directory in `dir`: if it does not
/// exist, the newest staging directory of another snapshot (left by a
/// failed sync) is renamed to it, since consecutive snapshots share most
/// files, which are re-hashed before being kept; any other is removed.
/// Blocking.
fn adopt_staging(dir: &Path, staging: &Path) -> Result<(), SyncError> {
    let mut others = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(io(dir))? {
        let entry = entry.map_err(io(dir))?;
        let path = entry.path();
        if path == staging || !entry.file_name().to_string_lossy().starts_with(STAGING) {
            continue;
        }
        // Not followed if a link.
        let meta = entry.metadata().map_err(io(&path))?;
        others.push((meta.is_dir(), meta.modified().ok(), path));
    }
    // Directories last, newest last.
    others.sort();
    if std::fs::symlink_metadata(staging).is_err() {
        if let Some((true, _, newest)) = others.last() {
            std::fs::rename(newest, staging).map_err(io(staging))?;
            others.pop();
        }
    }
    for (is_dir, _, path) in others {
        let removed = if is_dir {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        removed.map_err(io(&path))?;
    }
    Ok(())
}

/// Files of the complete snapshots in `dir` other than `except`, by path.
/// Unreadable manifests are skipped: their files are fetched again.
/// Blocking.
fn earlier_files(dir: &Path, except: &str) -> HashMap<String, (Stored, PathBuf)> {
    let mut files = HashMap::new();
    for snapshot in complete_snapshots(dir) {
        if snapshot == except {
            continue;
        }
        let base = dir.join(&snapshot);
        let Ok(manifest) = read_manifest(&base) else {
            continue;
        };
        for stored in manifest.files {
            let source = base.join(&stored.file.path);
            files
                .entry(stored.file.path.clone())
                .or_insert((stored, source));
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

/// Removes the files of `staging` that `files` does not list, then writes
/// the manifest, renames `staging` to `<dir>/<snapshot>` and, if `swap`,
/// points `current` at it. Blocking.
fn publish(
    dir: &Path,
    staging: &Path,
    snapshot: &str,
    files: BTreeMap<String, Stored>,
    swap: bool,
) -> Result<(), SyncError> {
    remove_unlisted(staging, staging, &files)?;
    let manifest = Manifest {
        files: files.into_values().collect(),
    };
    let path = staging.join(MANIFEST);
    let json = serde_json::to_vec(&manifest).map_err(|e| io(&path)(e.into()))?;
    write_atomically(&path, &json).map_err(io(&path))?;
    let target = dir.join(snapshot);
    // Only a rename creates it, so without a manifest it is debris.
    match std::fs::remove_dir_all(&target) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(io(&target)(e).into()),
        _ => {}
    }
    std::fs::rename(staging, &target).map_err(io(&target))?;
    if swap {
        swap_current(dir, snapshot).map_err(io(dir))?;
    }
    Ok(())
}

/// Removes every file under `dir` whose path relative to `base` is not in
/// `files`, and the directories left empty: what an adopted staging
/// directory held for another snapshot, and `.part` debris. Blocking.
fn remove_unlisted(
    base: &Path,
    dir: &Path,
    files: &BTreeMap<String, Stored>,
) -> Result<(), IoError> {
    for entry in std::fs::read_dir(dir).map_err(io(dir))? {
        let path = entry.map_err(io(dir))?.path();
        let meta = std::fs::symlink_metadata(&path).map_err(io(&path))?;
        if meta.is_dir() {
            remove_unlisted(base, &path, files)?;
            if std::fs::read_dir(&path)
                .map_err(io(&path))?
                .next()
                .is_none()
            {
                std::fs::remove_dir(&path).map_err(io(&path))?;
            }
            continue;
        }
        let listed = path
            .strip_prefix(base)
            .ok()
            .and_then(Path::to_str)
            .is_some_and(|rel| files.contains_key(rel));
        if !listed {
            std::fs::remove_file(&path).map_err(io(&path))?;
        }
    }
    Ok(())
}
