//! `/index.json` (ADR-0034): the mirrors a listener publishes, the
//! snapshot each `current` names and the snapshots pinned by update
//! campaigns (ADR-0038), built from the served tree so that a relay's
//! index describes exactly what that relay serves. It also names the
//! mirrors an administrator deleted (ADR-0075): a mirror merely missing
//! says nothing, as an upstream restored from a backup lists none.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{is_mirror_name, is_snapshot_name, read_current, Family, Served, PINNED, REMOVED};

/// Format version of the index.
pub const VERSION: u32 = 1;

/// Prefix of a mirror directory on its way out, hidden from the listener.
const RETIRED: &str = ".removing-";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Index {
    pub version: u32,
    pub mirrors: Vec<IndexMirror>,
    /// Agent releases served under `/agent/<version>/`, newest first
    /// (ADR-0053). Absent from older servers' indexes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agent_releases: Vec<String>,
    /// Mirrors deleted upstream, for relays to delete their copy
    /// (ADR-0075). Absent from older servers' indexes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<RemovedMirror>,
}

/// A mirror an administrator deleted, and that is not published again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovedMirror {
    pub family: Family,
    pub name: String,
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
    #[error("index lists an invalid agent release")]
    AgentRelease,
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
        if index
            .agent_releases
            .iter()
            .any(|v| v.parse::<crate::release::Version>().is_err())
        {
            return Err(IndexError::AgentRelease);
        }
        if let Some(bad) = index.removed.iter().find(|m| !is_mirror_name(&m.name)) {
            return Err(IndexError::Entry {
                family: bad.family,
                name: bad.name.clone(),
                reason: "invalid removed mirror name",
            });
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
    /// snapshots its `.pinned` file names, and the mirrors `.removed`
    /// names that are not published again. Blocking.
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
        let removed = Family::ALL
            .into_iter()
            .flat_map(|family| {
                read_removed(served.root(family))
                    .into_iter()
                    .map(move |name| RemovedMirror { family, name })
            })
            .filter(|r| {
                !mirrors
                    .iter()
                    .any(|m| m.family == r.family && m.name == r.name)
            })
            .collect();
        let agent_releases = crate::release::cached_versions(&served.agent)?
            .iter()
            .map(ToString::to_string)
            .collect();
        Ok(Self {
            version: VERSION,
            mirrors,
            agent_releases,
            removed,
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
    write_names(&dir.join(PINNED), names)
}

/// The mirror names in `<root>/.removed`, `root` being a family's
/// directory, one per line, sorted; invalid lines are skipped and a
/// missing file is none. Blocking.
pub fn read_removed(root: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(root.join(REMOVED)).unwrap_or_default();
    let mut names: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|n| is_mirror_name(n))
        .map(str::to_owned)
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Makes `<root>/.removed` list `names`, as [`write_pinned`]. Blocking.
pub fn write_removed(root: &Path, names: &[String]) -> io::Result<()> {
    write_names(&root.join(REMOVED), names)
}

/// Takes `<root>/<name>` out of service at once, whole, with a rename to a
/// hidden name, then deletes it. Hidden files stop being served and the
/// disk space comes back; a crash in between leaves what
/// [`remove_retired`] finishes. Blocking.
pub fn remove_mirror_dir(root: &Path, name: &str) -> io::Result<()> {
    let hidden = root.join(format!("{RETIRED}{name}"));
    match std::fs::remove_dir_all(&hidden) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    match std::fs::rename(root.join(name), &hidden) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
        Ok(()) => std::fs::remove_dir_all(&hidden),
    }
}

/// Deletes what [`remove_mirror_dir`] left in `root`. Blocking.
pub fn remove_retired(root: &Path) -> io::Result<()> {
    let entries = match std::fs::read_dir(root) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        other => other?,
    };
    for entry in entries {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with(RETIRED) {
            std::fs::remove_dir_all(entry.path())?;
        }
    }
    Ok(())
}

fn write_names(path: &Path, names: &[String]) -> io::Result<()> {
    if names.is_empty() {
        return match std::fs::remove_file(path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let text: String = names.iter().map(|n| format!("{n}\n")).collect();
    if std::fs::read_to_string(path).ok().as_deref() == Some(text.as_str()) {
        return Ok(());
    }
    super::write_atomically(path, text.as_bytes())
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
    fn scan_lists_agent_releases_and_parse_checks_them() {
        let dir = tempfile::tempdir().unwrap();
        let served = Served::under(dir.path());
        for (name, version) in [
            ("0.2.0", "0.2.0\n"),
            ("0.10.0", "0.10.0\n"),
            ("0.3.0", "0.4.0\n"),
        ] {
            std::fs::create_dir_all(served.agent.join(name)).unwrap();
            std::fs::write(served.agent.join(name).join("VERSION"), version).unwrap();
        }
        std::fs::create_dir_all(served.agent.join(".0.11.0.part")).unwrap();
        let index = Index::scan(&served).unwrap();
        assert_eq!(index.agent_releases, ["0.10.0", "0.2.0"]);
        let json = serde_json::to_vec(&index).unwrap();
        assert_eq!(Index::parse(&json).unwrap(), index);
        // Older indexes have none; a name that is not a version is refused.
        let old = br#"{"version":1,"mirrors":[]}"#;
        assert!(Index::parse(old).unwrap().agent_releases.is_empty());
        let bad = br#"{"version":1,"mirrors":[],"agent_releases":["../x"]}"#;
        assert!(matches!(Index::parse(bad), Err(IndexError::AgentRelease)));
    }

    #[test]
    fn removed_mirrors_are_listed_until_published_again() {
        let dir = tempfile::tempdir().unwrap();
        let served = Served::under(dir.path());
        let dnf = served.root(Family::Dnf);
        std::fs::create_dir_all(dnf.join("rocky/s1")).unwrap();
        write_removed(dnf, &["alma".into(), "rocky".into(), "Bad/name".into()]).unwrap();
        // Hidden from the listener by its dot, and no mirror name.
        assert_eq!(read_removed(dnf), ["alma", "rocky"]);

        // `rocky` has no `current`, so it is not published.
        let index = Index::scan(&served).unwrap();
        let removed = |name: &str| RemovedMirror {
            family: Family::Dnf,
            name: name.into(),
        };
        assert_eq!(index.removed, [removed("alma"), removed("rocky")]);
        let json = serde_json::to_vec(&index).unwrap();
        assert_eq!(Index::parse(&json).unwrap(), index);

        swap_current(&dnf.join("rocky"), "s1").unwrap();
        assert_eq!(Index::scan(&served).unwrap().removed, [removed("alma")]);

        // Absent from older indexes, and left out when empty.
        let old = br#"{"version":1,"mirrors":[]}"#;
        assert!(Index::parse(old).unwrap().removed.is_empty());
        write_removed(dnf, &[]).unwrap();
        let json = serde_json::to_string(&Index::scan(&served).unwrap()).unwrap();
        assert!(!json.contains("removed"), "{json}");

        for bad in ["../x", "a/b", ".hidden", ""] {
            let json = format!(
                r#"{{"version":1,"mirrors":[],"removed":[{{"family":"dnf","name":"{bad}"}}]}}"#
            );
            assert!(Index::parse(json.as_bytes()).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_mirror_directory_goes_whole() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("rocky/s1/Packages")).unwrap();
        std::fs::write(root.join("rocky/s1/Packages/a.rpm"), "rpm").unwrap();
        // A crash left these behind.
        std::fs::create_dir_all(root.join(".removing-rocky/s0")).unwrap();
        std::fs::create_dir_all(root.join(".removing-alma/s0")).unwrap();
        std::fs::create_dir_all(root.join("keep")).unwrap();

        remove_mirror_dir(root, "rocky").unwrap();
        remove_mirror_dir(root, "rocky").unwrap();
        remove_mirror_dir(root, "never-here").unwrap();
        assert!(!root.join("rocky").exists());
        assert!(!root.join(".removing-rocky").exists());
        remove_retired(root).unwrap();
        let left: Vec<_> = std::fs::read_dir(root).unwrap().collect();
        assert_eq!(left.len(), 1);
        assert!(root.join("keep").is_dir());
        remove_retired(&root.join("missing")).unwrap();
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
