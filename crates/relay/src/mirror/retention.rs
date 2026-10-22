//! Bounded retention of a mirror's snapshots on the relay (ADR-0034).

use std::io;
use std::path::Path;
use std::time::SystemTime;

use monitorflow_common::mirror::read_current;

use super::sync::{complete_snapshots, MANIFEST};

/// Keeps the snapshot `current` names and the most recently completed
/// others, `keep` in all, plus the `pinned` ones (ADR-0038), and removes
/// the rest along with any staging directory (`.sync-*`): call it after a
/// successful sync, when none is in progress. Returns the removed
/// snapshots. Blocking.
pub fn prune(dir: &Path, keep: usize, pinned: &[String]) -> io::Result<Vec<String>> {
    let current = read_current(dir);
    let mut others: Vec<(SystemTime, String)> = complete_snapshots(dir)
        .into_iter()
        .filter(|s| Some(s) != current.as_ref() && !pinned.contains(s))
        .map(|s| {
            let done = std::fs::metadata(dir.join(&s).join(MANIFEST))
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            (done, s)
        })
        .collect();
    others.sort_by(|a, b| b.cmp(a));
    let kept = keep.saturating_sub(usize::from(current.is_some()));
    let mut removed = Vec::new();
    for (_, snapshot) in others.into_iter().skip(kept) {
        std::fs::remove_dir_all(dir.join(&snapshot))?;
        removed.push(snapshot);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with(".sync-") {
            std::fs::remove_dir_all(entry.path())?;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use monitorflow_common::mirror::swap_current;

    use super::*;

    fn snapshot(dir: &Path, name: &str) {
        std::fs::create_dir_all(dir.join(name).join("pool")).unwrap();
        std::fs::write(dir.join(name).join(MANIFEST), "{\"files\":[]}").unwrap();
        // Distinct completion times.
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    #[test]
    fn keeps_current_and_the_newest() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        for s in ["s1", "s2", "s3", "s4"] {
            snapshot(d, s);
        }
        std::fs::create_dir_all(d.join(".sync-s5/pool")).unwrap();
        std::fs::create_dir_all(d.join("partial")).unwrap();
        // current names an older snapshot, as after a rollback upstream.
        swap_current(d, "s2").unwrap();

        let mut removed = prune(d, 2, &[]).unwrap();
        removed.sort();
        assert_eq!(removed, ["s1", "s3"]);
        assert!(d.join("s2").is_dir() && d.join("s4").is_dir());
        assert!(!d.join(".sync-s5").exists());
        assert!(d.join("partial").is_dir(), "not a complete snapshot");
        assert_eq!(read_current(d).as_deref(), Some("s2"));

        // Pinned snapshots are kept on top of the count.
        assert!(prune(d, 1, &["s4".into()]).unwrap().is_empty());
        assert_eq!(prune(d, 1, &[]).unwrap(), ["s4"]);
        assert!(prune(d, 1, &[]).unwrap().is_empty());
        assert!(d.join("s2").is_dir());
    }
}
