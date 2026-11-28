//! Packages of on-demand snapshots, fetched on their first request
//! (ADR-0142). The listener calls [`OnDemand::package`] for a path that is
//! not in a snapshot's tree; it answers from the content store, or fetches
//! the file from the snapshot's upstream, checks it against the package
//! list, keeps it and answers with it.
//!
//! Only a path the package list names is fetched, so the listener is no
//! open proxy. Concurrent requests for one file share one download, which
//! runs in a task of its own: a client going away does not stop it. At
//! most [`MAX_DOWNLOADS`] run at a time; the others wait their turn.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use reqwest::StatusCode;
use tokio::sync::{watch, Semaphore};

use super::origin::{Origin, ORIGIN};
use super::packages::{PackageList, PACKAGES};
use super::repo::fetch::{Expect, FetchError, Fetcher};
use super::repo::meta::{Algorithm, Checksum, Package};
use super::serve::SOFTWARE_DIR;
use super::store::{self, Store};
use super::Family;
use crate::software::MAX_MSI_BYTES;

/// Downloads toward upstreams at a time, per process.
pub const MAX_DOWNLOADS: usize = 8;

/// Connecting, and every wait for more data from an upstream.
pub const TIMEOUT: Duration = Duration::from_secs(600);

/// Package lists kept in memory, the most recently used.
const LISTS: usize = 32;

/// Why a package could not be served.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DemandError {
    /// Neither the upstream nor any archive has it.
    #[error("missing upstream")]
    Missing,
    /// What came does not match the package list, or the upstream failed.
    #[error("{0}")]
    Upstream(String),
    #[error("{0}")]
    Local(String),
}

/// Where packages missing from the store come from.
#[derive(Debug, Clone)]
pub enum Source {
    /// The server: the snapshot's `.origin`, its upstream then its
    /// archives, each at the package's path. `client` makes the requests.
    Origin(Fetcher),
    /// A relay: its upstream's mirror listener, at the same URL path as
    /// the request, which fetches on a miss in turn.
    Listener(Fetcher),
}

/// A package to fetch.
struct Wanted {
    family: Family,
    mirror: String,
    snapshot: String,
    snapshot_dir: PathBuf,
    package: Package,
}

/// What to fetch: a package of a snapshot, or a file of the software
/// catalogue by its SHA-256 (ADR-0188).
enum Fetch {
    Package(Wanted),
    Software(Checksum),
}

impl Fetch {
    fn checksum(&self) -> &Checksum {
        match self {
            Self::Package(wanted) => &wanted.package.entry.checksum,
            Self::Software(checksum) => checksum,
        }
    }
}

type Outcome = Option<Result<PathBuf, DemandError>>;

struct Inner {
    store: Store,
    source: Source,
    permits: Semaphore,
    inflight: Mutex<HashMap<Checksum, watch::Receiver<Outcome>>>,
    /// By snapshot directory: its list's modification time and the list.
    lists: Mutex<Vec<(PathBuf, SystemTime, Arc<PackageList>)>>,
    /// Called with the stored size after each package kept.
    kept: Option<Box<dyn Fn(u64) + Send + Sync>>,
}

/// Fetches on-demand packages; clones share their downloads.
#[derive(Clone)]
pub struct OnDemand(Arc<Inner>);

impl std::fmt::Debug for OnDemand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnDemand")
            .field("store", &self.0.store)
            .finish_non_exhaustive()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl OnDemand {
    pub fn new(store: Store, source: Source) -> Self {
        Self::with_hook(store, source, None)
    }

    /// As [`Self::new`], calling `kept` with the size of each package kept,
    /// which is how eviction hears that the store grew.
    pub fn with_hook(
        store: Store,
        source: Source,
        kept: Option<Box<dyn Fn(u64) + Send + Sync>>,
    ) -> Self {
        Self(Arc::new(Inner {
            store,
            source,
            permits: Semaphore::new(MAX_DOWNLOADS),
            inflight: Mutex::new(HashMap::new()),
            lists: Mutex::new(Vec::new()),
            kept,
        }))
    }

    pub fn store(&self) -> &Store {
        &self.0.store
    }

    /// The package list of the snapshot directory `dir`, `None` when it
    /// is not an on-demand snapshot. Kept in memory while unchanged.
    /// Blocking.
    pub fn list(&self, dir: &Path) -> std::io::Result<Option<Arc<PackageList>>> {
        let path = dir.join(PACKAGES);
        let modified = match std::fs::metadata(&path) {
            Ok(meta) => meta.modified()?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        {
            let mut lists = lock(&self.0.lists);
            if let Some(i) = lists
                .iter()
                .position(|(d, m, _)| d == dir && *m == modified)
            {
                let found = lists.remove(i);
                let list = found.2.clone();
                lists.push(found);
                return Ok(Some(list));
            }
        }
        let Some(list) = PackageList::read(&path)? else {
            return Ok(None);
        };
        let list = Arc::new(list);
        let mut lists = lock(&self.0.lists);
        lists.retain(|(d, _, _)| d != dir);
        if lists.len() >= LISTS {
            lists.remove(0);
        }
        lists.push((dir.to_owned(), modified, list.clone()));
        Ok(Some(list))
    }

    /// The stored file of `rest`, a path below the on-demand snapshot
    /// `snapshot` of `mirror`, whose directory is `snapshot_dir`: from the
    /// store, or fetched now. `Ok(None)` when the snapshot is not on demand
    /// or does not list `rest`.
    pub async fn package(
        &self,
        family: Family,
        mirror: &str,
        snapshot: &str,
        snapshot_dir: &Path,
        rest: &str,
    ) -> Result<Option<PathBuf>, DemandError> {
        let this = self.clone();
        let dir = snapshot_dir.to_owned();
        let list = tokio::task::spawn_blocking(move || this.list(&dir))
            .await
            .map_err(|e| DemandError::Local(e.to_string()))?
            .map_err(|e| DemandError::Local(e.to_string()))?;
        let Some(package) = list.and_then(|l| l.get(rest).cloned()) else {
            return Ok(None);
        };
        self.fetch(Fetch::Package(Wanted {
            family,
            mirror: mirror.to_owned(),
            snapshot: snapshot.to_owned(),
            snapshot_dir: snapshot_dir.to_owned(),
            package,
        }))
        .await
        .map(Some)
    }

    /// The stored MSI file of SHA-256 `sha256` (ADR-0188), fetched from
    /// the upstream's listener at `/software/<sha256>.msi` when the store
    /// lacks it, and kept only when its digest matches. `Ok(None)` on the
    /// server, whose files are not fetched: they are uploaded.
    pub async fn software(&self, sha256: &str) -> Result<Option<PathBuf>, DemandError> {
        let Source::Listener(_) = &self.0.source else {
            return Ok(None);
        };
        let Some(checksum) = Checksum::new(Algorithm::Sha256, sha256) else {
            return Ok(None);
        };
        self.fetch(Fetch::Software(checksum)).await.map(Some)
    }

    /// The stored file of `wanted`, fetched if needed, one download per
    /// file whoever asks.
    async fn fetch(&self, wanted: Fetch) -> Result<PathBuf, DemandError> {
        let checksum = wanted.checksum().clone();
        let stored = self.0.store.package_path(&checksum);
        let waiting = {
            let mut inflight = lock(&self.0.inflight);
            match inflight.get(&checksum) {
                Some(rx) => Some(rx.clone()),
                // Checked under the lock: a download that just ended has
                // left the map and the file is there.
                None if stored.is_file() => None,
                None => {
                    let (tx, rx) = watch::channel(None);
                    inflight.insert(checksum.clone(), rx.clone());
                    let this = self.clone();
                    tokio::spawn(async move {
                        let result = this.download(&wanted).await;
                        lock(&this.0.inflight).remove(&checksum);
                        let _ = tx.send(Some(result));
                    });
                    Some(rx)
                }
            }
        };
        let Some(mut rx) = waiting else {
            let path = stored.clone();
            let _ = tokio::task::spawn_blocking(move || store::touch(&path)).await;
            return Ok(stored);
        };
        let outcome = rx
            .wait_for(Option::is_some)
            .await
            .map_err(|_| DemandError::Local("the download stopped".into()))?;
        outcome.clone().expect("waited for it")
    }

    /// Downloads `wanted` into the store, under a permit.
    async fn download(&self, wanted: &Fetch) -> Result<PathBuf, DemandError> {
        let _permit = self
            .0
            .permits
            .acquire()
            .await
            .map_err(|e| DemandError::Local(e.to_string()))?;
        let wanted = match wanted {
            Fetch::Package(wanted) => wanted,
            Fetch::Software(checksum) => return self.download_software(checksum).await,
        };
        let entry = &wanted.package.entry;
        let candidates: Vec<(Fetcher, String)> = match &self.0.source {
            Source::Listener(fetcher) => vec![(
                fetcher.clone(),
                format!(
                    "{}/{}/{}/{}",
                    wanted.family, wanted.mirror, wanted.snapshot, entry.path
                ),
            )],
            Source::Origin(client) => {
                let path = wanted.snapshot_dir.join(ORIGIN);
                let origin = tokio::task::spawn_blocking(move || Origin::read(&path))
                    .await
                    .map_err(|e| DemandError::Local(e.to_string()))?
                    .map_err(|e| DemandError::Local(format!("cannot read .origin: {e}")))?
                    .ok_or_else(|| DemandError::Local("the snapshot has no .origin".into()))?;
                std::iter::once(origin.upstream)
                    .chain(origin.archives)
                    .map(|base| (client.at(&base), entry.path.clone()))
                    .collect()
            }
        };
        let mut failure = None;
        for (fetcher, path) in candidates {
            for attempt in 0..2 {
                let temp = self.0.store.temp_path();
                match fetcher.file(&path, &temp, Expect::Entry(entry)).await {
                    Ok(Some(_)) => {
                        let (store, checksum) = (self.0.store.clone(), entry.checksum.clone());
                        let kept = tokio::task::spawn_blocking(move || {
                            store.keep_package(&temp, &checksum)
                        })
                        .await
                        .map_err(|e| DemandError::Local(e.to_string()))?
                        .map_err(|e| DemandError::Local(e.to_string()))?;
                        tracing::info!(mirror = %wanted.mirror, path = %entry.path, url = %fetcher.url(&path), bytes = entry.size, "package fetched on demand");
                        if let Some(hook) = &self.0.kept {
                            hook(entry.size);
                        }
                        return Ok(kept);
                    }
                    Ok(None) => break,
                    Err(error) => {
                        let _ = tokio::fs::remove_file(&temp).await;
                        let retry = attempt == 0
                            && matches!(&error, FetchError::Status { status, .. } if status.is_server_error() || *status == StatusCode::TOO_MANY_REQUESTS);
                        tracing::warn!(mirror = %wanted.mirror, path = %entry.path, %error, "cannot fetch a package on demand");
                        failure = Some(DemandError::Upstream(error.to_string()));
                        if !retry {
                            break;
                        }
                    }
                }
            }
        }
        Err(failure.unwrap_or(DemandError::Missing))
    }

    /// Downloads the MSI file of `checksum` from the upstream's listener,
    /// of at most [`MAX_MSI_BYTES`], and keeps it once its SHA-256 is the
    /// one asked for.
    async fn download_software(&self, checksum: &Checksum) -> Result<PathBuf, DemandError> {
        let Source::Listener(fetcher) = &self.0.source else {
            return Err(DemandError::Missing);
        };
        let path = format!("{SOFTWARE_DIR}/{}.msi", checksum.hex());
        let temp = self.0.store.temp_path();
        let fetched = fetcher
            .file(&path, &temp, Expect::AtMost(MAX_MSI_BYTES))
            .await;
        let entry = match fetched {
            Ok(Some(entry)) if entry.checksum == *checksum => entry,
            Ok(Some(_)) => {
                let _ = tokio::fs::remove_file(&temp).await;
                tracing::warn!(%path, "the upstream sent another file than the MSI file asked for");
                return Err(DemandError::Upstream(
                    "the file's SHA-256 is not its name".into(),
                ));
            }
            Ok(None) => return Err(DemandError::Missing),
            Err(error) => {
                let _ = tokio::fs::remove_file(&temp).await;
                tracing::warn!(%path, %error, "cannot fetch an MSI file");
                return Err(DemandError::Upstream(error.to_string()));
            }
        };
        let (store, kept_checksum) = (self.0.store.clone(), checksum.clone());
        let kept = tokio::task::spawn_blocking(move || store.keep_package(&temp, &kept_checksum))
            .await
            .map_err(|e| DemandError::Local(e.to_string()))?
            .map_err(|e| DemandError::Local(e.to_string()))?;
        tracing::info!(%path, bytes = entry.size, "MSI file fetched");
        if let Some(hook) = &self.0.kept {
            hook(entry.size);
        }
        Ok(kept)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mirror::repo::meta::Entry;

    #[test]
    fn package_lists_are_read_once_while_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::under(dir.path());
        let fetcher = Fetcher::new("http://127.0.0.1:9", Duration::from_secs(1)).unwrap();
        let demand = OnDemand::new(store, Source::Listener(fetcher));
        let snapshot = dir.path().join("s1");
        std::fs::create_dir(&snapshot).unwrap();
        assert!(demand.list(&snapshot).unwrap().is_none());
        let mut hasher = Algorithm::Sha256.hasher();
        hasher.update(b"x");
        let list = PackageList::new(vec![Package {
            entry: Entry {
                path: "pool/x.deb".into(),
                size: 1,
                checksum: hasher.finish(),
            },
            name: "x".into(),
            version: "1".into(),
            arch: "amd64".into(),
        }])
        .unwrap();
        list.write(&snapshot.join(PACKAGES)).unwrap();
        let first = demand.list(&snapshot).unwrap().unwrap();
        let again = demand.list(&snapshot).unwrap().unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        assert!(first.get("pool/x.deb").is_some());
    }
}
