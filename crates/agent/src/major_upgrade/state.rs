//! The state of the last major upgrade (ADR-0182), in
//! `/var/lib/monitorflow/major-upgrade/state.json`: written by root only
//! (the helper and its unit), read by everyone, including the agent's
//! user, which works out what holds after a reboot.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use monitorflow_common::major_upgrade::{self, Phase};
use monitorflow_common::proto::v1::MajorUpgradeProgress;
use serde::{Deserialize, Serialize};

/// The state directory: root's, readable by all.
pub const STATE_DIR: &str = "/var/lib/monitorflow/major-upgrade";
const STATE: &str = "state.json";
/// The full output, root only.
pub const LOG: &str = "log";
/// Copies of the source files changed, root only.
pub const BACKUP: &str = "backup";
/// Where Linux tells the current boot apart from the others.
pub const BOOT_ID: &str = "/proc/sys/kernel/random/boot_id";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub command_id: String,
    pub from: String,
    pub to: String,
    pub phase: Phase,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub sources_restored: bool,
    #[serde(default)]
    pub disabled_sources: Vec<String>,
    /// leapp's report (`leapp-report.txt`), as it is, bounded.
    #[serde(default)]
    pub report: String,
    /// The boot the upgrade started in, then the one it asked to reboot.
    #[serde(default)]
    pub boot_id: String,
    pub started_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
}

impl State {
    pub fn new(command_id: &str, from: &str, to: &str, boot_id: &str, now: i64) -> Self {
        Self {
            command_id: command_id.to_owned(),
            from: from.to_owned(),
            to: to.to_owned(),
            phase: Phase::Starting,
            error: String::new(),
            sources_restored: false,
            disabled_sources: Vec::new(),
            report: String::new(),
            boot_id: boot_id.to_owned(),
            started_at_unix_ms: now,
            updated_at_unix_ms: now,
        }
    }

    /// What holds now, from a state written in another boot or not:
    /// `rebooting` on another boot is `done` once the host runs the
    /// target, else `failed`; any other unfinished phase on another boot
    /// was cut short by a restart.
    pub fn effective(&self, boot_id: &str, release: &str) -> Self {
        let mut state = self.clone();
        let rebooted = !boot_id.is_empty() && !self.boot_id.is_empty() && boot_id != self.boot_id;
        match self.phase {
            Phase::Rebooting if rebooted => {
                if major_upgrade::runs(release, &self.to) {
                    state.phase = Phase::Done;
                } else {
                    state.phase = Phase::Failed;
                    state.error = format!("still on {release} after the reboot, not {}", self.to);
                }
            }
            phase if !phase.is_final() && phase != Phase::Rebooting && rebooted => {
                state.phase = Phase::Failed;
                state.error = format!("interrupted by a restart while {}", doing(phase));
            }
            _ => {}
        }
        state
    }

    pub fn to_progress(&self) -> MajorUpgradeProgress {
        MajorUpgradeProgress {
            command_id: self.command_id.clone(),
            from: self.from.clone(),
            to: self.to.clone(),
            phase: self.phase.as_str().into(),
            error: self.error.clone(),
            sources_restored: self.sources_restored,
            disabled_sources: self.disabled_sources.clone(),
            report: self.report.clone(),
            started_at_unix_ms: self.started_at_unix_ms,
            updated_at_unix_ms: self.updated_at_unix_ms,
        }
    }
}

/// "upgrading packages", for an error.
pub fn doing(phase: Phase) -> &'static str {
    match phase {
        Phase::Starting => "starting",
        Phase::Preupgrade => "checking with leapp",
        Phase::Sources => "changing the sources",
        Phase::Refreshing => "refreshing the package lists",
        Phase::Upgrading | Phase::FullUpgrade => "upgrading packages",
        Phase::Rebooting => "rebooting",
        Phase::Done | Phase::Failed => "finishing",
    }
}

/// The state file under `dir`.
pub fn path(dir: &Path) -> PathBuf {
    dir.join(STATE)
}

/// The state, `None` when there is none. A file that does not parse is
/// an error: the caller decides whether that blocks.
pub fn read(dir: &Path) -> io::Result<Option<State>> {
    match fs::read(path(dir)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// The state directory, created root 0755 with root-only `backup/`; it
/// must belong to this process's user and be writable by no one else.
pub fn prepare(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    check_owned(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

/// Refuses a directory owned by someone else, a symlink, or one others
/// can write.
pub fn check_owned(dir: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(dir)?;
    let me = rustix::process::geteuid().as_raw();
    if !meta.is_dir() || meta.uid() != me || meta.mode() & 0o022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} must be a directory of this user that no one else can write",
                dir.display()
            ),
        ));
    }
    Ok(())
}

/// Writes `state` through a synced temporary file and a rename, 0644.
pub fn write(dir: &Path, state: &State) -> io::Result<()> {
    let target = path(dir);
    let temporary = dir.join(".state.json.new");
    let _ = fs::remove_file(&temporary);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(&temporary)?;
    file.write_all(&serde_json::to_vec_pretty(state).map_err(io::Error::other)?)?;
    file.sync_all()?;
    fs::rename(&temporary, &target)?;
    fs::File::open(dir)?.sync_all()
}

/// The current boot's ID, empty when unreadable.
pub fn boot_id() -> String {
    fs::read_to_string(BOOT_ID)
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(phase: Phase) -> State {
        let mut state = State::new("c1", "debian 12", "debian 13", "boot-a", 1);
        state.phase = phase;
        state
    }

    #[test]
    fn a_reboot_into_the_target_is_done() {
        let done = state(Phase::Rebooting).effective("boot-b", "debian 13");
        assert_eq!(done.phase, Phase::Done);
        let failed = state(Phase::Rebooting).effective("boot-b", "debian 12");
        assert_eq!(failed.phase, Phase::Failed);
        assert_eq!(
            failed.error,
            "still on debian 12 after the reboot, not debian 13"
        );
        // Before the reboot, still rebooting.
        let same = state(Phase::Rebooting).effective("boot-a", "debian 13");
        assert_eq!(same.phase, Phase::Rebooting);
    }

    #[test]
    fn a_restart_during_the_upgrade_fails_it() {
        let cut = state(Phase::Upgrading).effective("boot-b", "debian 13");
        assert_eq!(cut.phase, Phase::Failed);
        assert_eq!(
            cut.error,
            "interrupted by a restart while upgrading packages"
        );
        let running = state(Phase::Upgrading).effective("boot-a", "debian 12");
        assert_eq!(running.phase, Phase::Upgrading);
        // Final states stay as they are.
        let mut failed = state(Phase::Failed);
        failed.error = "apt".into();
        assert_eq!(failed.effective("boot-b", "debian 12"), failed);
    }

    #[test]
    fn writes_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(dir.path()).unwrap(), None);
        let s = state(Phase::Sources);
        write(dir.path(), &s).unwrap();
        assert_eq!(read(dir.path()).unwrap(), Some(s));
        let mode = fs::metadata(path(dir.path())).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o644);
        fs::write(path(dir.path()), "{").unwrap();
        assert!(read(dir.path()).is_err());
    }

    #[test]
    fn refuses_a_directory_others_can_write() {
        let dir = tempfile::tempdir().unwrap();
        prepare(dir.path()).unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o777)).unwrap();
        assert!(check_owned(dir.path()).is_err());
    }
}
