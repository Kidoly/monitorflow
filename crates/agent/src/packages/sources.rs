//! Points the host's package manager at its MonitorFlow mirror and undoes
//! it (ADR-0032, architecture §7).
//!
//! [`apply`] checks the whole spec first, signing key fingerprints
//! included, then records every file it will change in the active backup
//! before touching any of them. A failure midway, or of the
//! [`PostCheck`] after the change, reverts to the originals. Applying
//! the same spec again changes nothing. [`revert`] restores the originals
//! byte for byte, with their modes and owners, and removes MonitorFlow's
//! files.
//!
//! Everything happens under `root`, `/` on a host, so tests use a
//! temporary directory. Writing under `/etc` needs root: the agent runs
//! this through `sudo` and its `sources` subcommand.

mod apt;
mod backup;
mod key;
#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use monitorflow_common::proto::v1::configure_sources::{
    AptRepository, Family as ProtoFamily, Mode as ProtoMode,
};
use monitorflow_common::proto::v1::ConfigureSources;
use serde::{Deserialize, Serialize};

use backup::{disk_path, io_error, read_regular, Action, Backup};

/// First lines of every file MonitorFlow writes.
const HEADER: &str = "# Managed by MonitorFlow (ADR-0032). Changes are overwritten;\n\
                      # `monitorflow-agent sources revert` restores the previous sources.\n";
/// Mode of MonitorFlow's files and directories.
const FILE_MODE: u32 = 0o644;
const DIR_MODE: u32 = 0o755;
const MAX_TOKEN: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Family {
    Apt,
    Dnf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Replace,
    Add,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid sources spec: {0}")]
    Invalid(String),
    #[error("signing key `{name}` refused: {reason}")]
    Key { name: String, reason: String },
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{}: {reason}", path.display())]
    Unsupported { path: PathBuf, reason: String },
    #[error("backup {}: {reason}", path.display())]
    Backup { path: PathBuf, reason: String },
    #[error("post-check failed, sources reverted: {0}")]
    PostCheck(String),
    #[error("{cause}; reverting failed too: {rollback}")]
    RollbackFailed {
        cause: Box<Error>,
        rollback: Box<Error>,
    },
}

/// What `apply` or `revert` did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Paths changed on the host, as the package manager sees them.
    pub changed: Vec<PathBuf>,
    /// The backup directory on disk; none when nothing was ever applied.
    pub backup: Option<PathBuf>,
}

/// Checks the package manager works with the new sources, like
/// `apt-get update`. A failure reverts the change.
pub trait PostCheck {
    fn check(&self, family: Family) -> Result<(), String>;
}

/// The checked spec.
struct Spec {
    family: Family,
    mode: Mode,
    /// Without the trailing slash.
    base: String,
    apt: Vec<AptRepository>,
    keys: Vec<(String, key::PublicKey)>,
}

/// One change to the host, recorded in the backup before it runs.
enum Step {
    Mkdir(PathBuf),
    Write(PathBuf, Vec<u8>),
    /// Set aside: the copy in the backup is the only one left.
    Remove(PathBuf),
}

/// Points the package manager at the mirror of `spec`.
pub fn apply(
    spec: &ConfigureSources,
    root: &Path,
    checker: &dyn PostCheck,
) -> Result<Outcome, Error> {
    let spec = Spec::check(spec)?;
    let _lock = backup::lock(root)?;
    let mut changed = Vec::new();
    let mut backup = match Backup::active(root)? {
        Some(active) if active.is_for(spec.family, spec.mode) => active,
        Some(active) => {
            // Another family or mode starts again from the originals.
            changed = active.restore()?;
            Backup::new(root, spec.family, spec.mode)
        }
        None => Backup::new(root, spec.family, spec.mode),
    };
    let steps = plan(&spec, root, &mut backup)?;
    if steps.is_empty() {
        return Ok(Outcome {
            changed,
            backup: backup.dir(),
        });
    }
    backup.save()?;
    let result = steps
        .iter()
        .try_for_each(|step| run(root, step))
        .and_then(|()| checker.check(spec.family).map_err(Error::PostCheck));
    if let Err(cause) = result {
        return Err(match backup.restore() {
            Ok(_) => cause,
            Err(rollback) => Error::RollbackFailed {
                cause: Box::new(cause),
                rollback: Box::new(rollback),
            },
        });
    }
    tracing::info!(changed = steps.len(), "package sources applied");
    changed.extend(steps.iter().map(|step| step.path().to_owned()));
    Ok(Outcome {
        changed,
        backup: backup.dir(),
    })
}

/// Restores the sources `apply` changed. Nothing to revert is a success.
pub fn revert(root: &Path) -> Result<Outcome, Error> {
    let _lock = backup::lock(root)?;
    let Some(active) = Backup::active(root)? else {
        return Ok(Outcome::default());
    };
    let changed = active.restore()?;
    tracing::info!(changed = changed.len(), "package sources reverted");
    Ok(Outcome {
        changed,
        backup: active.dir(),
    })
}

/// The steps from the host's current state to `spec`'s, recording in
/// `backup` the paths they change that it has not seen yet.
fn plan(spec: &Spec, root: &Path, backup: &mut Backup) -> Result<Vec<Step>, Error> {
    let (files, dirs, others) = match spec.family {
        Family::Apt => (
            apt::files(spec),
            [apt::PARTS, apt::KEYRINGS],
            apt::others(root)?,
        ),
        Family::Dnf => unreachable!("refused by Spec::check"),
    };
    let mut steps = Vec::new();
    for dir in dirs {
        // Outermost first, so revert removes them innermost first.
        let missing: Vec<_> = Path::new(dir)
            .ancestors()
            .take_while(|dir| !disk_path(root, dir).exists())
            .collect();
        for dir in missing.into_iter().rev() {
            backup.record_created(dir, Action::CreatedDir);
            steps.push(Step::Mkdir(dir.to_owned()));
        }
    }
    let wanted: BTreeSet<PathBuf> = files.iter().map(|(path, _)| path.clone()).collect();
    // MonitorFlow files an earlier spec had and this one has not; an
    // original under one stays in the backup.
    let stale: Vec<PathBuf> = backup
        .own_files()
        .filter(|path| !wanted.contains(*path) && disk_path(root, path).exists())
        .map(Path::to_owned)
        .collect();
    for (path, content) in files {
        match read_regular(&disk_path(root, &path))? {
            Some((current, meta)) if current == content && meta.mode() & 0o7777 == FILE_MODE => {
                continue
            }
            Some(_) if backup.entry(&path).is_none() => {
                backup.record_original(&path, Action::Overwritten)?
            }
            None if backup.entry(&path).is_none() => backup.record_created(&path, Action::Created),
            _ => {}
        }
        steps.push(Step::Write(path, content));
    }
    let own: BTreeSet<PathBuf> = wanted.into_iter().chain(stale.iter().cloned()).collect();
    steps.extend(stale.into_iter().map(Step::Remove));
    if spec.mode == Mode::Add {
        return Ok(steps);
    }
    for path in others.into_iter().filter(|path| !own.contains(path)) {
        let Some((current, _)) = read_regular(&disk_path(root, &path))? else {
            continue;
        };
        let (step, action) = match spec.family {
            Family::Apt => (Step::Remove(path.clone()), Action::Moved),
            Family::Dnf => unreachable!("refused by Spec::check"),
        };
        match backup
            .entry(&path)
            .and_then(|entry| entry.original.as_ref())
        {
            None => backup.record_original(&path, action)?,
            // Set aside before, then changed by someone: its new content
            // has no copy, so it is not touched.
            Some(original) if original.sha256 != backup::sha256(&current) => {
                return Err(Error::Unsupported {
                    path,
                    reason: "changed since MonitorFlow set it aside; revert first".into(),
                })
            }
            Some(_) => {}
        }
        steps.push(step);
    }
    Ok(steps)
}

fn run(root: &Path, step: &Step) -> Result<(), Error> {
    let disk = disk_path(root, step.path());
    #[cfg(test)]
    tests::before_change(root).map_err(io_error(&disk))?;
    match step {
        Step::Mkdir(_) => {
            std::fs::create_dir_all(&disk).map_err(io_error(&disk))?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&disk, std::fs::Permissions::from_mode(DIR_MODE))
                .map_err(io_error(&disk))
        }
        Step::Write(_, content) => backup::write_atomic(&disk, content, FILE_MODE, None),
        Step::Remove(_) => backup::remove_file(&disk).map(drop),
    }
}

impl Step {
    fn path(&self) -> &Path {
        match self {
            Self::Mkdir(path) | Self::Write(path, _) | Self::Remove(path) => path,
        }
    }
}

/// Regular files of `dir` with one of `extensions`, sorted.
fn list_dir(root: &Path, dir: &str, extensions: &[&str]) -> Result<Vec<PathBuf>, Error> {
    let disk = disk_path(root, Path::new(dir));
    let entries = match std::fs::read_dir(&disk) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(Error::Io {
                path: disk,
                source: error,
            })
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(io_error(&disk))?;
        let path = Path::new(dir).join(entry.file_name());
        let wanted = path
            .extension()
            .is_some_and(|ext| extensions.iter().any(|e| ext == *e));
        // Directories are skipped; `read_regular` refuses other types.
        if wanted && !entry.file_type().map_err(io_error(&disk))?.is_dir() {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

impl Spec {
    fn check(spec: &ConfigureSources) -> Result<Self, Error> {
        let invalid = |message: String| Err(Error::Invalid(message));
        let family = match spec.family() {
            ProtoFamily::Apt => Family::Apt,
            ProtoFamily::Dnf => return invalid("dnf sources are not supported yet".into()),
            ProtoFamily::Unspecified => return invalid("family is required".into()),
        };
        let mode = match spec.mode() {
            ProtoMode::Replace => Mode::Replace,
            ProtoMode::Add => Mode::Add,
            ProtoMode::Unspecified => return invalid("mode is required".into()),
        };
        let base = spec.mirror_base_url.trim_end_matches('/');
        let scheme_ok = base.starts_with("https://") || base.starts_with("http://");
        let host = base.split_once("://").map_or("", |(_, rest)| rest);
        let bad_char = |c: char| c.is_whitespace() || c.is_control() || "?#\"'\\".contains(c);
        if !scheme_ok || host.is_empty() || host.starts_with('/') || base.contains(bad_char) {
            return invalid(format!("mirror base URL `{base}` is not an http(s) URL"));
        }
        let (count, other) = match family {
            Family::Apt => (spec.apt.len(), spec.dnf.len()),
            Family::Dnf => (spec.dnf.len(), spec.apt.len()),
        };
        if count == 0 || other != 0 {
            return invalid("repositories must be given for the family only".into());
        }
        for repo in &spec.apt {
            for (field, value) in [
                ("mirror", &repo.mirror),
                ("snapshot", &repo.snapshot),
                ("suite", &repo.suite),
            ] {
                token(field, value)?;
            }
            if repo.components.is_empty() {
                return invalid(format!("repository {} has no components", repo.mirror));
            }
            for value in repo.components.iter().chain(&repo.architectures) {
                token("component or architecture", value)?;
            }
        }
        let mut ids = BTreeSet::new();
        for repo in &spec.dnf {
            token("mirror", &repo.mirror)?;
            token("snapshot", &repo.snapshot)?;
            token("repo_id", &repo.repo_id)?;
            if !ids.insert(&repo.repo_id) {
                return invalid(format!("repo_id `{}` is repeated", repo.repo_id));
            }
            if repo.name.len() > 200 || repo.name.contains(|c: char| c.is_control()) {
                return invalid(format!("repository name of {} is invalid", repo.repo_id));
            }
        }
        if family == Family::Apt && spec.keys.is_empty() {
            return invalid("apt repositories need a signing key".into());
        }
        let mut keys: Vec<(String, key::PublicKey)> = Vec::new();
        for signing in &spec.keys {
            token("key name", &signing.name)?;
            if keys.iter().any(|(name, _)| *name == signing.name) {
                return invalid(format!("key name `{}` is repeated", signing.name));
            }
            let key = key::verify(&signing.armored, &signing.fingerprint).map_err(|reason| {
                Error::Key {
                    name: signing.name.clone(),
                    reason,
                }
            })?;
            keys.push((signing.name.clone(), key));
        }
        Ok(Self {
            family,
            mode,
            base: base.to_owned(),
            apt: spec.apt.clone(),
            keys,
        })
    }
}

/// A value used in a path, a URL or a field: letters, digits and
/// `._+~-`, not starting with a punctuation character.
fn token(field: &str, value: &str) -> Result<(), Error> {
    let starts_ok = value.starts_with(|c: char| c.is_ascii_alphanumeric());
    let chars_ok = value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._+~-".contains(c));
    if !starts_ok || !chars_ok || value.len() > MAX_TOKEN {
        return Err(Error::Invalid(format!("{field} `{value}` is invalid")));
    }
    Ok(())
}
