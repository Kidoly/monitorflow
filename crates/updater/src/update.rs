//! One update, from a request to its result (ADR-0056), and the recovery
//! of an update the updater's own restart interrupted.

use std::path::{Path, PathBuf};

use chrono::Utc;
use monitorflow_common::release::{self, Version};
use monitorflow_common::server_update::{
    read_small, write_atomic, Candidate, Request, Running, Status, Step, UpdateRun, UpdateState,
    MAX_STATE_FILE, STATUS_FILE,
};
use serde::{Deserialize, Serialize};

use monitorflow_common::server_update::backup::BackupKind;

use crate::env_file::{EnvFile, Pins};
use crate::host::Host;
use crate::source::Source;

/// The updater's private record of an update that changed `.env`, in its
/// own volume: what to roll back to after a restart.
const PENDING_FILE: &str = "pending.json";
/// Release files staged for the agent cache, under the update volume.
const RELEASE_DIR: &str = "release";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pending {
    to: String,
    previous: Pins,
    migration_before: i64,
    phase: Phase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    /// `.env` names the new images; the new server is not confirmed yet.
    Switching,
    /// The new server runs and reports the target version.
    Confirmed,
}

/// Keeps `status.json` in the update volume up to date.
pub struct Reporter {
    dir: PathBuf,
    pub status: Status,
}

impl Reporter {
    /// Starts from the status already there, so the last update stays
    /// visible across restarts.
    pub fn open(dir: &Path) -> Self {
        let previous = read_small(&dir.join(STATUS_FILE), MAX_STATE_FILE)
            .ok()
            .flatten()
            .and_then(|bytes| serde_json::from_slice::<Status>(&bytes).ok());
        let (update, backups) = previous.map(|s| (s.update, s.backups)).unwrap_or_default();
        Self {
            dir: dir.to_owned(),
            status: Status {
                updater_version: monitorflow_common::VERSION.to_owned(),
                heartbeat_at: Utc::now(),
                running: None,
                update,
                backups,
            },
        }
    }

    pub fn write(&mut self) {
        self.status.heartbeat_at = Utc::now();
        let bytes = serde_json::to_vec_pretty(&self.status).expect("status serializes");
        if let Err(e) = write_atomic(&self.dir, STATUS_FILE, &bytes, 0o644) {
            tracing::error!(error = %e, "cannot write the status file");
        }
    }

    pub fn set_running(&mut self, running: Option<Running>) {
        self.status.running = running;
        self.write();
    }

    fn run(&mut self) -> &mut UpdateRun {
        self.status.update.as_mut().expect("an update is running")
    }

    fn begin(&mut self, request: &Request) {
        tracing::info!(id = %request.id, to = %request.version, by = %request.requested_by, "update requested");
        self.status.update = Some(UpdateRun {
            id: request.id.clone(),
            from: None,
            to: request.version.clone(),
            requested_by: request.requested_by.clone(),
            state: UpdateState::Verifying,
            steps: Vec::new(),
            error: None,
            backup: None,
            migrated: false,
            started_at: Utc::now(),
            finished_at: None,
        });
        self.write();
    }

    fn state(&mut self, state: UpdateState) {
        self.run().state = state;
        self.write();
    }

    pub(crate) fn step(&mut self, message: impl Into<String>) {
        let message = message.into();
        tracing::info!("{message}");
        self.run().steps.push(Step {
            at: Utc::now(),
            message,
        });
        self.write();
    }

    fn finish(&mut self, state: UpdateState, error: Option<String>) -> UpdateState {
        if let Some(error) = &error {
            tracing::warn!(%error, ?state, "update ended");
        }
        let run = self.run();
        run.state = state;
        run.error = error;
        run.finished_at = Some(Utc::now());
        self.write();
        state
    }
}

/// Everything an update works with.
pub struct Updater<'a> {
    pub host: &'a dyn Host,
    pub source: &'a dyn Source,
    pub env: EnvFile,
    /// The update volume, as this container sees it.
    pub update_dir: PathBuf,
    /// The same volume in the server container.
    pub server_update_dir: PathBuf,
    /// The updater's own volume.
    pub state_dir: PathBuf,
    /// Where backups go.
    pub backup_dir: PathBuf,
    /// The server's data volume, which backups include.
    pub serverdata_dir: PathBuf,
    pub reporter: Reporter,
}

impl Updater<'_> {
    /// The running server, for the status.
    pub async fn running(&self) -> Option<Running> {
        let version = self.host.server_version().await.ok()?;
        let pins = self.env.pins().unwrap_or_default();
        Some(Running {
            version: version.to_string(),
            server_image: pins.server,
            web_image: pins.web,
            updater_image: pins.updater,
        })
    }

    /// Updates to `target`, as `request` asks, and returns how it ended.
    pub async fn update(&mut self, request: &Request, target: &Version) -> UpdateState {
        self.reporter.begin(request);
        let fail = |e: &dyn std::fmt::Display| Some(e.to_string());

        let current = match self.host.server_version().await {
            Ok(version) => version,
            Err(e) => {
                let error = format!("cannot ask the running server its version: {e}");
                return self.reporter.finish(UpdateState::Failed, Some(error));
            }
        };
        self.reporter.run().from = Some(current.to_string());
        match release::check_upgrade(&current, target, false) {
            Ok(true) => {}
            Ok(false) => {
                let error = format!("{target} is already running");
                return self.reporter.finish(UpdateState::Failed, Some(error));
            }
            Err(e) => return self.reporter.finish(UpdateState::Failed, fail(&e)),
        }
        let candidate = match self.source.candidate(target).await {
            Ok(candidate) => candidate,
            Err(e) => return self.reporter.finish(UpdateState::Failed, fail(&e)),
        };
        self.reporter.step(format!(
            "release {target} verified: server {}, web {}, updater {}",
            candidate.images.server, candidate.images.web, candidate.images.updater
        ));

        self.reporter.state(UpdateState::Downloading);
        let staged = self.stage_agent_release(&candidate).await;
        for image in [
            &candidate.images.server,
            &candidate.images.web,
            &candidate.images.updater,
        ] {
            if let Err(e) = self.host.pull(image).await {
                return self.reporter.finish(UpdateState::Failed, fail(&e));
            }
        }
        self.reporter.step("images pulled");

        self.reporter.state(UpdateState::BackingUp);
        let migration_before = match self.host.latest_migration().await {
            Ok(version) => version,
            Err(e) => return self.reporter.finish(UpdateState::Failed, fail(&e)),
        };
        let name = match self
            .backup(
                BackupKind::Update,
                request.id.clone(),
                &request.requested_by,
            )
            .await
        {
            Ok(name) => name,
            Err(e) => {
                let error = format!("backup failed: {e}");
                return self.reporter.finish(UpdateState::Failed, Some(error));
            }
        };
        self.reporter.run().backup = Some(name.clone());
        self.reporter
            .step(format!("database and server data saved to {name}"));

        let previous = match self.env.pins() {
            Ok(pins) => pins,
            Err(e) => return self.reporter.finish(UpdateState::Failed, fail(&e)),
        };
        let mut pending = Pending {
            to: target.to_string(),
            previous,
            migration_before,
            phase: Phase::Switching,
        };
        if let Err(e) = self.save_pending(&pending) {
            return self.reporter.finish(UpdateState::Failed, Some(e));
        }
        self.reporter.state(UpdateState::Updating);
        if let Err(e) = self.env.set_pins(&Pins::from(&candidate.images)) {
            self.clear_pending();
            let error = format!("cannot write {}: {e}", self.env.path().display());
            return self.reporter.finish(UpdateState::Failed, Some(error));
        }
        self.reporter
            .step("new images pinned; recreating server and web");
        if let Err(e) = self.host.recreate().await {
            return self
                .rollback(&pending, format!("the new version did not start: {e}"))
                .await;
        }

        self.reporter.state(UpdateState::Checking);
        match self.host.server_version().await {
            Ok(version) if version == *target => {}
            Ok(version) => {
                let reason = format!("the server reports {version}, not {target}");
                return self.rollback(&pending, reason).await;
            }
            Err(e) => {
                let reason = format!("cannot ask the new server its version: {e}");
                return self.rollback(&pending, reason).await;
            }
        }
        self.reporter.step(format!("server {target} is healthy"));
        pending.phase = Phase::Confirmed;
        if let Err(e) = self.save_pending(&pending) {
            tracing::warn!(error = %e, "cannot record the confirmed update");
        }
        self.complete(&pending, staged).await
    }

    /// Writes the whole release into the update volume, for the server's
    /// agent cache: the agent executables for self-updates, the installer
    /// files and the MSI for installs from the server (ADR-0054). Not
    /// fatal: the release can still be imported by hand.
    async fn stage_agent_release(&mut self, candidate: &Candidate) -> bool {
        let dir = self.release_dir(&candidate.release.version);
        let _ = std::fs::remove_dir_all(&dir);
        let written = std::fs::create_dir_all(&dir).and_then(|()| {
            std::fs::write(dir.join(release::SUMS), &candidate.sums)?;
            std::fs::write(dir.join(release::SIGNATURE), &candidate.signature)?;
            std::fs::write(dir.join(release::VERSION_FILE), &candidate.version_file)
        });
        let result = match written {
            Err(e) => Err(e.to_string()),
            Ok(()) => self
                .source
                .release_files(candidate, &dir)
                .await
                .map_err(|e| e.to_string()),
        };
        match result {
            Ok(names) if names.iter().any(|n| crate::source::is_agent(n)) => {
                self.reporter
                    .step(format!("agent release downloaded: {}", names.join(", ")));
                true
            }
            Ok(_) => {
                self.reporter
                    .step("warning: the release has no agent executables");
                false
            }
            Err(e) => {
                self.reporter
                    .step(format!("warning: agent release not downloaded: {e}"));
                false
            }
        }
    }

    fn release_dir(&self, version: &impl std::fmt::Display) -> PathBuf {
        self.update_dir.join(RELEASE_DIR).join(version.to_string())
    }

    /// After the new server is confirmed: the agent cache, then the
    /// updater itself.
    async fn complete(&mut self, pending: &Pending, staged: bool) -> UpdateState {
        self.reporter.state(UpdateState::CachingAgents);
        let dir = self.release_dir(&pending.to);
        if staged {
            let server_dir = self.server_update_dir.join(RELEASE_DIR).join(&pending.to);
            match self.host.import_agent_release(&server_dir).await {
                Ok(()) => self
                    .reporter
                    .step(format!("agent release {} cached for agents", pending.to)),
                Err(e) => self
                    .reporter
                    .step(format!("warning: agent release not cached: {e}")),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        self.clear_pending();
        let state = self.reporter.finish(UpdateState::Succeeded, None);
        let new = self.env.pins().unwrap_or_default();
        if new.updater != pending.previous.updater {
            self.reporter
                .step("replacing the updater with the new release's");
            if let Err(e) = self.host.replace_updater().await {
                self.reporter
                    .step(format!("warning: the updater was not replaced: {e}"));
            }
        }
        state
    }

    async fn rollback(&mut self, pending: &Pending, reason: String) -> UpdateState {
        self.reporter.step(reason.clone());
        self.reporter.state(UpdateState::RollingBack);
        match self.env.set_pins(&pending.previous) {
            Ok(()) => self.reporter.step("previous images pinned again"),
            Err(e) => self.reporter.step(format!(
                "cannot restore the previous images in {}: {e}",
                self.env.path().display()
            )),
        }
        match self.host.recreate().await {
            Ok(()) => self.reporter.step("previous version restarted"),
            Err(e) => self
                .reporter
                .step(format!("the previous version did not start: {e}")),
        }
        let migrated = match self.host.latest_migration().await {
            Ok(after) => after != pending.migration_before,
            Err(e) => {
                self.reporter
                    .step(format!("cannot tell whether migrations ran: {e}"));
                true
            }
        };
        self.reporter.run().migrated = migrated;
        if migrated {
            let backup = self.reporter.run().backup.clone().unwrap_or_default();
            self.reporter.step(format!(
                "the new version migrated the database, which the previous one cannot use: \
                 restore {backup} with deploy/restore.sh"
            ));
        }
        let _ = std::fs::remove_dir_all(self.release_dir(&pending.to));
        self.clear_pending();
        self.reporter.finish(UpdateState::RolledBack, Some(reason))
    }

    /// Finishes an update interrupted by a restart of the updater: rolled
    /// back unless the new server was already confirmed.
    pub async fn recover(&mut self) -> Option<UpdateState> {
        let bytes = read_small(&self.state_dir.join(PENDING_FILE), MAX_STATE_FILE).ok()??;
        let Ok(pending) = serde_json::from_slice::<Pending>(&bytes) else {
            self.clear_pending();
            return None;
        };
        if self.reporter.status.update.is_none() {
            self.reporter.begin(&Request {
                id: "recovered".into(),
                version: pending.to.clone(),
                requested_by: String::new(),
                requested_at: Utc::now(),
            });
        }
        Some(match pending.phase {
            Phase::Switching => {
                let reason = "the updater restarted before the new version was confirmed".into();
                self.rollback(&pending, reason).await
            }
            Phase::Confirmed => {
                let staged = self
                    .release_dir(&pending.to)
                    .join(release::VERSION_FILE)
                    .exists();
                self.complete(&pending, staged).await
            }
        })
    }

    fn save_pending(&self, pending: &Pending) -> Result<(), String> {
        let bytes = serde_json::to_vec(pending).expect("serializes");
        write_atomic(&self.state_dir, PENDING_FILE, &bytes, 0o600)
            .map_err(|e| format!("cannot record the update: {e}"))
    }

    fn clear_pending(&self) {
        let _ = std::fs::remove_file(self.state_dir.join(PENDING_FILE));
    }
}
