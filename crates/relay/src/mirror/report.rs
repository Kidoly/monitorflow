//! The report of what this relay serves, sent to the server on the
//! relay's own stream (ADR-0037) so the package sources of its agents
//! point at the relay.

use std::io;

use monitorflow_common::mirror::{is_mirror_name, read_current, Family, Served};
use monitorflow_common::proto::v1::{RelayMirrors, ServedMirror};

use super::sync::complete_snapshots;

/// Every mirror under `served` with a complete snapshot, and the snapshot
/// `current` names, reachable at `public_url`. Blocking.
pub fn scan(served: &Served, public_url: &str) -> io::Result<RelayMirrors> {
    let mut mirrors = Vec::new();
    for family in Family::ALL {
        let entries = match std::fs::read_dir(served.root(family)) {
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
            let mut snapshots = complete_snapshots(&dir);
            if snapshots.is_empty() {
                continue;
            }
            snapshots.sort();
            let current = read_current(&dir)
                .filter(|c| snapshots.contains(c))
                .unwrap_or_default();
            mirrors.push(ServedMirror {
                family: family.as_str().into(),
                name,
                current,
                snapshots,
            });
        }
    }
    mirrors.sort_by(|a, b| (&a.family, &a.name).cmp(&(&b.family, &b.name)));
    Ok(RelayMirrors {
        public_url: public_url.to_owned(),
        mirrors,
    })
}

#[cfg(test)]
mod tests {
    use monitorflow_common::mirror::swap_current;

    use super::super::sync::MANIFEST;
    use super::*;

    fn snapshot(dir: &std::path::Path, name: &str, complete: bool) {
        std::fs::create_dir_all(dir.join(name)).unwrap();
        if complete {
            std::fs::write(dir.join(name).join(MANIFEST), "{}").unwrap();
        }
    }

    #[test]
    fn scan_lists_complete_snapshots_and_current() {
        let dir = tempfile::tempdir().unwrap();
        let served = Served::under(dir.path());
        let debian = served.root(Family::Apt).join("debian");
        snapshot(&debian, "s2", true);
        snapshot(&debian, "s1", true);
        snapshot(&debian, ".sync-s3", true);
        swap_current(&debian, "s2").unwrap();
        let rocky = served.root(Family::Dnf).join("rocky9");
        snapshot(&rocky, "s1", true);
        // current names a snapshot still incomplete.
        snapshot(&rocky, "s2", false);
        swap_current(&rocky, "s2").unwrap();
        // Nothing complete, or not a mirror name.
        snapshot(&served.root(Family::Dnf).join("alma"), "s1", false);
        snapshot(&served.root(Family::Dnf).join("Bad"), "s1", true);

        let report = scan(&served, "http://relay.example:7480").unwrap();
        assert_eq!(report.public_url, "http://relay.example:7480");
        assert_eq!(
            report.mirrors,
            [
                ServedMirror {
                    family: "apt".into(),
                    name: "debian".into(),
                    current: "s2".into(),
                    snapshots: vec!["s1".into(), "s2".into()],
                },
                ServedMirror {
                    family: "dnf".into(),
                    name: "rocky9".into(),
                    current: String::new(),
                    snapshots: vec!["s1".into()],
                },
            ]
        );
        let empty = tempfile::tempdir().unwrap();
        assert!(scan(&Served::under(empty.path()), "http://r")
            .unwrap()
            .mirrors
            .is_empty());
    }
}
