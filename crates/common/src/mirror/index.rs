//! `/index.json` (ADR-0034): the mirrors a listener publishes and the
//! snapshot each `current` names, built from the served tree so that a
//! relay's index describes exactly what that relay serves.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{is_mirror_name, is_snapshot_name, read_current, Family, Served};

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
            if !is_snapshot_name(&m.current) {
                return Err(fail("invalid snapshot name"));
            }
            match m.family {
                Family::Apt if m.distributions.is_empty() => {
                    return Err(fail("apt mirror without distributions"))
                }
                Family::Dnf if !m.distributions.is_empty() => {
                    return Err(fail("dnf mirror with distributions"))
                }
                _ => {}
            }
            if !m.distributions.iter().all(|d| is_snapshot_name(d)) {
                return Err(fail("invalid distribution name"));
            }
        }
        Ok(index)
    }

    /// Lists what `served` publishes: every mirror directory with a valid
    /// name whose `current` names an existing snapshot. Blocking.
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
                let distributions = match family {
                    Family::Apt => subdirectories(&snapshot.join("dists"))?,
                    Family::Dnf => Vec::new(),
                };
                if family == Family::Apt && distributions.is_empty() {
                    continue;
                }
                mirrors.push(IndexMirror {
                    family,
                    name,
                    current,
                    distributions,
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
        swap_current(&dnf.join("rocky9"), "s2").unwrap();
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
                },
                IndexMirror {
                    family: Family::Dnf,
                    name: "rocky9".into(),
                    current: "s2".into(),
                    distributions: vec![],
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
    fn parse_rejects_unsafe_entries() {
        let entry = |family: &str, name: &str, current: &str, dists: &str| {
            format!(
                r#"{{"version":1,"mirrors":[{{"family":"{family}","name":"{name}","current":"{current}","distributions":[{dists}]}}]}}"#
            )
        };
        assert!(Index::parse(entry("apt", "debian", "s1", r#""bookworm""#).as_bytes()).is_ok());
        assert!(Index::parse(entry("dnf", "rocky", "s1", "").as_bytes()).is_ok());
        for bad in [
            entry("apt", "../x", "s1", r#""bookworm""#),
            entry("apt", "debian", "..", r#""bookworm""#),
            entry("apt", "debian", "current", r#""bookworm""#),
            entry("apt", "debian", "s1", r#""../../etc""#),
            entry("apt", "debian", "s1", ""),
            entry("dnf", "rocky", "s1", r#""x""#),
            entry("rpm", "rocky", "s1", ""),
            r#"{"version":2,"mirrors":[]}"#.into(),
            "not json".into(),
        ] {
            assert!(Index::parse(bad.as_bytes()).is_err(), "{bad}");
        }
    }
}
