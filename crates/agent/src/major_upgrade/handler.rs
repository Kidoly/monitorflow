//! The agent's side, as its own user (ADR-0182): the `major_upgrade`
//! part of the package report, the `major_upgrade` command, which starts
//! the upgrade through `sudo -n /usr/bin/monitorflow-agent major-upgrade`,
//! and the watch that sends the report again at each phase.

use std::path::{Path, PathBuf};
use std::time::Duration;

use monitorflow_common::major_upgrade;
use monitorflow_common::proto::v1::{
    Command, CommandAcceptance, MajorUpgrade, MajorUpgradeReport, Package,
};
use prost::Message;

use super::checks::{self, Host};
use super::sources;
use super::state::{self, STATE_DIR};
use crate::commands::{Handler, Output};
use crate::helper_trust::HelperRequest;
use crate::packages::{PackageError, Reports, Tools};
use crate::shutdown::Shutdown;

/// How often a running upgrade's state is looked at.
pub const WATCH: Duration = Duration::from_secs(10);

/// The `major_upgrade` part of a report: the next release this agent
/// knows, the checks before upgrading to it, and the last upgrade.
/// `None` on a release without a known next one and no upgrade recorded.
pub async fn report(packages: &[Package]) -> Option<MajorUpgradeReport> {
    let upgradable = packages
        .iter()
        .filter(|p| !p.upgradable_version.is_empty())
        .count();
    tokio::task::spawn_blocking(move || build(upgradable, Path::new("/"), Path::new(STATE_DIR)))
        .await
        .ok()
        .flatten()
}

fn build(upgradable: usize, root: &Path, state_dir: &Path) -> Option<MajorUpgradeReport> {
    let host = crate::os::detect_from(None);
    let release = host.os_version;
    let progress = progress(state_dir, &release);
    let next = major_upgrade::next(&release);
    if next.is_none() && progress.is_none() {
        return None;
    }
    let mut report = MajorUpgradeReport {
        release: release.clone(),
        progress,
        ..MajorUpgradeReport::default()
    };
    if let Some(upgrade) = next {
        report.codename = upgrade.from_codename.into();
        report.target = upgrade.to_release();
        report.target_codename = upgrade.to_codename.into();
        let plan = sources::plan(root, upgrade);
        report.checks = checks::evaluate(&Host, upgradable, &plan);
    }
    Some(report)
}

/// The last upgrade as it stands now, from the state file.
fn progress(
    state_dir: &Path,
    release: &str,
) -> Option<monitorflow_common::proto::v1::MajorUpgradeProgress> {
    match state::read(state_dir) {
        Ok(Some(state)) => Some(state.effective(&state::boot_id(), release).to_progress()),
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(%error, "cannot read the major upgrade state");
            None
        }
    }
}

/// Runs `major_upgrade` through the root helper.
pub struct MajorUpgradeHandler {
    tools: Tools,
    reports: Reports,
    /// `packages.major_upgrades`, and sudo rules that know the helper.
    allowed: bool,
}

impl MajorUpgradeHandler {
    pub fn new(tools: Tools, reports: Reports, allowed: bool) -> Self {
        Self {
            tools,
            reports,
            allowed,
        }
    }
}

/// Whether sudo's rules let this user run the helper: `sudo -n -l` with
/// its exact line.
pub async fn sudo_rule(tools: &Tools) -> bool {
    let agent = tools.agent.display().to_string();
    tools
        .read(&tools.sudo, &["-n", "-l", &agent, "major-upgrade"])
        .await
        .is_ok_and(|run| run.success())
}

/// What the log says when the host agreed and sudo does not know the
/// helper.
pub const OLD_SUDO_RULES: &str = "major upgrades are allowed in agent.toml, but sudo's rules predate them; run the install command again";

#[async_trait::async_trait]
impl Handler for MajorUpgradeHandler {
    fn acceptance(&self) -> CommandAcceptance {
        if self.allowed {
            CommandAcceptance::Accepted
        } else {
            CommandAcceptance::Disabled
        }
    }

    async fn run(&self, _payload: &[u8]) -> Output {
        Output::rejected("a major upgrade needs its signed command")
    }

    async fn run_command(&self, command: &Command) -> Output {
        if !self.allowed {
            return Output::rejected(format!(
                "major upgrades are not allowed on this server: {}",
                super::helper::ALLOW_LINE
            ));
        }
        match MajorUpgrade::decode(command.payload.as_slice()) {
            Ok(request) if major_upgrade::step(&request.from, &request.to).is_some() => {}
            Ok(request) => {
                return Output::rejected(format!(
                    "this agent does not know an upgrade from {} to {}",
                    request.from, request.to
                ))
            }
            Err(e) => return Output::rejected(format!("invalid payload: {e}")),
        }
        let request = HelperRequest::of(command).framed();
        let run = self
            .tools
            .privileged_with_input(&[], &self.tools.agent, &["major-upgrade"], Some(&request))
            .await;
        let mut output = Output::default();
        match run {
            Ok(run) => {
                tracing::info!(exit_code = run.code, "major upgrade helper finished");
                if !run.success() {
                    output.exit_code = if run.code == 0 { 1 } else { run.code };
                    output.error = Some(PackageError::helper(&run).to_string());
                }
                output.stdout = run.stdout;
                output.stderr = run.stderr;
            }
            Err(error) => {
                output.exit_code = 1;
                output.error = Some(error.to_string());
            }
        }
        self.reports.refresh_major_upgrade().await;
        output
    }
}

/// Sends the report again whenever the state of an upgrade changes, until
/// `shutdown`. Cheap while nothing runs: one small file read every
/// [`WATCH`].
pub async fn watch(reports: Reports, dir: PathBuf, mut shutdown: Shutdown) {
    let mut seen: Option<(String, String, i64)> = None;
    loop {
        let now = tokio::task::spawn_blocking({
            let dir = dir.clone();
            move || state::read(&dir).ok().flatten()
        })
        .await
        .ok()
        .flatten()
        .map(|s| {
            (
                s.command_id,
                s.phase.as_str().to_owned(),
                s.updated_at_unix_ms,
            )
        });
        if now.is_some() && now != seen {
            if seen.is_some() {
                reports.refresh_major_upgrade().await;
            }
            seen = now;
        }
        tokio::select! {
            () = tokio::time::sleep(WATCH) => {}
            () = shutdown.wait() => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_release_without_a_next_one_and_no_upgrade_reports_nothing() {
        let dir = tempfile::tempdir().unwrap();
        // The host running the tests may be anything; only a release this
        // version knows gets a target.
        let report = build(0, dir.path(), dir.path());
        let release = crate::os::detect_from(None).os_version;
        assert_eq!(
            report.is_some(),
            major_upgrade::next(&release).is_some(),
            "{release}"
        );
    }

    #[test]
    fn the_last_upgrade_is_reported_as_it_stands() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = state::State::new("c1", "debian 12", "debian 13", "other-boot", 1);
        s.phase = major_upgrade::Phase::Rebooting;
        state::write(dir.path(), &s).unwrap();
        let progress = progress(dir.path(), "debian 13").unwrap();
        assert_eq!(progress.command_id, "c1");
        // Another boot ID than this one: the reboot happened.
        assert_eq!(progress.phase, "done");
    }
}
