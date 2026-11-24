//! The checks before a major upgrade (ADR-0182). The agent works them out
//! with each package report as its own user, and the root helper runs the
//! same code again just before the upgrade, refusing on any failed one.

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::Command;

use monitorflow_common::major_upgrade::{bytes, FAILED, FREE_SPACE, OK, WARNING};
use monitorflow_common::proto::v1::UpgradeCheck;

use super::sources::Plan;

/// Items listed in a check, at most.
const MAX_ITEMS: usize = 50;

/// What the checks read of the host. [`Host`] is the real one.
pub trait Probe {
    /// `apt-mark showhold`.
    fn held(&self) -> Result<Vec<String>, String>;
    /// `dpkg --audit`'s output: empty when every package is fine.
    fn audit(&self) -> Result<String, String>;
    /// Free bytes for unprivileged users and the device of `path`.
    fn free(&self, path: &str) -> Result<(u64, u64), String>;
    fn reboot_pending(&self) -> bool;
}

/// The real host, tools at fixed paths in the C locale.
pub struct Host;

fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let first = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        return Err(format!(
            "`{program} {}` exited with {}: {first}",
            args.join(" "),
            output.status.code().unwrap_or(-1)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

impl Probe for Host {
    fn held(&self) -> Result<Vec<String>, String> {
        Ok(run("/usr/bin/apt-mark", &["showhold"])?
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect())
    }

    fn audit(&self) -> Result<String, String> {
        run("/usr/bin/dpkg", &["--audit"])
    }

    fn free(&self, path: &str) -> Result<(u64, u64), String> {
        let stat = rustix::fs::statvfs(path).map_err(|e| format!("{path}: {e}"))?;
        let device = std::fs::metadata(path)
            .map_err(|e| format!("{path}: {e}"))?
            .dev();
        Ok((stat.f_bavail.saturating_mul(stat.f_frsize), device))
    }

    fn reboot_pending(&self) -> bool {
        Path::new("/var/run/reboot-required").exists()
    }
}

fn check(name: &str, state: &str, detail: impl Into<String>, items: Vec<String>) -> UpgradeCheck {
    UpgradeCheck {
        name: name.into(),
        state: state.into(),
        detail: detail.into(),
        items: items.into_iter().take(MAX_ITEMS).collect(),
    }
}

/// Every check, in the order of `major_upgrade::CHECKS`. `upgradable` is
/// how many packages of the current release have an update pending.
pub fn evaluate(probe: &dyn Probe, upgradable: usize, plan: &Plan) -> Vec<UpgradeCheck> {
    let mut checks = Vec::with_capacity(7);
    checks.push(if upgradable == 0 {
        check("up_to_date", OK, "every package is up to date", vec![])
    } else {
        check(
            "up_to_date",
            FAILED,
            format!("{upgradable} updates pending in the current release; install them first"),
            vec![],
        )
    });
    checks.push(match probe.held() {
        Ok(held) if held.is_empty() => check("held_packages", OK, "no package is held", vec![]),
        Ok(held) => check(
            "held_packages",
            FAILED,
            format!(
                "{} held packages; release them with apt-mark unhold",
                held.len()
            ),
            held,
        ),
        Err(error) => check("held_packages", FAILED, error, vec![]),
    });
    checks.push(match probe.audit() {
        Ok(out) if out.trim().is_empty() => {
            check("broken_packages", OK, "no broken package", vec![])
        }
        Ok(out) => check(
            "broken_packages",
            FAILED,
            "dpkg --audit reports packages half-installed or not configured; run dpkg --configure -a",
            out.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_owned)
                .collect(),
        ),
        Err(error) => check("broken_packages", FAILED, error, vec![]),
    });
    checks.push(free_space(probe));
    checks.push(if probe.reboot_pending() {
        check(
            "reboot_pending",
            FAILED,
            "a reboot is pending; reboot the server first",
            vec![],
        )
    } else {
        check("reboot_pending", OK, "no reboot pending", vec![])
    });
    checks.push(match &plan.refusal {
        Some(why) => check("sources", FAILED, why.clone(), vec![]),
        None => check(
            "sources",
            OK,
            format!("{} entries move to the new release", plan.release_entries),
            vec![],
        ),
    });
    checks.push(if plan.third_party.is_empty() {
        check("third_party_sources", OK, "no third-party source", vec![])
    } else {
        check(
            "third_party_sources",
            WARNING,
            format!(
                "{} third-party sources are switched off for the upgrade and left off",
                plan.third_party.len()
            ),
            plan.third_party.clone(),
        )
    });
    checks
}

/// Free space on `/`, `/var` and `/boot`, paths on one filesystem adding
/// up their needs.
fn free_space(probe: &dyn Probe) -> UpgradeCheck {
    // device -> (free, needed, paths)
    let mut devices: BTreeMap<u64, (u64, u64, Vec<&str>)> = BTreeMap::new();
    for (path, needed) in FREE_SPACE {
        if *path != "/" && !Path::new(path).exists() {
            continue;
        }
        match probe.free(path) {
            Ok((free, device)) => {
                let entry = devices.entry(device).or_insert((free, 0, Vec::new()));
                entry.1 += needed;
                entry.2.push(path);
            }
            Err(error) => return check("free_space", FAILED, error, vec![]),
        }
    }
    let short: Vec<String> = devices
        .values()
        .filter(|(free, needed, _)| free < needed)
        .map(|(free, needed, paths)| {
            format!(
                "{} has {} free, needs {}",
                paths.join(" and "),
                bytes(*free),
                bytes(*needed)
            )
        })
        .collect();
    if short.is_empty() {
        let summary: Vec<String> = devices
            .values()
            .map(|(free, _, paths)| format!("{} {}", paths.join(" and "), bytes(*free)))
            .collect();
        check(
            "free_space",
            OK,
            format!("free: {}", summary.join(", ")),
            vec![],
        )
    } else {
        check("free_space", FAILED, short.join("; "), vec![])
    }
}

/// The first failed check, as the helper's error.
pub fn first_failure(checks: &[UpgradeCheck]) -> Option<String> {
    checks
        .iter()
        .find(|c| c.state == FAILED)
        .map(|c| format!("{}: {}", c.name, c.detail))
}
