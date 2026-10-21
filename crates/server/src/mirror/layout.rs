//! Files under `mirror_dir` (ADR-0030). The HTTP listener serves
//! `aptly/public/` as `/apt/`, `dnf/` as `/dnf/` and `keys/` as `/keys/`;
//! everything else stays private.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::model::{Family, Mirror, CURRENT};

/// File name of the public signing key under `keys/`.
pub const PUBLIC_KEY: &str = "monitorflow-archive.asc";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// aptly's `rootDir`: its database, package pool and `public/`.
    pub fn aptly_root(&self) -> PathBuf {
        self.root.join("aptly")
    }

    pub fn aptly_config(&self) -> PathBuf {
        self.root.join("aptly.conf")
    }

    /// Upstream archive keys aptly checks `Release` files with; filled by
    /// the administrator.
    pub fn upstream_keyring(&self) -> PathBuf {
        self.root.join("upstream-keyring.gpg")
    }

    /// `GNUPGHOME` holding the signing key.
    pub fn gnupg(&self) -> PathBuf {
        self.root.join("gnupg")
    }

    pub fn keys(&self) -> PathBuf {
        self.root.join("keys")
    }

    pub fn public_key(&self) -> PathBuf {
        self.keys().join(PUBLIC_KEY)
    }

    /// Served directory of the family: `aptly/public` or `dnf`.
    pub fn served(&self, family: Family) -> PathBuf {
        match family {
            Family::Apt => self.aptly_root().join("public"),
            Family::Dnf => self.root.join("dnf"),
        }
    }

    /// Directory of a mirror's snapshots and of its `current` link.
    pub fn mirror_dir(&self, mirror: &Mirror) -> PathBuf {
        self.served(mirror.family).join(&mirror.name)
    }

    /// reposync's working tree of a dnf mirror; not served.
    pub fn dnf_work(&self, mirror: &Mirror) -> PathBuf {
        self.root.join("dnf-work").join(&mirror.name)
    }

    /// Creates the directories and writes aptly's configuration. Blocking.
    pub fn prepare(&self) -> io::Result<()> {
        for dir in [
            self.served(Family::Apt),
            self.served(Family::Dnf),
            self.keys(),
        ] {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::create_dir_all(self.root.join("dnf-work"))?;
        let gnupg = self.gnupg();
        std::fs::create_dir_all(&gnupg)?;
        std::fs::set_permissions(&gnupg, std::fs::Permissions::from_mode(0o700))?;
        let config = serde_json::json!({
            "rootDir": self.aptly_root(),
            "downloadConcurrency": 4,
            "architectures": [],
            "dependencyFollowSuggests": false,
            "gpgDisableSign": false,
            "gpgDisableVerify": false,
            "gpgProvider": "gpg",
            "skipContentsPublishing": false,
        });
        write_atomically(&self.aptly_config(), config.to_string().as_bytes())
    }
}

/// Writes `path` through a temporary file and a rename.
pub fn write_atomically(path: &Path, contents: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(tmp, path)
}

/// Points `<dir>/current` at `snapshot`: a new link, then a rename over
/// the old one, so readers always see one or the other. Blocking.
pub fn swap_current(dir: &Path, snapshot: &str) -> io::Result<()> {
    let tmp = dir.join(format!(".{CURRENT}.tmp"));
    match std::fs::remove_file(&tmp) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    std::os::unix::fs::symlink(snapshot, &tmp)?;
    std::fs::rename(&tmp, dir.join(CURRENT))
}

/// Apparent size of the files under `path`, without following symlinks;
/// hardlinked files count each time. Blocking.
pub fn tree_size(path: &Path) -> io::Result<u64> {
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_dir() {
        return Ok(if meta.is_file() { meta.len() } else { 0 });
    }
    let mut total = 0;
    for entry in std::fs::read_dir(path)? {
        total += tree_size(&entry?.path())?;
    }
    Ok(total)
}

/// Rebuilds `src` in `dst` (which must not exist): files hardlinked and
/// made read-only, so an in-place rewrite of the source fails rather than
/// changing the copy; `copied` directory names (`repodata`) are copied
/// instead. Falls back to copying across file systems. Dot files are
/// skipped. Blocking.
pub fn link_tree(src: &Path, dst: &Path, copied: &[&str]) -> io::Result<()> {
    std::fs::create_dir(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let (from, to) = (entry.path(), dst.join(&name));
        let kind = entry.file_type()?;
        if kind.is_dir() {
            if copied.iter().any(|c| name == *c) {
                copy_tree(&from, &to)?;
            } else {
                link_tree(&from, &to, copied)?;
            }
        } else if kind.is_file() {
            std::fs::set_permissions(&from, std::fs::Permissions::from_mode(0o444))?;
            match std::fs::hard_link(&from, &to) {
                Err(e) if e.raw_os_error() == Some(rustix::io::Errno::XDEV.raw_os_error()) => {
                    std::fs::copy(&from, &to).map(drop)?
                }
                other => other?,
            }
        }
    }
    Ok(())
}

fn copy_tree(src: &Path, dst: &Path) -> io::Result<()> {
    std::fs::create_dir(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &to)?;
        } else if entry.file_type()?.is_file() {
            std::fs::copy(entry.path(), to)?;
        }
    }
    Ok(())
}

/// Removes `path` and its content if it exists. Blocking.
pub fn remove_tree(path: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt;

    use super::*;

    #[test]
    fn link_tree_links_packages_and_copies_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let (src, dst) = (dir.path().join("work"), dir.path().join("snap"));
        std::fs::create_dir_all(src.join("Packages/a")).unwrap();
        std::fs::create_dir_all(src.join("repodata")).unwrap();
        std::fs::write(src.join("Packages/a/a-1-1.x86_64.rpm"), "rpm").unwrap();
        std::fs::write(src.join("repodata/repomd.xml"), "<repomd/>").unwrap();
        std::fs::write(src.join(".partial"), "x").unwrap();
        link_tree(&src, &dst, &["repodata"]).unwrap();

        let ino = |p: &Path| std::fs::metadata(p).unwrap().ino();
        let rpm = "Packages/a/a-1-1.x86_64.rpm";
        assert_eq!(ino(&src.join(rpm)), ino(&dst.join(rpm)));
        let mode = std::fs::metadata(dst.join(rpm)).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o444);
        assert_ne!(
            ino(&src.join("repodata/repomd.xml")),
            ino(&dst.join("repodata/repomd.xml"))
        );
        assert!(!dst.join(".partial").exists());
        assert_eq!(tree_size(&dst).unwrap(), 3 + 9);
        assert!(
            link_tree(&src, &dst, &[]).is_err(),
            "the target must be new"
        );
        remove_tree(&dst).unwrap();
        remove_tree(&dst).unwrap();
    }

    #[test]
    fn current_is_swapped_atomically() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("s1")).unwrap();
        std::fs::create_dir(dir.path().join("s2")).unwrap();
        swap_current(dir.path(), "s1").unwrap();
        assert_eq!(
            std::fs::read_link(dir.path().join("current")).unwrap(),
            Path::new("s1")
        );
        swap_current(dir.path(), "s2").unwrap();
        assert_eq!(
            std::fs::read_link(dir.path().join("current")).unwrap(),
            Path::new("s2")
        );
        // The link is not counted, and nothing temporary is left.
        assert_eq!(tree_size(dir.path()).unwrap(), 0);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 3);
    }

    #[test]
    fn prepare_writes_the_aptly_config_and_a_private_gnupg_home() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        layout.prepare().unwrap();
        layout.prepare().unwrap();
        let config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(layout.aptly_config()).unwrap()).unwrap();
        assert_eq!(config["rootDir"], layout.aptly_root().to_str().unwrap());
        let mode = std::fs::metadata(layout.gnupg()).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o700);
        assert!(layout.served(Family::Apt).is_dir());
        assert!(layout.served(Family::Dnf).is_dir());
    }
}
