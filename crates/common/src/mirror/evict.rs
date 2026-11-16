//! Keeping the content store of on-demand mirrors under its cap
//! (ADR-0142), on the server and on relays alike.
//!
//! Only packages are evicted, the least recently used first. Spared:
//!
//! - packages listed by a snapshot pinned by a campaign that is not final
//!   (the `.pinned` file of each mirror, ADR-0038);
//! - packages listed by a mirror's `current` snapshot that were used in the
//!   last [`RECENT`].
//!
//! When those alone exceed the cap, nothing protected goes and the outcome
//! says so.

use std::collections::HashSet;
use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime};

use super::packages::{PackageList, PACKAGES};
use super::repo::meta::Checksum;
use super::store::{Store, StoredPackage};
use super::{is_mirror_name, read_current, read_pinned};

/// How long a package of a `current` snapshot stays protected after its
/// last use: twice the 7 daily snapshots retention keeps (ADR-0074), so a
/// weekly maintenance window that skipped a week still finds it.
pub const RECENT: Duration = Duration::from_secs(14 * 24 * 60 * 60);

/// GiB, for caps.
pub const GIB: u64 = 1 << 30;

/// The default cap: 100 GiB, or half the disk when that is smaller.
pub fn default_cap(disk_total_bytes: Option<u64>) -> u64 {
    let cap = 100 * GIB;
    disk_total_bytes.map_or(cap, |total| cap.min(total / 2))
}

/// What one pass did and left.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Outcome {
    pub cap_bytes: u64,
    /// What the store holds after the pass.
    pub used_bytes: u64,
    /// Packages removed, and their bytes.
    pub evicted: u64,
    pub freed_bytes: u64,
    /// Bytes of the packages that could not go.
    pub protected_bytes: u64,
    /// Still over the cap: protected packages and indexes alone exceed it.
    pub over_cap: bool,
}

/// The checksums of the packages in `stored` that must stay, reading the
/// mirrors under `roots` (the served directory of each family). Blocking.
pub fn protected(
    roots: &[&Path],
    stored: &[StoredPackage],
    now: SystemTime,
) -> io::Result<HashSet<Checksum>> {
    let present: HashSet<&Checksum> = stored.iter().map(|p| &p.checksum).collect();
    let recent: HashSet<&Checksum> = stored
        .iter()
        .filter(|p| now.duration_since(p.used).unwrap_or_default() < RECENT)
        .map(|p| &p.checksum)
        .collect();
    let mut keep = HashSet::new();
    for root in roots {
        let entries = match std::fs::read_dir(root) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            other => other?,
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            if !name.to_str().is_some_and(is_mirror_name) || !entry.file_type()?.is_dir() {
                continue;
            }
            let dir = entry.path();
            let mut snapshots: Vec<(String, bool)> =
                read_pinned(&dir).into_iter().map(|s| (s, true)).collect();
            snapshots.extend(read_current(&dir).map(|s| (s, false)));
            for (snapshot, pinned) in snapshots {
                let Some(list) = PackageList::read(&dir.join(&snapshot).join(PACKAGES))? else {
                    continue;
                };
                for package in list.iter() {
                    let checksum = &package.entry.checksum;
                    let wanted = if pinned {
                        present.contains(checksum)
                    } else {
                        recent.contains(checksum)
                    };
                    if wanted {
                        keep.insert(checksum.clone());
                    }
                }
            }
        }
    }
    Ok(keep)
}

/// Removes the least recently used packages that are not protected until
/// `store` holds at most `cap` bytes. Blocking.
pub fn evict(store: &Store, roots: &[&Path], cap: u64, now: SystemTime) -> io::Result<Outcome> {
    let usage = store.usage()?;
    let mut outcome = Outcome {
        cap_bytes: cap,
        used_bytes: usage.total(),
        ..Outcome::default()
    };
    if outcome.used_bytes <= cap {
        return Ok(outcome);
    }
    let mut stored = store.packages()?;
    let keep = protected(roots, &stored, now)?;
    stored.sort_by_key(|p| p.used);
    for package in &stored {
        if keep.contains(&package.checksum) {
            outcome.protected_bytes += package.size;
            continue;
        }
        if outcome.used_bytes <= cap {
            continue;
        }
        store.remove_package(&package.checksum)?;
        outcome.used_bytes = outcome.used_bytes.saturating_sub(package.size);
        outcome.evicted += 1;
        outcome.freed_bytes += package.size;
    }
    outcome.over_cap = outcome.used_bytes > cap;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mirror::repo::meta::{Algorithm, Entry, Package};
    use crate::mirror::{swap_current, write_pinned};

    fn sha256(data: &[u8]) -> Checksum {
        let mut hasher = Algorithm::Sha256.hasher();
        hasher.update(data);
        hasher.finish()
    }

    /// Stores `body` with its last use `days` ago.
    fn put(store: &Store, body: &[u8], days: u64, now: SystemTime) {
        let temp = store.temp_path();
        std::fs::write(&temp, body).unwrap();
        let path = store.keep_package(&temp, &sha256(body)).unwrap();
        let used = now - Duration::from_secs(days * 24 * 3600);
        std::fs::File::open(path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(used))
            .unwrap();
    }

    fn list(dir: &Path, bodies: &[&[u8]]) {
        std::fs::create_dir_all(dir).unwrap();
        let packages = bodies
            .iter()
            .enumerate()
            .map(|(i, body)| Package {
                entry: Entry {
                    path: format!("pool/p{i}.deb"),
                    size: body.len() as u64,
                    checksum: sha256(body),
                },
                name: format!("p{i}"),
                version: "1".into(),
                arch: "amd64".into(),
            })
            .collect();
        PackageList::new(packages)
            .unwrap()
            .write(&dir.join(PACKAGES))
            .unwrap();
    }

    #[test]
    fn the_default_cap_is_100_gib_or_half_the_disk() {
        assert_eq!(default_cap(None), 100 * GIB);
        assert_eq!(default_cap(Some(1000 * GIB)), 100 * GIB);
        assert_eq!(default_cap(Some(60 * GIB)), 30 * GIB);
    }

    #[test]
    fn the_least_recently_used_go_first_and_protected_ones_stay() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::under(dir.path());
        store.prepare().unwrap();
        let now = SystemTime::now();
        let apt = dir.path().join("apt");
        let mirror = apt.join("debian");
        // The current snapshot lists `cur-old` (unused for 30 days) and
        // `cur-new` (used 2 days ago); a pinned one lists `pinned`, unused
        // for 60 days. `a` and `b` are listed by nothing in use.
        list(&mirror.join("s2"), &[b"cur-old", b"cur-new"]);
        list(&mirror.join("s1"), &[b"pinned"]);
        list(&mirror.join("s0"), &[b"aaaa", b"bbbb"]);
        swap_current(&mirror, "s2").unwrap();
        write_pinned(&mirror, &["s1".to_owned()]).unwrap();
        put(&store, b"pinned", 60, now);
        put(&store, b"aaaa", 40, now);
        put(&store, b"cur-old", 30, now);
        put(&store, b"bbbb", 10, now);
        put(&store, b"cur-new", 2, now);
        let total = store.usage().unwrap().total();
        assert_eq!(total, 6 + 4 + 7 + 4 + 7);

        // Under the cap: nothing goes.
        let outcome = evict(&store, &[&apt], total, now).unwrap();
        assert_eq!((outcome.evicted, outcome.used_bytes), (0, total));

        // 8 bytes over: `aaaa` (oldest unprotected), then `cur-old`, which
        // was not used in the last 14 days.
        let outcome = evict(&store, &[&apt], total - 8, now).unwrap();
        assert_eq!(outcome.evicted, 2);
        assert_eq!(outcome.freed_bytes, 4 + 7);
        assert!(!outcome.over_cap);
        let left: HashSet<Checksum> = store
            .packages()
            .unwrap()
            .into_iter()
            .map(|p| p.checksum)
            .collect();
        assert!(!left.contains(&sha256(b"aaaa")) && !left.contains(&sha256(b"cur-old")));
        assert!(left.contains(&sha256(b"bbbb")) && left.contains(&sha256(b"pinned")));

        // A cap below what is protected: everything else goes, the
        // protected stay, and the outcome says the cap is too small.
        let outcome = evict(&store, &[&apt], 1, now).unwrap();
        assert!(outcome.over_cap);
        assert_eq!(outcome.used_bytes, 6 + 7);
        assert_eq!(outcome.protected_bytes, 6 + 7);
        let left: Vec<Checksum> = store
            .packages()
            .unwrap()
            .into_iter()
            .map(|p| p.checksum)
            .collect();
        assert_eq!(left.len(), 2);
    }

    #[test]
    fn indexes_count_against_the_cap_but_never_go() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::under(dir.path());
        store.prepare().unwrap();
        let now = SystemTime::now();
        let temp = store.temp_path();
        std::fs::write(&temp, b"Packages index").unwrap();
        store.keep_index(&temp, &sha256(b"Packages index")).unwrap();
        put(&store, b"pkg", 1, now);
        let outcome = evict(&store, &[], 14, now).unwrap();
        assert_eq!(
            (outcome.evicted, outcome.used_bytes, outcome.over_cap),
            (1, 14, false)
        );
        let outcome = evict(&store, &[], 1, now).unwrap();
        assert!(outcome.over_cap);
        assert_eq!(store.usage().unwrap().index_bytes, 14);
    }
}
