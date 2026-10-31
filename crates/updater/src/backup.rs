//! Backups (ADR-0058): a `.tar.gz` of the database dump and the server's
//! data volume, taken once a day, on request and before every update,
//! seven kept. GNU tar does the archiving: it keeps owners and modes, and
//! the updater image has it.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use monitorflow_common::server_update::backup::{
    self as names, BackupFile, BackupKind, BackupRun, BackupState, Manifest, DATABASE, FORMAT,
    KEEP, MANIFEST, SERVERDATA,
};
use monitorflow_common::server_update::UpdateState;
use tokio::process::Command;

use crate::update::Updater;

/// What backups leave out of the server's data: the mirror repositories,
/// synced again from upstream, and the agent release cache, filled again
/// by the next update. Mirror keys and configuration are kept.
pub const EXCLUDED: [&str; 5] = [
    "mirrors/aptly",
    "mirrors/dnf",
    "mirrors/dnf-work",
    "mirrors/agent",
    "mirrors/tools",
];
/// The automatic backup: once a day, an hour after a failure, and not in
/// the first minutes, while the deployment starts.
pub const DAILY: Duration = Duration::from_secs(24 * 60 * 60);
pub const RETRY: Duration = Duration::from_secs(60 * 60);
pub const FIRST_DELAY: Duration = Duration::from_secs(5 * 60);

/// GNU tar's status for "some files changed while being archived": the
/// archive is complete, with those files as they were read.
const TAR_CHANGED: i32 = 1;

impl Updater<'_> {
    /// Takes a backup, reports it in the status, and prunes old ones.
    /// Returns the file name.
    pub async fn backup(
        &mut self,
        kind: BackupKind,
        id: String,
        requested_by: &str,
    ) -> Result<String, String> {
        let started = Utc::now();
        let name = names::file_name(started, kind);
        self.reporter.status.backups.last = Some(BackupRun {
            id,
            kind,
            requested_by: requested_by.to_owned(),
            state: BackupState::Running,
            error: None,
            warning: None,
            file: None,
            started_at: started,
            finished_at: None,
        });
        self.reporter.write();
        let result = self.write_backup(&name, started, kind).await;
        let backups = &mut self.reporter.status.backups;
        let last = backups.last.as_mut().expect("set above");
        last.finished_at = Some(Utc::now());
        let result = match result {
            Ok(warning) => {
                tracing::info!(%name, ?warning, "backup saved");
                last.state = BackupState::Succeeded;
                last.file = Some(name.clone());
                last.warning = warning;
                backups.last_success_at = last.finished_at;
                self.prune();
                Ok(name)
            }
            Err(error) => {
                tracing::error!(%error, ?kind, "backup failed");
                last.state = BackupState::Failed;
                last.error = Some(error.clone());
                Err(error)
            }
        };
        self.refresh_backups();
        result
    }

    /// Writes the archive; `Ok` carries a warning to show.
    async fn write_backup(
        &self,
        name: &str,
        created_at: DateTime<Utc>,
        kind: BackupKind,
    ) -> Result<Option<String>, String> {
        remove_partial(&self.backup_dir);
        let staging = self.backup_dir.join(format!(".staging-{name}"));
        let part = self.backup_dir.join(format!(".{name}.part"));
        let result = async {
            std::fs::create_dir(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
            let migration = self
                .host
                .latest_migration()
                .await
                .map_err(|e| e.to_string())?;
            self.host
                .dump_database(&staging.join(DATABASE))
                .await
                .map_err(|e| format!("database dump failed: {e}"))?;
            let serverdata = self.serverdata_dir.is_dir();
            let manifest = Manifest {
                format: FORMAT,
                created_at,
                kind,
                server_version: self.host.server_version().await.ok().map(|v| v.to_string()),
                migration,
                serverdata,
            };
            let bytes = serde_json::to_vec_pretty(&manifest).expect("serializes");
            std::fs::write(staging.join(MANIFEST), bytes).map_err(|e| e.to_string())?;
            let changed =
                archive(&staging, serverdata.then_some(&*self.serverdata_dir), &part).await?;
            std::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o640))
                .and_then(|()| std::fs::File::open(&part)?.sync_all())
                .and_then(|()| std::fs::rename(&part, self.backup_dir.join(name)))
                .map_err(|e| format!("{}: {e}", part.display()))?;
            Ok(if !serverdata {
                Some(
                    "the server's data (its certificate authority and keys) is not included: \
                     deploy/docker-compose.yml predates backups, take the new one from the release"
                        .into(),
                )
            } else {
                changed.then(|| "some server files changed while they were saved".into())
            })
        }
        .await;
        let _ = std::fs::remove_dir_all(&staging);
        if result.is_err() {
            let _ = std::fs::remove_file(&part);
        }
        result
    }

    /// Deletes all but the [`KEEP`] newest backups, the dumps of updaters
    /// before backups included. The backup a rollback after migrations
    /// needs is kept whatever its age.
    pub fn prune(&self) {
        let needed = self
            .reporter
            .status
            .update
            .as_ref()
            .filter(|u| u.state == UpdateState::RolledBack && u.migrated)
            .and_then(|u| u.backup.clone());
        let mut found: Vec<(DateTime<Utc>, PathBuf)> = Vec::new();
        for (name, meta) in entries(&self.backup_dir) {
            let at = match names::parse_file_name(&name) {
                Some((at, _)) => at,
                None if is_legacy_dump(&name) => match meta.modified() {
                    Ok(time) => time.into(),
                    Err(_) => continue,
                },
                None => continue,
            };
            if needed.as_deref() != Some(name.as_str()) {
                found.push((at, self.backup_dir.join(name)));
            }
        }
        found.sort_by_key(|f| std::cmp::Reverse(f.0));
        let keep = KEEP - usize::from(needed.is_some());
        for (_, path) in found.into_iter().skip(keep) {
            match std::fs::remove_file(&path) {
                Ok(()) => tracing::info!(path = %path.display(), "old backup deleted"),
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "cannot delete an old backup")
                }
            }
        }
    }

    /// Lists the backups in the status, newest first.
    pub fn refresh_backups(&mut self) {
        let mut files: Vec<BackupFile> = entries(&self.backup_dir)
            .filter_map(|(name, meta)| {
                let (created_at, kind) = names::parse_file_name(&name)?;
                Some(BackupFile {
                    size: meta.len(),
                    name,
                    created_at,
                    kind,
                })
            })
            .collect();
        files.sort_by_key(|f| std::cmp::Reverse(f.created_at));
        let backups = &mut self.reporter.status.backups;
        if backups.last_success_at.is_none() {
            backups.last_success_at = files.first().map(|f| f.created_at);
        }
        backups.files = files;
        self.reporter.write();
    }

    /// A backup the updater's restart interrupted failed.
    pub fn recover_backup(&mut self) {
        if let Some(last) = self
            .reporter
            .status
            .backups
            .last
            .as_mut()
            .filter(|l| l.state == BackupState::Running)
        {
            last.state = BackupState::Failed;
            last.error = Some("interrupted by a restart of the updater".into());
            last.finished_at = Some(Utc::now());
        }
        remove_partial(&self.backup_dir);
        self.refresh_backups();
    }

    /// When the automatic backup is due, for an updater started at
    /// `started`.
    pub fn next_backup(&self, started: DateTime<Utc>) -> DateTime<Utc> {
        let backups = &self.reporter.status.backups;
        let due = match &backups.last {
            Some(run) if run.state == BackupState::Failed => {
                run.finished_at.unwrap_or(run.started_at) + RETRY
            }
            _ => backups.last_success_at.map_or(started, |at| at + DAILY),
        };
        due.max(started + FIRST_DELAY)
    }
}

/// Regular files of `dir` with their metadata, links skipped.
fn entries(dir: &Path) -> impl Iterator<Item = (String, std::fs::Metadata)> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let meta = entry.metadata().ok().filter(|m| m.is_file())?;
            Some((entry.file_name().into_string().ok()?, meta))
        })
}

/// `monitorflow-0.1.0-to-0.2.0-20261029T030000Z.dump`, from updaters
/// before backups (ADR-0056).
fn is_legacy_dump(name: &str) -> bool {
    name.starts_with("monitorflow-") && name.contains("-to-") && name.ends_with(".dump")
}

/// What an interrupted backup left.
fn remove_partial(dir: &Path) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(".staging-") {
            let _ = std::fs::remove_dir_all(entry.path());
        } else if name.starts_with('.') && name.ends_with(".part") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// `tar` with `args`; `Ok(true)` when files changed while being read.
async fn tar(args: &[&std::ffi::OsStr]) -> Result<bool, String> {
    let output = Command::new("tar")
        .args(args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("cannot run tar: {e}"))?;
    match output.status.code() {
        Some(0) => Ok(false),
        Some(TAR_CHANGED) if args.first() == Some(&"--create".as_ref()) => Ok(true),
        _ => {
            let text = String::from_utf8_lossy(&output.stderr);
            let tail: Vec<&str> = text.lines().rev().take(3).collect();
            Err(format!(
                "tar failed: {}",
                tail.into_iter().rev().collect::<Vec<_>>().join(" / ")
            ))
        }
    }
}

/// Writes `dest`: the manifest and the dump of `staging`, then the
/// content of `serverdata` under `serverdata/`, without [`EXCLUDED`].
async fn archive(staging: &Path, serverdata: Option<&Path>, dest: &Path) -> Result<bool, String> {
    let excludes: Vec<String> = EXCLUDED
        .iter()
        .map(|p| format!("--exclude=./{p}"))
        .collect();
    let mut args: Vec<&std::ffi::OsStr> = vec![
        "--create".as_ref(),
        "--gzip".as_ref(),
        "--numeric-owner".as_ref(),
        "--anchored".as_ref(),
        "--file".as_ref(),
        dest.as_os_str(),
        "--directory".as_ref(),
        staging.as_os_str(),
        MANIFEST.as_ref(),
        DATABASE.as_ref(),
    ];
    if let Some(dir) = serverdata {
        args.extend(excludes.iter().map(std::ffi::OsStr::new));
        args.extend::<[&std::ffi::OsStr; 4]>([
            // `./ca/key.pem` becomes `serverdata/ca/key.pem`; link
            // targets are left as they are.
            "--transform=s,^\\.,serverdata,S".as_ref(),
            "--directory".as_ref(),
            dir.as_os_str(),
            ".".as_ref(),
        ]);
    }
    tar(&args).await
}

/// Extracts the manifest and the dump of `archive` into `dest`.
pub async fn extract_database(archive: &Path, dest: &Path) -> Result<Manifest, String> {
    tar(&[
        "--extract".as_ref(),
        "--gzip".as_ref(),
        "--file".as_ref(),
        archive.as_os_str(),
        "--directory".as_ref(),
        dest.as_os_str(),
        MANIFEST.as_ref(),
        DATABASE.as_ref(),
    ])
    .await?;
    let bytes = std::fs::read(dest.join(MANIFEST)).map_err(|e| format!("{MANIFEST}: {e}"))?;
    let manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|e| format!("{MANIFEST}: {e}"))?;
    if manifest.format != FORMAT {
        return Err(format!(
            "backup format {} is not known to this updater, which reads {FORMAT}",
            manifest.format
        ));
    }
    Ok(manifest)
}

/// Replaces the content of `dir`, except [`EXCLUDED`], with the
/// `serverdata/` of `archive`, owners and modes included.
pub async fn extract_serverdata(archive: &Path, dir: &Path) -> Result<(), String> {
    clear(dir, "").map_err(|e| format!("emptying {}: {e}", dir.display()))?;
    tar(&[
        "--extract".as_ref(),
        "--gzip".as_ref(),
        "--numeric-owner".as_ref(),
        "--same-permissions".as_ref(),
        "--file".as_ref(),
        archive.as_os_str(),
        "--directory".as_ref(),
        dir.as_os_str(),
        "--strip-components=1".as_ref(),
        SERVERDATA.as_ref(),
    ])
    .await
    .map(drop)
}

/// Removes everything under `dir` but [`EXCLUDED`]; `prefix` is `dir`'s
/// path below the volume's root.
fn clear(dir: &Path, prefix: &str) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let relative = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let path = entry.path();
        if EXCLUDED.contains(&relative.as_str()) {
            continue;
        }
        let is_dir = entry.file_type()?.is_dir();
        if is_dir
            && EXCLUDED
                .iter()
                .any(|e| e.starts_with(&format!("{relative}/")))
        {
            clear(&path, &relative)?;
        } else if is_dir {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clearing_keeps_the_excluded_trees_only() {
        let dir = tempfile::TempDir::new().unwrap();
        for path in [
            "ca",
            "mirrors/aptly/db",
            "mirrors/gnupg",
            "mirrors/agent/0.2.0",
        ] {
            std::fs::create_dir_all(dir.path().join(path)).unwrap();
        }
        std::fs::write(dir.path().join("ca/key.pem"), "k").unwrap();
        std::fs::write(dir.path().join("mirrors/aptly.conf"), "c").unwrap();
        clear(dir.path(), "").unwrap();
        let mut left: Vec<String> = walk(dir.path());
        left.sort();
        assert_eq!(
            left,
            [
                "mirrors",
                "mirrors/agent",
                "mirrors/agent/0.2.0",
                "mirrors/aptly",
                "mirrors/aptly/db"
            ]
        );
    }

    fn walk(root: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_owned()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                out.push(path.strip_prefix(root).unwrap().display().to_string());
                if path.is_dir() {
                    stack.push(path);
                }
            }
        }
        out
    }

    #[test]
    fn legacy_dumps_are_recognised() {
        assert!(is_legacy_dump(
            "monitorflow-0.1.0-to-0.2.0-20261029T030000Z.dump"
        ));
        assert!(!is_legacy_dump(
            "monitorflow-backup-20261029T030000Z-daily.tar.gz"
        ));
        assert!(!is_legacy_dump("notes.dump"));
    }
}
