//! The content store of on-demand mirrors (ADR-0142), under
//! `<mirror_dir>/ondemand/`, which the listener does not serve:
//!
//! - `index/sha256/<2 hex>/<64 hex>`: index files, hardlinked into every
//!   snapshot tree that has them, so a file shared by snapshots or mirrors
//!   is stored once. One whose only link is the store's belongs to no
//!   snapshot any more, and [`Store::sweep_indexes`] removes it.
//! - `packages/<algorithm>/<2 hex>/<hex>`: packages fetched on demand, by
//!   the checksum their index gives, never linked elsewhere. The file's
//!   modification time is when it was last used, which eviction reads.
//! - `.tmp/`: downloads in progress, moved into place once checked.
//!
//! Everything in the store was checked against its checksum before it
//! was moved there, and is read-only.

use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use super::repo::meta::{Algorithm, Checksum};

/// Directory of the store under a mirror directory.
pub const STORE_DIR: &str = "ondemand";

/// How stale a package's time of use may get before serving it updates
/// the time: writing it on every request would cost a metadata write per
/// download for no better eviction.
pub const TOUCH_EVERY: Duration = Duration::from_secs(60 * 60);

const INDEX: &str = "index";
const PACKAGES: &str = "packages";
const TEMP: &str = ".tmp";

/// A package in the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPackage {
    pub checksum: Checksum,
    pub path: PathBuf,
    pub size: u64,
    /// When it was last fetched or served.
    pub used: SystemTime,
}

/// What the store holds, in bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub index_bytes: u64,
    pub package_bytes: u64,
    pub packages: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.index_bytes.saturating_add(self.package_bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    root: PathBuf,
}

fn algorithm_dir(algorithm: Algorithm) -> &'static str {
    match algorithm {
        Algorithm::Sha256 => "sha256",
        Algorithm::Sha512 => "sha512",
    }
}

/// `<dir>/<algorithm>/<2 hex>/<hex>`.
fn by_checksum(dir: PathBuf, checksum: &Checksum) -> PathBuf {
    let hex = checksum.hex();
    dir.join(algorithm_dir(checksum.algorithm))
        .join(&hex[..2])
        .join(hex)
}

impl Store {
    /// The store at `root`, usually `<mirror_dir>/ondemand`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The store of the mirror directory `mirror_dir`.
    pub fn under(mirror_dir: &Path) -> Self {
        Self::new(mirror_dir.join(STORE_DIR))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Creates the store's directories and empties `.tmp`, which only a
    /// process stopped mid-download leaves anything in. Blocking.
    pub fn prepare(&self) -> io::Result<()> {
        for dir in [INDEX, PACKAGES] {
            std::fs::create_dir_all(self.root.join(dir))?;
        }
        let temp = self.root.join(TEMP);
        match std::fs::remove_dir_all(&temp) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        std::fs::create_dir_all(temp)
    }

    /// Where the index file of SHA-256 `sha256` is kept.
    pub fn index_path(&self, sha256: &Checksum) -> PathBuf {
        by_checksum(self.root.join(INDEX), sha256)
    }

    /// Where the package of checksum `checksum` is kept.
    pub fn package_path(&self, checksum: &Checksum) -> PathBuf {
        by_checksum(self.root.join(PACKAGES), checksum)
    }

    /// A new path in `.tmp`, for a download. Two calls never give the same
    /// one in a process; the process ID keeps two processes apart.
    pub fn temp_path(&self) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        self.root
            .join(TEMP)
            .join(format!("{}-{n}", std::process::id()))
    }

    /// Moves `file`, already checked to have SHA-256 `sha256`, into the
    /// index store, or removes it when the store has it already. Returns
    /// the stored path. Blocking.
    pub fn keep_index(&self, file: &Path, sha256: &Checksum) -> io::Result<PathBuf> {
        keep(file, self.index_path(sha256))
    }

    /// Moves `file`, already checked to have checksum `checksum`, into the
    /// package store, or removes it when the store has it already. Its time
    /// of use is now. Returns the stored path. Blocking.
    pub fn keep_package(&self, file: &Path, checksum: &Checksum) -> io::Result<PathBuf> {
        let kept = keep(file, self.package_path(checksum))?;
        set_used(&kept, SystemTime::now())?;
        Ok(kept)
    }

    /// The packages in the store, in no particular order. Files whose name
    /// is not a checksum are skipped. Blocking.
    pub fn packages(&self) -> io::Result<Vec<StoredPackage>> {
        let mut found = Vec::new();
        for algorithm in [Algorithm::Sha256, Algorithm::Sha512] {
            let dir = self.root.join(PACKAGES).join(algorithm_dir(algorithm));
            walk_files(&dir, &mut |path, meta| {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if let Some(checksum) = Checksum::new(algorithm, name) {
                    found.push(StoredPackage {
                        checksum,
                        path: path.to_owned(),
                        size: meta.len(),
                        used: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    });
                }
            })?;
        }
        Ok(found)
    }

    /// What the store holds. Temporary files do not count. Blocking.
    pub fn usage(&self) -> io::Result<Usage> {
        let mut usage = Usage::default();
        walk_files(&self.root.join(INDEX), &mut |_, meta| {
            usage.index_bytes += meta.len();
        })?;
        walk_files(&self.root.join(PACKAGES), &mut |_, meta| {
            usage.package_bytes += meta.len();
            usage.packages += 1;
        })?;
        Ok(usage)
    }

    /// Removes the index files no snapshot links to any more, and returns
    /// the bytes freed. Blocking.
    pub fn sweep_indexes(&self) -> io::Result<u64> {
        let mut unused = Vec::new();
        walk_files(&self.root.join(INDEX), &mut |path, meta| {
            if meta.nlink() == 1 {
                unused.push((path.to_owned(), meta.len()));
            }
        })?;
        let mut freed = 0;
        for (path, size) in unused {
            match std::fs::remove_file(&path) {
                Ok(()) => freed += size,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(freed)
    }

    /// Removes a package. Blocking.
    pub fn remove_package(&self, checksum: &Checksum) -> io::Result<()> {
        match std::fs::remove_file(self.package_path(checksum)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }
}

/// Records that the stored file at `path` was used now, unless it was
/// within [`TOUCH_EVERY`]. Blocking.
pub fn touch(path: &Path) -> io::Result<()> {
    let modified = std::fs::metadata(path)?.modified()?;
    let now = SystemTime::now();
    if now.duration_since(modified).unwrap_or_default() >= TOUCH_EVERY {
        set_used(path, now)?;
    }
    Ok(())
}

/// Sets the time of use of the read-only file at `path`. Changing the
/// times of a file needs no write permission for its owner.
fn set_used(path: &Path, time: SystemTime) -> io::Result<()> {
    let file = std::fs::File::open(path)?;
    file.set_times(std::fs::FileTimes::new().set_modified(time))
}

/// Links the stored file at `stored` to `dest`, which must not exist;
/// copies it across file systems. Blocking.
pub fn link(stored: &Path, dest: &Path) -> io::Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::hard_link(stored, dest) {
        Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
            std::fs::copy(stored, dest).map(drop)
        }
        other => other,
    }
}

/// Moves `file` to `dest`, read-only, unless `dest` exists: then `file` is
/// removed, as the store's copy has the same content.
fn keep(file: &Path, dest: PathBuf) -> io::Result<PathBuf> {
    if dest.exists() {
        std::fs::remove_file(file)?;
        return Ok(dest);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o444))?;
    std::fs::rename(file, &dest)?;
    Ok(dest)
}

/// Calls `found` with every regular file under `dir`, without following
/// links; a missing `dir` has none. Blocking.
fn walk_files(dir: &Path, found: &mut dyn FnMut(&Path, &std::fs::Metadata)) -> io::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        other => other?,
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let meta = match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            other => other?,
        };
        if meta.is_dir() {
            walk_files(&path, found)?;
        } else if meta.is_file() {
            found(&path, &meta);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha256(data: &[u8]) -> Checksum {
        let mut hasher = Algorithm::Sha256.hasher();
        hasher.update(data);
        hasher.finish()
    }

    fn temp_with(store: &Store, data: &[u8]) -> PathBuf {
        let path = store.temp_path();
        std::fs::write(&path, data).unwrap();
        path
    }

    #[test]
    fn indexes_are_stored_once_and_swept_when_unlinked() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::under(dir.path());
        store.prepare().unwrap();
        let sum = sha256(b"Packages");
        let kept = store
            .keep_index(&temp_with(&store, b"Packages"), &sum)
            .unwrap();
        assert_eq!(kept, store.index_path(&sum));
        assert!(kept.ends_with(format!("index/sha256/{}/{}", &sum.hex()[..2], sum.hex())));
        // The same content again: the store keeps its copy.
        let again = temp_with(&store, b"Packages");
        assert_eq!(store.keep_index(&again, &sum).unwrap(), kept);
        assert!(!again.exists());
        let mode = std::fs::metadata(&kept).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o444);

        let s1 = dir
            .path()
            .join("apt/m/s1/dists/x/main/binary-amd64/Packages");
        let s2 = dir
            .path()
            .join("apt/m/s2/dists/x/main/binary-amd64/Packages");
        link(&kept, &s1).unwrap();
        link(&kept, &s2).unwrap();
        assert_eq!(std::fs::metadata(&kept).unwrap().nlink(), 3);
        assert_eq!(store.usage().unwrap().index_bytes, 8);

        assert_eq!(store.sweep_indexes().unwrap(), 0);
        std::fs::remove_file(&s1).unwrap();
        assert_eq!(store.sweep_indexes().unwrap(), 0);
        std::fs::remove_file(&s2).unwrap();
        assert_eq!(store.sweep_indexes().unwrap(), 8);
        assert!(!kept.exists());
    }

    #[test]
    fn packages_are_listed_with_their_time_of_use() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::under(dir.path());
        store.prepare().unwrap();
        assert!(store.packages().unwrap().is_empty());
        assert_eq!(store.usage().unwrap(), Usage::default());

        let a = sha256(b"a.deb");
        let path = store
            .keep_package(&temp_with(&store, b"a.deb"), &a)
            .unwrap();
        let mut sha512 = Algorithm::Sha512.hasher();
        sha512.update(b"b.rpm!");
        let b = sha512.finish();
        store
            .keep_package(&temp_with(&store, b"b.rpm!"), &b)
            .unwrap();
        std::fs::write(path.with_file_name("not-a-checksum"), "x").unwrap();

        let mut listed = store.packages().unwrap();
        listed.sort_by_key(|p| p.size);
        assert_eq!(listed.len(), 2);
        assert_eq!((listed[0].checksum.clone(), listed[0].size), (a.clone(), 5));
        assert_eq!((listed[1].checksum.clone(), listed[1].size), (b, 6));
        let usage = store.usage().unwrap();
        assert_eq!((usage.packages, usage.package_bytes), (3, 12));
        assert_eq!(usage.total(), 12);

        // A recent use is not written again; an old one is.
        let old = SystemTime::now() - 2 * TOUCH_EVERY;
        set_used(&path, old).unwrap();
        touch(&path).unwrap();
        let used = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert!(used > old + TOUCH_EVERY);
        let recent = SystemTime::now() - TOUCH_EVERY / 2;
        set_used(&path, recent).unwrap();
        touch(&path).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            recent
        );

        store.remove_package(&a).unwrap();
        store.remove_package(&a).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn prepare_empties_the_downloads_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::under(dir.path());
        store.prepare().unwrap();
        let left = temp_with(&store, b"half");
        assert_ne!(store.temp_path(), store.temp_path());
        store.prepare().unwrap();
        assert!(!left.exists());
        assert!(store.root().join(".tmp").is_dir());
    }
}
