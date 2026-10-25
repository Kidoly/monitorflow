//! Mirror content as served over plain HTTP by the server (ADR-0030) and
//! by relays (ADR-0034): the fixed URL layout, the `/index.json` listing
//! what is published, and the atomic `current` link.
//!
//! With the `mirror` feature, `serve` holds the listener and `index` the
//! index built from the tree.

#[cfg(unix)]
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[cfg(feature = "mirror")]
pub mod index;
#[cfg(feature = "mirror")]
pub mod serve;

#[cfg(feature = "mirror")]
pub use index::{read_pinned, write_pinned, Index, IndexMirror, PinnedSnapshot};
#[cfg(feature = "mirror")]
pub use serve::Served;

/// Name of the link to the newest published snapshot of a mirror.
pub const CURRENT: &str = "current";

/// File name of the public signing key under `/keys/`.
pub const PUBLIC_KEY: &str = "monitorflow-archive.asc";

/// File in a mirror's directory naming, one per line, the snapshots
/// pinned by update campaigns (ADR-0038); hidden from the listener.
pub const PINNED: &str = ".pinned";

/// URL path of the index.
pub const INDEX_PATH: &str = "/index.json";

/// Longest mirror, snapshot or distribution name.
const MAX_NAME_LEN: usize = 64;

/// Repository format of a mirror, and its top-level URL segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Family {
    Apt,
    Dnf,
}

impl Family {
    pub const ALL: [Family; 2] = [Family::Apt, Family::Dnf];

    pub fn as_str(self) -> &'static str {
        match self {
            Family::Apt => "apt",
            Family::Dnf => "dnf",
        }
    }
}

impl std::fmt::Display for Family {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A mirror name as the server accepts it (ADR-0030): `[a-z0-9]` then up
/// to 63 of `[a-z0-9._-]`.
pub fn is_mirror_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c))
        && name.len() <= MAX_NAME_LEN
}

/// A snapshot name (ADR-0030), also used for apt distributions: 1 to 64
/// of `[A-Za-z0-9._-]`, not starting with a dot, and not [`CURRENT`].
/// Always a single safe path segment.
pub fn is_snapshot_name(name: &str) -> bool {
    (1..=MAX_NAME_LEN).contains(&name.len())
        && !name.starts_with('.')
        && name != CURRENT
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

/// Points `<dir>/current` at `snapshot`: a new link, then a rename over
/// the old one, so readers always see one or the other. Blocking. Unix
/// only, like the server and relays that hold mirrors.
#[cfg(unix)]
pub fn swap_current(dir: &Path, snapshot: &str) -> io::Result<()> {
    let tmp = dir.join(format!(".{CURRENT}.tmp"));
    match std::fs::remove_file(&tmp) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    std::os::unix::fs::symlink(snapshot, &tmp)?;
    std::fs::rename(&tmp, dir.join(CURRENT))
}

/// Writes `path` durably through `<path>.tmp`: the contents are synced
/// before the rename and the directory after it, so a crash leaves the
/// old file or the new one, never a truncated one. Blocking. Unix only,
/// like [`swap_current`].
#[cfg(unix)]
pub fn write_atomically(path: &Path, contents: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(contents)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)?;
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    std::fs::File::open(dir)?.sync_all()
}

/// The snapshot `<dir>/current` names, if it is a link to a valid name.
/// Blocking.
pub fn read_current(dir: &Path) -> Option<String> {
    let target = std::fs::read_link(dir.join(CURRENT)).ok()?;
    let name = target.to_str()?;
    is_snapshot_name(name).then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        for ok in ["debian", "rocky9", "a", "a.b_c-d"] {
            assert!(is_mirror_name(ok), "{ok}");
        }
        for bad in ["", "Debian", ".x", "-x", "a/b", "a:b", &"a".repeat(65)] {
            assert!(!is_mirror_name(bad), "{bad}");
        }
        for ok in ["20261023T090000Z", "bookworm", "noble-updates", "S1.x"] {
            assert!(is_snapshot_name(ok), "{ok}");
        }
        for bad in ["", "current", ".creating-s1", "a/b", "..", "a b"] {
            assert!(!is_snapshot_name(bad), "{bad}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn current_is_swapped_atomically() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("s1")).unwrap();
        std::fs::create_dir(dir.path().join("s2")).unwrap();
        assert_eq!(read_current(dir.path()), None);
        swap_current(dir.path(), "s1").unwrap();
        assert_eq!(read_current(dir.path()).as_deref(), Some("s1"));
        swap_current(dir.path(), "s2").unwrap();
        assert_eq!(read_current(dir.path()).as_deref(), Some("s2"));
        // Nothing temporary is left.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 3);
        std::os::unix::fs::symlink("../x", dir.path().join("bad")).unwrap();
        std::fs::rename(dir.path().join("bad"), dir.path().join(CURRENT)).unwrap();
        assert_eq!(read_current(dir.path()), None);
    }

    #[cfg(unix)]
    #[test]
    fn files_are_replaced_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("monitorflow-archive.asc");
        write_atomically(&path, b"one").unwrap();
        write_atomically(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        // Nothing temporary is left.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        assert!(write_atomically(&dir.path().join("missing/x"), b"x").is_err());
    }
}
