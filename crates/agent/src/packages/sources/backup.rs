//! The backup of every file `apply` changes, and the file operations
//! behind it. One backup is active at a time, from the first `apply`
//! until `revert`: later applies add to its manifest, so MonitorFlow's own
//! files are never backed up as originals. Revert keeps the directory.
//!
//! Layout under the state directory, root-only:
//!
//! ```text
//! lock                      held while applying or reverting
//! active                    name of the active backup
//! <unix-time>/manifest.toml entries: path, action, mode, owner, sha256
//! <unix-time>/files/etc/... the original contents
//! ```
//!
//! The manifest and the copies are on disk, synced, before the first file
//! of the host changes, so a revert after a crash at any point restores
//! the originals.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ring::digest;
use serde::{Deserialize, Serialize};

use super::{Error, Family, Mode};

/// Relative to the root. The agent's own data directory belongs to the
/// agent user; this one must belong to root (ADR-0032).
pub(super) const STATE_DIR: &str = "var/lib/monitorflow/sources-backup";
const ACTIVE: &str = "active";
const LOCK: &str = "lock";
const MANIFEST: &str = "manifest.toml";
const FILES: &str = "files";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Action {
    /// An original set aside: restored on revert.
    Moved,
    /// An original rewritten with its repositories disabled.
    Disabled,
    /// An original at a path MonitorFlow writes.
    Overwritten,
    /// A MonitorFlow file: removed on revert.
    Created,
    /// A directory created for MonitorFlow files: removed if empty.
    CreatedDir,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Original {
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Entry {
    /// Absolute, as the package manager sees it.
    pub path: PathBuf,
    pub action: Action,
    /// For the actions that keep a copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original: Option<Original>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Manifest {
    family: Family,
    mode: Mode,
    created_at: u64,
    #[serde(default, rename = "entry")]
    entries: Vec<Entry>,
}

/// Holds the state directory's lock until dropped.
pub(super) struct Lock(#[allow(dead_code)] File);

/// Takes the lock, creating the state directory, and checks the
/// directory cannot be changed by anyone but this process's user.
pub(super) fn lock(root: &Path) -> Result<Lock, Error> {
    let dir = root.join(STATE_DIR);
    fs::create_dir_all(&dir).map_err(io_error(&dir))?;
    let meta = fs::symlink_metadata(&dir).map_err(io_error(&dir))?;
    if !meta.is_dir() || meta.uid() != rustix::process::geteuid().as_raw() {
        return Err(Error::Unsupported {
            path: dir,
            reason: "not a directory owned by this user".into(),
        });
    }
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).map_err(io_error(&dir))?;
    let path = dir.join(LOCK);
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
        .map_err(io_error(&path))?;
    file.lock().map_err(io_error(&path))?;
    Ok(Lock(file))
}

pub(super) struct Backup {
    root: PathBuf,
    /// Directory name; chosen on the first save.
    name: Option<String>,
    manifest: Manifest,
}

impl Backup {
    pub fn new(root: &Path, family: Family, mode: Mode) -> Self {
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        Self {
            root: root.to_owned(),
            name: None,
            manifest: Manifest {
                family,
                mode,
                created_at,
                entries: Vec::new(),
            },
        }
    }

    /// The active backup, if any. Call with the lock held.
    pub fn active(root: &Path) -> Result<Option<Self>, Error> {
        let state = root.join(STATE_DIR);
        let pointer = state.join(ACTIVE);
        let name = match fs::read_to_string(&pointer) {
            Ok(name) => name.trim().to_owned(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(Error::Io {
                    path: pointer,
                    source: error,
                })
            }
        };
        let corrupt = |path: &Path, reason: &str| Error::Backup {
            path: path.to_owned(),
            reason: reason.into(),
        };
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(corrupt(&pointer, "invalid backup name"));
        }
        let path = state.join(&name).join(MANIFEST);
        let text = fs::read_to_string(&path).map_err(io_error(&path))?;
        let manifest: Manifest =
            toml::from_str(&text).map_err(|e| corrupt(&path, &e.to_string()))?;
        for entry in &manifest.entries {
            let keeps_copy = !matches!(entry.action, Action::Created | Action::CreatedDir);
            if !is_etc_path(&entry.path) || keeps_copy != entry.original.is_some() {
                return Err(corrupt(&path, "invalid entry"));
            }
        }
        Ok(Some(Self {
            root: root.to_owned(),
            name: Some(name),
            manifest,
        }))
    }

    pub fn is_for(&self, family: Family, mode: Mode) -> bool {
        self.manifest.family == family && self.manifest.mode == mode
    }

    /// Absolute on disk, once saved.
    pub fn dir(&self) -> Option<PathBuf> {
        self.name
            .as_ref()
            .map(|name| self.root.join(STATE_DIR).join(name))
    }

    pub fn entry(&self, path: &Path) -> Option<&Entry> {
        self.manifest.entries.iter().find(|e| e.path == path)
    }

    /// Paths of MonitorFlow files, created or written over an original.
    pub fn own_files(&self) -> impl Iterator<Item = &Path> {
        self.manifest
            .entries
            .iter()
            .filter(|e| matches!(e.action, Action::Created | Action::Overwritten))
            .map(|e| e.path.as_path())
    }

    /// Records a path MonitorFlow creates.
    pub fn record_created(&mut self, path: &Path, action: Action) {
        self.manifest.entries.push(Entry {
            path: path.to_owned(),
            action,
            original: None,
        });
    }

    /// Copies the existing file at `path` into the backup and records it.
    pub fn record_original(&mut self, path: &Path, action: Action) -> Result<(), Error> {
        let disk = disk_path(&self.root, path);
        let Some((content, meta)) = read_regular(&disk)? else {
            return Err(Error::Io {
                path: disk,
                source: io::ErrorKind::NotFound.into(),
            });
        };
        let dir = self.ensure_dir()?;
        let copy = dir.join(FILES).join(relative(path));
        if let Some(parent) = copy.parent() {
            fs::create_dir_all(parent).map_err(io_error(parent))?;
        }
        write_atomic(&copy, &content, 0o600, None)?;
        self.manifest.entries.push(Entry {
            path: path.to_owned(),
            action,
            original: Some(Original {
                mode: meta.mode() & 0o7777,
                uid: meta.uid(),
                gid: meta.gid(),
                sha256: sha256(&content),
            }),
        });
        Ok(())
    }

    /// Writes the manifest, then marks the backup active.
    pub fn save(&mut self) -> Result<(), Error> {
        let dir = self.ensure_dir()?;
        let text = toml::to_string(&self.manifest).map_err(|e| Error::Backup {
            path: dir.clone(),
            reason: e.to_string(),
        })?;
        write_atomic(&dir.join(MANIFEST), text.as_bytes(), 0o600, None)?;
        let name = self.name.as_deref().unwrap_or_default();
        let pointer = self.root.join(STATE_DIR).join(ACTIVE);
        write_atomic(&pointer, format!("{name}\n").as_bytes(), 0o600, None)
    }

    /// Restores every original, removes MonitorFlow's files and
    /// deactivates the backup. Returns the paths it changed. Works from
    /// any state an interrupted apply left.
    pub fn restore(&self) -> Result<Vec<PathBuf>, Error> {
        let mut changed = Vec::new();
        // Files before the directories created for them.
        for entry in self.manifest.entries.iter().rev() {
            let disk = disk_path(&self.root, &entry.path);
            let did = match (&entry.action, &entry.original) {
                (Action::Created, _) => remove_file(&disk)?,
                (Action::CreatedDir, _) => match fs::remove_dir(&disk) {
                    Ok(()) => true,
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
                        ) =>
                    {
                        false
                    }
                    Err(e) => {
                        return Err(Error::Io {
                            path: disk,
                            source: e,
                        })
                    }
                },
                (_, Some(original)) => self.restore_original(&entry.path, original)?,
                (_, None) => unreachable!("checked on load"),
            };
            if did {
                changed.push(entry.path.clone());
            }
        }
        if self.name.is_some() {
            let pointer = self.root.join(STATE_DIR).join(ACTIVE);
            remove_file(&pointer)?;
            sync_parent(&pointer)?;
        }
        Ok(changed)
    }

    fn restore_original(&self, path: &Path, original: &Original) -> Result<bool, Error> {
        let disk = disk_path(&self.root, path);
        if let Some((content, meta)) = read_regular(&disk)? {
            let same_owner = (meta.uid(), meta.gid()) == (original.uid, original.gid);
            if sha256(&content) == original.sha256
                && meta.mode() & 0o7777 == original.mode
                && same_owner
            {
                return Ok(false);
            }
        }
        let copy = self
            .dir()
            .unwrap_or_default()
            .join(FILES)
            .join(relative(path));
        let content = fs::read(&copy).map_err(io_error(&copy))?;
        if sha256(&content) != original.sha256 {
            return Err(Error::Backup {
                path: copy,
                reason: "copy does not match the manifest".into(),
            });
        }
        write_atomic(
            &disk,
            &content,
            original.mode,
            Some((original.uid, original.gid)),
        )?;
        Ok(true)
    }

    fn ensure_dir(&mut self) -> Result<PathBuf, Error> {
        if self.name.is_none() {
            let state = self.root.join(STATE_DIR);
            let base = self.manifest.created_at.to_string();
            let mut name = base.clone();
            for n in 1.. {
                if !state.join(&name).exists() {
                    break;
                }
                name = format!("{base}-{n}");
            }
            let dir = state.join(&name);
            fs::create_dir(&dir).map_err(io_error(&dir))?;
            self.name = Some(name);
        }
        Ok(self.dir().unwrap_or_default())
    }
}

/// `path` (absolute, as the package manager sees it) under `root`.
pub(super) fn disk_path(root: &Path, path: &Path) -> PathBuf {
    root.join(relative(path))
}

fn relative(path: &Path) -> &Path {
    path.strip_prefix("/").unwrap_or(path)
}

fn is_etc_path(path: &Path) -> bool {
    let mut components = path.components();
    components.next() == Some(Component::RootDir)
        && components.next() == Some(Component::Normal("etc".as_ref()))
        && components.all(|c| matches!(c, Component::Normal(_)))
}

/// The content and metadata of a regular file; `None` when missing.
/// Symbolic links and other types are refused.
pub(super) fn read_regular(disk: &Path) -> Result<Option<(Vec<u8>, fs::Metadata)>, Error> {
    let meta = match fs::symlink_metadata(disk) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(Error::Io {
                path: disk.to_owned(),
                source: error,
            })
        }
    };
    if !meta.is_file() {
        return Err(Error::Unsupported {
            path: disk.to_owned(),
            reason: "not a regular file".into(),
        });
    }
    let content = fs::read(disk).map_err(io_error(disk))?;
    Ok(Some((content, meta)))
}

/// Replaces `path` through a synced temporary file in the same directory
/// and a rename, so readers see the old or the new content, never a mix.
pub(super) fn write_atomic(
    path: &Path,
    content: &[u8],
    mode: u32,
    owner: Option<(u32, u32)>,
) -> Result<(), Error> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    // apt and dnf only read `.list`, `.sources` and `.repo` files.
    let temp = path.with_file_name(format!(".{name}.monitorflow-tmp"));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(content)?;
        // The umask applies to `mode` at creation, not here.
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        if let Some((uid, gid)) = owner {
            let meta = file.metadata()?;
            if (meta.uid(), meta.gid()) != (uid, gid) {
                std::os::unix::fs::fchown(&file, Some(uid), Some(gid))?;
            }
        }
        file.sync_all()?;
        fs::rename(&temp, path)
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temp);
        return Err(Error::Io {
            path: path.to_owned(),
            source: error,
        });
    }
    sync_parent(path)
}

/// Removes a file if present; whether it was.
pub(super) fn remove_file(disk: &Path) -> Result<bool, Error> {
    match fs::remove_file(disk) {
        Ok(()) => sync_parent(disk).map(|()| true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(Error::Io {
            path: disk.to_owned(),
            source: error,
        }),
    }
}

fn sync_parent(path: &Path) -> Result<(), Error> {
    let parent = path.parent().unwrap_or(Path::new("/"));
    File::open(parent)
        .and_then(|dir| dir.sync_all())
        .map_err(io_error(parent))
}

pub(super) fn sha256(content: &[u8]) -> String {
    digest::digest(&digest::SHA256, content)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub(super) fn io_error(path: &Path) -> impl FnOnce(io::Error) -> Error + '_ {
    move |source| Error::Io {
        path: path.to_owned(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, path: &str, content: &str, mode: u32) {
        let disk = disk_path(root, Path::new(path));
        fs::create_dir_all(disk.parent().unwrap()).unwrap();
        fs::write(&disk, content).unwrap();
        fs::set_permissions(&disk, fs::Permissions::from_mode(mode)).unwrap();
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().mode() & 0o7777
    }

    #[test]
    fn write_atomic_sets_the_mode_and_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.list");
        write_atomic(&path, b"one", 0o640, None).unwrap();
        write_atomic(&path, b"two", 0o604, None).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert_eq!(mode(&path), 0o604);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_saved_backup_restores_originals_and_removes_created_files() {
        let root = tempfile::tempdir().unwrap();
        let r = root.path();
        write(r, "/etc/apt/sources.list", "original\n", 0o640);
        let _lock = lock(r).unwrap();
        let mut backup = Backup::new(r, Family::Apt, Mode::Replace);
        backup
            .record_original(Path::new("/etc/apt/sources.list"), Action::Moved)
            .unwrap();
        backup.record_created(Path::new("/etc/apt/keyrings"), Action::CreatedDir);
        backup.record_created(Path::new("/etc/apt/keyrings/k.asc"), Action::Created);
        backup.save().unwrap();
        // What an apply does.
        fs::remove_file(r.join("etc/apt/sources.list")).unwrap();
        write(r, "/etc/apt/keyrings/k.asc", "key", 0o644);

        let active = Backup::active(r).unwrap().unwrap();
        assert!(active.is_for(Family::Apt, Mode::Replace));
        assert_eq!(active.own_files().count(), 1);
        let changed = active.restore().unwrap();
        assert_eq!(changed.len(), 3);
        let list = r.join("etc/apt/sources.list");
        assert_eq!(fs::read_to_string(&list).unwrap(), "original\n");
        assert_eq!(mode(&list), 0o640);
        assert!(!r.join("etc/apt/keyrings").exists());
        assert!(Backup::active(r).unwrap().is_none());
        // The backup itself is kept.
        let state = r.join(STATE_DIR);
        assert_eq!(mode(&state), 0o700);
        assert_eq!(fs::read_dir(&state).unwrap().count(), 2);
    }

    #[test]
    fn restore_only_rewrites_what_differs() {
        let root = tempfile::tempdir().unwrap();
        let r = root.path();
        write(r, "/etc/yum.repos.d/a.repo", "[a]\n", 0o644);
        let _lock = lock(r).unwrap();
        let mut backup = Backup::new(r, Family::Dnf, Mode::Replace);
        backup
            .record_original(Path::new("/etc/yum.repos.d/a.repo"), Action::Disabled)
            .unwrap();
        backup.save().unwrap();
        assert!(backup.restore().unwrap().is_empty());
        // A changed mode alone is restored.
        let path = r.join("etc/yum.repos.d/a.repo");
        backup.save().unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(backup.restore().unwrap().len(), 1);
        assert_eq!(mode(&path), 0o644);
    }

    #[test]
    fn a_tampered_copy_or_manifest_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let r = root.path();
        write(r, "/etc/apt/sources.list", "original\n", 0o644);
        let _lock = lock(r).unwrap();
        let mut backup = Backup::new(r, Family::Apt, Mode::Replace);
        backup
            .record_original(Path::new("/etc/apt/sources.list"), Action::Moved)
            .unwrap();
        backup.save().unwrap();
        fs::remove_file(r.join("etc/apt/sources.list")).unwrap();
        let dir = backup.dir().unwrap();
        fs::write(dir.join("files/etc/apt/sources.list"), "evil\n").unwrap();
        assert!(matches!(backup.restore(), Err(Error::Backup { .. })));

        let manifest = dir.join(MANIFEST);
        let text = fs::read_to_string(&manifest).unwrap();
        fs::write(
            &manifest,
            text.replace("/etc/apt/sources.list", "/root/.ssh/x"),
        )
        .unwrap();
        assert!(matches!(Backup::active(r), Err(Error::Backup { .. })));
        fs::write(
            &manifest,
            text.replace("/etc/apt/sources.list", "/etc/../root/x"),
        )
        .unwrap();
        assert!(matches!(Backup::active(r), Err(Error::Backup { .. })));
    }

    #[test]
    fn a_state_directory_writable_by_others_is_fixed_and_a_symlink_refused() {
        let root = tempfile::tempdir().unwrap();
        let r = root.path();
        let state = r.join(STATE_DIR);
        fs::create_dir_all(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o777)).unwrap();
        drop(lock(r).unwrap());
        assert_eq!(mode(&state), 0o700);

        let other = tempfile::tempdir().unwrap();
        fs::remove_dir_all(&state).unwrap();
        std::os::unix::fs::symlink(other.path(), &state).unwrap();
        assert!(matches!(lock(r), Err(Error::Unsupported { .. })));
    }
}
