//! `/index.json` (ADR-0034): the mirrors a listener publishes, the
//! snapshot each `current` names and the snapshots pinned by update
//! campaigns (ADR-0038), built from the served tree so that a relay's
//! index describes exactly what that relay serves.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{is_mirror_name, is_snapshot_name, read_current, Family, Served, PINNED};

/// Format version of the index.
pub const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Index {
    pub version: u32,
    pub mirrors: Vec<IndexMirror>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexMirror {
    pub family: Family,
    pub name: String,
    /// Snapshot `current` names.
    pub current: String,
    /// apt: directories under `dists/` of the snapshot. Empty for dnf.
    #[serde(default)]
    pub distributions: Vec<String>,
    /// Other snapshots to keep, pinned by update campaigns (ADR-0038).
    /// Absent from indexes of older upstreams.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pinned: Vec<PinnedSnapshot>,
}

/// A snapshot other than `current` that downstream relays must hold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinnedSnapshot {
    pub name: String,
    /// As [`IndexMirror::distributions`], for this snapshot.
    #[serde(default)]
    pub distributions: Vec<String>,
}

impl IndexMirror {
    /// The entry of the pinned snapshot `pinned` of this mirror, as if
    /// `current` named it.
    pub fn at(&self, pinned: &PinnedSnapshot) -> Self {
        Self {
            family: self.family,
            name: self.name.clone(),
            current: pinned.name.clone(),
            distributions: pinned.distributions.clone(),
            pinned: Vec::new(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("index is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("index version {0} is not supported (expected {VERSION})")]
    Version(u32),
    #[error("index entry {family}/{name}: {reason}")]
    Entry {
        family: Family,
        name: String,
        reason: &'static str,
    },
}

impl Index {
    /// Parses an index from an upstream, rejecting names that are not
    /// single safe path segments: they become paths on the relay.
    pub fn parse(bytes: &[u8]) -> Result<Self, IndexError> {
        let index: Index = serde_json::from_slice(bytes)?;
        if index.version != VERSION {
            return Err(IndexError::Version(index.version));
        }
        for m in &index.mirrors {
            let fail = |reason| IndexError::Entry {
                family: m.family,
                name: m.name.clone(),
                reason,
            };
            if !is_mirror_name(&m.name) {
                return Err(fail("invalid mirror name"));
            }
            let snapshots = std::iter::once((&m.current, &m.distributions))
                .chain(m.pinned.iter().map(|p| (&p.name, &p.distributions)));
            for (snapshot, distributions) in snapshots {
                if !is_snapshot_name(snapshot) {
                    return Err(fail("invalid snapshot name"));
                }
                match m.family {
                    Family::Apt if distributions.is_empty() => {
                        return Err(fail("apt mirror without distributions"))
                    }
                    Family::Dnf if !distributions.is_empty() => {
                        return Err(fail("dnf mirror with distributions"))
                    }
                    _ => {}
                }
                if !distributions.iter().all(|d| is_snapshot_name(d)) {
                    return Err(fail("invalid distribution name"));
                }
            }
        }
        Ok(index)
    }

    /// Lists what `served` publishes: every mirror directory with a valid
    /// name whose `current` names an existing snapshot, with the existing
    /// snapshots its `.pinned` file names. Blocking.
    pub fn scan(served: &Served) -> io::Result<Self> {
        let mut mirrors = Vec::new();
        for family in Family::ALL {
            let root = served.root(family);
            let entries = match std::fs::read_dir(root) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                other => other?,
            };
            for entry in entries {
                let entry = entry?;
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if !is_mirror_name(&name) || !entry.file_type()?.is_dir() {
                    continue;
                }
                let dir = entry.path();
                let Some(current) = read_current(&dir) else {
                    continue;
                };
                let snapshot = dir.join(&current);
                if !snapshot.is_dir() {
                    continue;
                }
                let Some(distributions) = distributions_of(family, &snapshot)? else {
                    continue;
                };
                let mut pinned = Vec::new();
                for name in read_pinned(&dir) {
                    let path = dir.join(&name);
                    if name == current || !path.is_dir() {
                        continue;
                    }
                    if let Some(distributions) = distributions_of(family, &path)? {
                        pinned.push(PinnedSnapshot {
                            name,
                            distributions,
                        });
                    }
                }
                mirrors.push(IndexMirror {
                    family,
                    name,
                    current,
                    distributions,
                    pinned,
                });
            }
        }
        mirrors.sort_by(|a, b| (a.family, &a.name).cmp(&(b.family, &b.name)));
        Ok(Self {
            version: VERSION,
            mirrors,
        })
    }
}

/// The distributions of the snapshot at `path`; `None` for an apt
/// snapshot without any, which is not usable.
fn distributions_of(family: Family, path: &Path) -> io::Result<Option<Vec<String>>> {
    Ok(match family {
        Family::Apt => Some(subdirectories(&path.join("dists"))?).filter(|d| !d.is_empty()),
        Family::Dnf => Some(Vec::new()),
    })
}

/// The snapshot names in `<dir>/.pinned`, one per line, sorted; invalid
/// lines are skipped and a missing file is none. Blocking.
pub fn read_pinned(dir: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(dir.join(PINNED)).unwrap_or_default();
    let mut names: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|n| is_snapshot_name(n))
        .map(str::to_owned)
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Makes `<dir>/.pinned` list `names`, removing it when there are none;
/// unchanged contents are not rewritten. Blocking.
pub fn write_pinned(dir: &Path, names: &[String]) -> io::Result<()> {
    let path = dir.join(PINNED);
    if names.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let text: String = names.iter().map(|n| format!("{n}\n")).collect();
    if std::fs::read_to_string(&path).ok().as_deref() == Some(text.as_str()) {
        return Ok(());
    }
    super::write_atomically(&path, text.as_bytes())
}

/// Sorted names of the directories in `dir` that are valid names.
fn subdirectories(dir: &Path) -> io::Result<Vec<String>> {
    let entries = match std::fs::read_dir(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        other => other?,
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry?;
        if let Some(name) = entry.file_name().to_str() {
            if is_snapshot_name(name) && entry.file_type()?.is_dir() {
                names.push(name.to_owned());
            }
        }
    }
    names.sort();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::super::swap_current;
    use super::*;

    #[test]
    fn scan_lists_published_mirrors_only() {
        let dir = tempfile::tempdir().unwrap();
        let served = Served::under(dir.path());
        let apt = served.root(Family::Apt);
        let dnf = served.root(Family::Dnf);
        std::fs::create_dir_all(apt.join("debian/s1/dists/bookworm")).unwrap();
        std::fs::create_dir_all(apt.join("debian/s1/dists/.hidden")).unwrap();
        swap_current(&apt.join("debian"), "s1").unwrap();
        // Building, no current yet.
        std::fs::create_dir_all(apt.join("ubuntu/.sync-s1/dists/noble")).unwrap();
        std::fs::create_dir_all(dnf.join("rocky9/s2/repodata")).unwrap();
        std::fs::create_dir_all(dnf.join("rocky9/s0/repodata")).unwrap();
        swap_current(&dnf.join("rocky9"), "s2").unwrap();
        // Pinned: an existing snapshot, current itself, a missing one and
        // an invalid name.
        std::fs::write(dnf.join("rocky9/.pinned"), "s0\ns2\ngone\n../x\n").unwrap();
        // An apt snapshot without distributions cannot be pinned.
        std::fs::create_dir_all(apt.join("debian/s0")).unwrap();
        write_pinned(&apt.join("debian"), &["s0".into()]).unwrap();
        // Dangling current.
        std::fs::create_dir_all(dnf.join("alma")).unwrap();
        swap_current(&dnf.join("alma"), "gone").unwrap();
        std::fs::create_dir_all(dnf.join("Bad")).unwrap();

        let index = Index::scan(&served).unwrap();
        let json = serde_json::to_vec(&index).unwrap();
        assert_eq!(Index::parse(&json).unwrap(), index);
        assert_eq!(
            index.mirrors,
            [
                IndexMirror {
                    family: Family::Apt,
                    name: "debian".into(),
                    current: "s1".into(),
                    distributions: vec!["bookworm".into()],
                    pinned: vec![],
                },
                IndexMirror {
                    family: Family::Dnf,
                    name: "rocky9".into(),
                    current: "s2".into(),
                    distributions: vec![],
                    pinned: vec![PinnedSnapshot {
                        name: "s0".into(),
                        distributions: vec![],
                    }],
                },
            ]
        );
        let empty = tempfile::tempdir().unwrap();
        assert!(Index::scan(&Served::under(empty.path()))
            .unwrap()
            .mirrors
            .is_empty());
    }

    #[test]
    fn pinned_files_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_pinned(dir.path()).is_empty());
        write_pinned(dir.path(), &["s2".into(), "s1".into()]).unwrap();
        assert_eq!(read_pinned(dir.path()), ["s1", "s2"]);
        write_pinned(dir.path(), &[]).unwrap();
        assert!(!dir.path().join(PINNED).exists());
        write_pinned(dir.path(), &[]).unwrap();
    }

    #[test]
    fn parse_rejects_unsafe_entries() {
        let entry = |family: &str, name: &str, current: &str, dists: &str| {
            format!(
                r#"{{"version":1,"mirrors":[{{"family":"{family}","name":"{name}","current":"{current}","distributions":[{dists}]}}]}}"#
            )
        };
        assert!(Index::parse(entry("apt", "debian", "s1", r#""bookworm""#).as_bytes()).is_ok());
        assert!(Index::parse(entry("dnf", "rocky", "s1", "").as_bytes()).is_ok());
        let pinned = r#"{"version":1,"mirrors":[{"family":"dnf","name":"rocky","current":"s1","pinned":[{"name":"s0"}]}]}"#;
        assert_eq!(
            Index::parse(pinned.as_bytes()).unwrap().mirrors[0].pinned[0].name,
            "s0"
        );
        for bad in [
            entry("apt", "../x", "s1", r#""bookworm""#),
            entry("apt", "debian", "..", r#""bookworm""#),
            entry("apt", "debian", "current", r#""bookworm""#),
            entry("apt", "debian", "s1", r#""../../etc""#),
            entry("apt", "debian", "s1", ""),
            entry("dnf", "rocky", "s1", r#""x""#),
            entry("rpm", "rocky", "s1", ""),
            r#"{"version":2,"mirrors":[]}"#.into(),
            r#"{"version":1,"mirrors":[{"family":"dnf","name":"rocky","current":"s1","pinned":[{"name":".."}]}]}"#.into(),
            r#"{"version":1,"mirrors":[{"family":"apt","name":"d","current":"s1","distributions":["b"],"pinned":[{"name":"s0"}]}]}"#.into(),
            "not json".into(),
        ] {
            assert!(Index::parse(bad.as_bytes()).is_err(), "{bad}");
        }
    }
}
