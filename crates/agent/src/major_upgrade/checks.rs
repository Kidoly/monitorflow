//! The checks before a major upgrade (ADR-0182). The agent works them out
//! with each package report as its own user, and the root helper runs the
//! same code again just before the upgrade, refusing on any failed one.

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::Command;

use monitorflow_common::major_upgrade::{
    self, bytes, Method, Upgrade, FAILED, FREE_SPACE, OK, WARNING,
};
use monitorflow_common::proto::v1::UpgradeCheck;

use super::sources::Plan;

/// Items listed in a check, at most.
const MAX_ITEMS: usize = 50;
/// Ubuntu's choice of the releases `do-release-upgrade` offers.
pub const RELEASE_UPGRADES: &str = "/etc/update-manager/release-upgrades";
/// dnf's version locks.
const VERSIONLOCK: &str = "/etc/dnf/plugins/versionlock.list";

/// What the checks read of the host. [`Host`] is the real one.
pub trait Probe {
    /// Held packages: `apt-mark showhold`, or dnf's version locks.
    fn held(&self) -> Result<Vec<String>, String>;
    /// `dpkg --audit`'s or `dnf check`'s output: empty when every package
    /// is fine.
    fn audit(&self) -> Result<String, String>;
    /// Free bytes for unprivileged users and the device of `path`.
    fn free(&self, path: &str) -> Result<(u64, u64), String>;
    fn reboot_pending(&self) -> bool;
    /// Ubuntu: the version `do-release-upgrade -c` offers, like
    /// "24.04.1"; `None` when it offers none.
    fn offered(&self) -> Result<Option<String>, String> {
        Ok(None)
    }
    /// Ubuntu: `Prompt=` of [`RELEASE_UPGRADES`].
    fn prompt(&self) -> Option<String> {
        None
    }
    /// The RHEL family: which of `packages` are not installed.
    fn missing(&self, packages: &[String]) -> Vec<String> {
        packages.to_vec()
    }
}

/// The real host, tools at fixed paths in the C locale, for the upgrade
/// method of its distribution.
pub struct Host {
    pub method: Method,
}

impl Host {
    pub fn new(method: Method) -> Self {
        Self { method }
    }
}

/// Exit code and output (stdout, then stderr) of `program args`.
fn output(program: &str, args: &[&str]) -> Result<(i32, String, String), String> {
    let output = Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    Ok((
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let (code, stdout, stderr) = output(program, args)?;
    if code != 0 {
        let first = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        return Err(format!(
            "`{program} {}` exited with {code}: {first}",
            args.join(" ")
        ));
    }
    Ok(stdout)
}

/// The version in `do-release-upgrade -c`'s "New release '24.04.1 LTS'
/// available." line.
pub fn parse_offered(text: &str) -> Option<String> {
    let line = text.lines().find(|l| l.contains("New release '"))?;
    let quoted = line.split('\'').nth(1)?;
    quoted.split_whitespace().next().map(str::to_owned)
}

/// `Prompt=` of the `[DEFAULT]` section of [`RELEASE_UPGRADES`].
pub fn parse_prompt(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .filter(|(k, _)| k.trim().eq_ignore_ascii_case("prompt"))
        .map(|(_, v)| v.trim().to_ascii_lowercase())
        .next_back()
}

/// dnf's version locks, one entry per line.
pub fn parse_versionlock(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

impl Probe for Host {
    fn held(&self) -> Result<Vec<String>, String> {
        if self.method == Method::Leapp {
            return match std::fs::read_to_string(VERSIONLOCK) {
                Ok(text) => Ok(parse_versionlock(&text)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
                Err(e) => Err(format!("cannot read {VERSIONLOCK}: {e}")),
            };
        }
        Ok(run("/usr/bin/apt-mark", &["showhold"])?
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect())
    }

    fn audit(&self) -> Result<String, String> {
        if self.method != Method::Leapp {
            return run("/usr/bin/dpkg", &["--audit"]);
        }
        // Cached metadata only, as the package report reads it.
        let (code, stdout, stderr) = output("/usr/bin/dnf", &["-q", "-C", "check"])?;
        let found = format!("{stdout}{stderr}");
        match code {
            0 => Ok(stdout),
            _ if !found.trim().is_empty() => Ok(found),
            code => Err(format!("`dnf check` exited with {code}")),
        }
    }

    fn free(&self, path: &str) -> Result<(u64, u64), String> {
        let stat = rustix::fs::statvfs(path).map_err(|e| format!("{path}: {e}"))?;
        let device = std::fs::metadata(path)
            .map_err(|e| format!("{path}: {e}"))?
            .dev();
        Ok((stat.f_bavail.saturating_mul(stat.f_frsize), device))
    }

    fn reboot_pending(&self) -> bool {
        if self.method == Method::Leapp {
            // Exits 1 when a reboot is needed.
            return output("/usr/bin/dnf", &["-q", "needs-restarting", "-r"])
                .is_ok_and(|(code, _, _)| code == 1);
        }
        Path::new("/var/run/reboot-required").exists()
    }

    fn offered(&self) -> Result<Option<String>, String> {
        let program = "/usr/bin/do-release-upgrade";
        if !Path::new(program).exists() {
            return Err(
                "do-release-upgrade is not installed; install ubuntu-release-upgrader-core".into(),
            );
        }
        // Exits 1 when no release is offered; any other failure says why.
        let (code, stdout, stderr) = output(program, &["-c"])?;
        match parse_offered(&stdout) {
            Some(version) => Ok(Some(version)),
            None if code <= 1 && stderr.trim().is_empty() => Ok(None),
            None => {
                let text = format!("{stdout}{stderr}");
                let last = text
                    .lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("");
                Err(format!(
                    "`do-release-upgrade -c` exited with {code}: {}",
                    last.trim()
                ))
            }
        }
    }

    fn prompt(&self) -> Option<String> {
        std::fs::read_to_string(RELEASE_UPGRADES)
            .ok()
            .and_then(|text| parse_prompt(&text))
    }

    fn missing(&self, packages: &[String]) -> Vec<String> {
        packages
            .iter()
            .filter(|p| !output("/usr/bin/rpm", &["-q", p]).is_ok_and(|(code, _, _)| code == 0))
            .cloned()
            .collect()
    }
}

/// The packages leapp needs for `upgrade`: its actors for the step, and
/// the ELevate data on a derivative.
pub fn leapp_packages(upgrade: &Upgrade) -> Vec<String> {
    let mut packages = vec![format!(
        "leapp-upgrade-el{}toel{}",
        upgrade.from, upgrade.to
    )];
    packages.extend(upgrade.elevate_data());
    packages
}

fn check(name: &str, state: &str, detail: impl Into<String>, items: Vec<String>) -> UpgradeCheck {
    UpgradeCheck {
        name: name.into(),
        state: state.into(),
        detail: detail.into(),
        items: items.into_iter().take(MAX_ITEMS).collect(),
    }
}

/// Every check, in the order of `major_upgrade::checks`. `upgradable` is
/// how many packages of the current release have an update pending.
pub fn evaluate(
    probe: &dyn Probe,
    upgrade: &Upgrade,
    upgradable: usize,
    plan: &Plan,
) -> Vec<UpgradeCheck> {
    let method = upgrade.method;
    let leapp = method == Method::Leapp;
    let mut checks = Vec::with_capacity(8);
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
            if leapp {
                format!(
                    "{} packages are locked to a version; remove the locks with dnf versionlock delete",
                    held.len()
                )
            } else {
                format!(
                    "{} held packages; release them with apt-mark unhold",
                    held.len()
                )
            },
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
            if leapp {
                "dnf check reports broken dependencies or duplicate packages; fix them first"
            } else {
                "dpkg --audit reports packages half-installed or not configured; run dpkg --configure -a"
            },
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
    checks.push(match (&plan.refusal, method) {
        (Some(why), _) => check("sources", FAILED, why.clone(), vec![]),
        (None, Method::ReleaseUpgrade | Method::Leapp) if plan.mirrored => check(
            "sources",
            WARNING,
            format!(
                "the server takes its packages from the MonitorFlow mirrors, which {} does not use: the campaign puts its own sources back before the upgrade, and the mirrors of {} after it when they carry it",
                super::sources::tool(method),
                upgrade.to_label()
            ),
            vec![],
        ),
        (None, Method::Apt) => check(
            "sources",
            OK,
            format!("{} entries move to the new release", plan.release_entries),
            vec![],
        ),
        (None, Method::ReleaseUpgrade) => check(
            "sources",
            OK,
            format!(
                "{} entries of the Ubuntu archive; do-release-upgrade moves them to the new release",
                plan.release_entries
            ),
            vec![],
        ),
        (None, Method::Leapp) => check(
            "sources",
            OK,
            format!(
                "leapp takes the repositories of {} from its own data",
                upgrade.to_label()
            ),
            vec![],
        ),
    });
    match method {
        Method::Apt => {}
        Method::ReleaseUpgrade => checks.push(release_offered(probe, upgrade)),
        Method::Leapp => checks.push(leapp_installed(probe, upgrade)),
    }
    checks.push(if plan.third_party.is_empty() {
        check("third_party_sources", OK, "no third-party source", vec![])
    } else {
        let n = plan.third_party.len();
        check(
            "third_party_sources",
            WARNING,
            match method {
                Method::Apt => format!(
                    "{n} third-party sources are switched off for the upgrade and left off"
                ),
                Method::ReleaseUpgrade => format!(
                    "{n} third-party sources are switched off by do-release-upgrade and left off"
                ),
                Method::Leapp => format!(
                    "{n} third-party repositories are not used by leapp; check their packages after the upgrade"
                ),
            },
            plan.third_party.clone(),
        )
    });
    checks
}

/// Ubuntu: LTS to the next LTS only, once that LTS offers the upgrade.
fn release_offered(probe: &dyn Probe, upgrade: &Upgrade) -> UpgradeCheck {
    match probe.prompt().as_deref() {
        Some("lts") => {}
        prompt => {
            return check(
                "release_offered",
                FAILED,
                format!(
                    "Prompt is {} in {RELEASE_UPGRADES}; set it to lts, so that only the next LTS is offered",
                    prompt.unwrap_or("not set")
                ),
                vec![],
            )
        }
    }
    match probe.offered() {
        Ok(Some(version))
            if major_upgrade::runs(
                &format!("{} {version}", upgrade.distribution),
                &upgrade.to_release(),
            ) =>
        {
            check(
                "release_offered",
                OK,
                format!("do-release-upgrade offers {version}"),
                vec![],
            )
        }
        Ok(Some(version)) => check(
            "release_offered",
            FAILED,
            format!("do-release-upgrade offers {version}, not {}", upgrade.to),
            vec![],
        ),
        Ok(None) => check(
            "release_offered",
            FAILED,
            format!(
                "{} does not offer the upgrade yet: Ubuntu offers it after its first point release",
                upgrade.to_label()
            ),
            vec![],
        ),
        Err(error) => check("release_offered", FAILED, error, vec![]),
    }
}

/// The RHEL family: leapp, its actors for the step and, on a derivative,
/// the ELevate data. The only check `leapp preupgrade` alone needs.
pub fn leapp_installed(probe: &dyn Probe, upgrade: &Upgrade) -> UpgradeCheck {
    let missing = probe.missing(&leapp_packages(upgrade));
    if missing.is_empty() {
        return check(
            "leapp",
            OK,
            format!(
                "leapp and its data for {} are installed",
                upgrade.to_label()
            ),
            vec![],
        );
    }
    let detail = match upgrade.elevate_data() {
        None => "install leapp first: dnf install leapp-upgrade".to_owned(),
        Some(data) => format!(
            "install leapp and the ELevate data first: dnf install https://repo.almalinux.org/elevate/elevate-release-latest-el{}.noarch.rpm, then dnf install leapp-upgrade {data}",
            upgrade.from
        ),
    };
    check("leapp", FAILED, detail, missing)
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
