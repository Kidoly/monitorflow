//! apt, dnf and the `apply_updates` handler against fake binaries: a fake
//! `sudo` logs the exact command lines sudoers rules must allow.

use std::path::{Path, PathBuf};

use monitorflow_common::proto::v1::ApplyUpdates;
use monitorflow_sensors::testing::write_executable;
use prost::Message;
use tempfile::TempDir;

use super::*;
use crate::commands::{Handler, Output};

const SIMULATION: &str = "\
Inst libc6 [2.36-9] (2.36-9+deb12u4 Debian-Security:12/stable-security [amd64])
Inst tzdata [2024a-0+deb12u1] (2025b-0+deb12u1 Debian:12.11/stable [all])
";
const DPKG: &str =
    "libc6\t2.36-9\tamd64\tii \ntzdata\t2024a-0+deb12u1\tall\tii \nbash\t5.2.15-2\tamd64\tii \n";
const RPM: &str =
    "bash\t5.1.8-9.el9\tx86_64\ngpg-pubkey\t1-2\t(none)\nopenssl\t1:3.0.7-27.el9\tx86_64\n";
const CHECK_UPDATE: &str = "\nopenssl.x86_64  1:3.2.2-6.el9_5  baseos\n";

/// Fake tools in a temporary directory. Each binary appends its arguments
/// to `calls`; `sudo` logs its own line too, then runs the command.
struct Fakes {
    dir: TempDir,
}

impl Fakes {
    /// `apt_exit` and `dnf_exit` are the exit codes of privileged runs.
    fn new(apt_exit: i32, dnf_exit: i32, reboot_exit: i32) -> Self {
        let dir = TempDir::new().unwrap();
        let calls = dir.path().join("calls");
        let log = |name: &str| format!("echo \"{name} $*\" >> '{}'", calls.display());
        let fake = |name: &str, body: String| {
            let script = format!("#!/bin/sh\n{}\n{body}\n", log(name));
            write_executable(&dir.path().join(name), &script, 0o755);
        };
        // sudo -n [VAR=value…] program args: env runs it with the variables.
        fake(
            "sudo",
            "[ \"$1\" = -n ] || exit 1\nshift\nexec env \"$@\"".into(),
        );
        fake(
            "apt-get",
            format!(
                "case \"$1\" in -s) printf '{}' ;; *) [ \"$DEBIAN_FRONTEND\" = noninteractive ] || [ \"$2\" = update ] || exit 9; echo \"apt $*\"; exit {apt_exit} ;; esac",
                SIMULATION.replace('\n', "\\n")
            ),
        );
        fake(
            "dpkg-query",
            format!(
                "printf '{}'",
                DPKG.replace('\t', "\\t").replace('\n', "\\n")
            ),
        );
        fake(
            "rpm",
            format!("printf '{}'", RPM.replace('\t', "\\t").replace('\n', "\\n")),
        );
        fake(
            "dnf",
            format!(
                "case \"$*\" in\n*check-update*) printf '{}'; exit 100 ;;\n*needs-restarting*) exit {reboot_exit} ;;\n*) echo upgraded >&2; exit {dnf_exit} ;;\nesac",
                CHECK_UPDATE.replace('\n', "\\n")
            ),
        );
        fake("shutdown", "exit 0".into());
        Self { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn tools(&self) -> Tools {
        Tools {
            sudo: self.path("sudo"),
            apt_get: self.path("apt-get"),
            dpkg_query: self.path("dpkg-query"),
            dnf: self.path("dnf"),
            rpm: self.path("rpm"),
            shutdown: self.path("shutdown"),
            reboot_required_file: self.path("reboot-required"),
        }
    }

    /// The lines `sudo` was called with, the fake directory shown as `@`.
    fn sudo_calls(&self) -> Vec<String> {
        let calls = std::fs::read_to_string(self.path("calls")).unwrap_or_default();
        let dir = format!("{}/", self.dir.path().display());
        calls
            .lines()
            .filter_map(|l| l.strip_prefix("sudo "))
            .map(|l| l.replace(&dir, "@"))
            .collect()
    }
}

fn apply_handler(
    fakes: &Fakes,
    family: OsFamily,
) -> (ApplyUpdatesHandler, watch::Receiver<Option<PackageReport>>) {
    let manager = for_family(family, fakes.tools());
    let (reports, rx) = Reports::new(manager.clone());
    (
        ApplyUpdatesHandler::new(manager, reports, fakes.tools()),
        rx,
    )
}

fn payload(packages: &[&str], security_only: bool, allow_reboot: bool) -> Vec<u8> {
    ApplyUpdates {
        packages: packages.iter().map(|p| p.to_string()).collect(),
        security_only,
        allow_reboot,
    }
    .encode_to_vec()
}

const APT_UPGRADE: &str = "-n DEBIAN_FRONTEND=noninteractive @apt-get -q -y -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold";

#[tokio::test]
async fn apt_upgrades_everything_then_reports() {
    let fakes = Fakes::new(0, 0, 0);
    let (handler, report) = apply_handler(&fakes, OsFamily::Debian);
    let output = handler.run(&payload(&[], false, false)).await;
    assert_eq!((output.exit_code, &output.error), (0, &None), "{output:?}");
    assert_eq!(
        fakes.sudo_calls(),
        [
            "-n @apt-get -q update".to_owned(),
            format!("{APT_UPGRADE} upgrade")
        ]
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("$ sudo -n") && stdout.contains("apt -q -y"),
        "{stdout}"
    );

    let report = report.borrow().clone().unwrap();
    assert!(!report.reboot_required);
    assert!(report.collected_at_unix_ms > 0);
    let upgradable: Vec<_> = report
        .packages
        .iter()
        .map(|p| (p.name.as_str(), p.upgradable_version.as_str()))
        .collect();
    assert_eq!(
        upgradable,
        [
            ("libc6", "2.36-9+deb12u4"),
            ("tzdata", "2025b-0+deb12u1"),
            ("bash", "")
        ]
    );
}

#[tokio::test]
async fn apt_security_only_upgrades_security_candidates_and_reboots_when_allowed() {
    let fakes = Fakes::new(0, 0, 0);
    std::fs::write(fakes.path("reboot-required"), "").unwrap();
    let (handler, report) = apply_handler(&fakes, OsFamily::Debian);
    let output = handler.run(&payload(&[], true, true)).await;
    assert_eq!(output.error, None);
    assert_eq!(
        fakes.sudo_calls(),
        [
            "-n @apt-get -q update".to_owned(),
            format!("{APT_UPGRADE} install --only-upgrade libc6"),
            "-n @shutdown -r +1".to_owned(),
        ]
    );
    assert!(report.borrow().as_ref().unwrap().reboot_required);

    // Named packages without a security update: nothing to do.
    let fakes = Fakes::new(0, 0, 0);
    let (handler, _) = apply_handler(&fakes, OsFamily::Debian);
    let output = handler.run(&payload(&["tzdata"], true, false)).await;
    assert_eq!(output.error, None);
    assert_eq!(fakes.sudo_calls(), ["-n @apt-get -q update"]);
    assert!(String::from_utf8_lossy(&output.stdout).contains("nothing to upgrade"));
}

#[tokio::test]
async fn a_failed_upgrade_fails_the_command_but_still_reports() {
    let fakes = Fakes::new(100, 0, 0);
    let (handler, report) = apply_handler(&fakes, OsFamily::Debian);
    let output = handler.run(&payload(&["bash"], false, true)).await;
    assert_eq!(output.exit_code, 100);
    let error = output.error.unwrap();
    assert!(
        error.contains("apt-get") && error.contains("100"),
        "{error}"
    );
    assert!(report.borrow().is_some());
    // No reboot after a failure, even when allowed.
    assert!(!fakes.sudo_calls().iter().any(|c| c.contains("shutdown")));
}

#[tokio::test]
async fn dnf_upgrades_with_its_own_flags_and_reads_rpm() {
    let fakes = Fakes::new(0, 0, 1);
    let (handler, report) = apply_handler(&fakes, OsFamily::Rhel);
    let output = handler
        .run(&payload(&["openssl", "bash"], true, false))
        .await;
    assert_eq!(output.error, None, "{output:?}");
    assert_eq!(
        fakes.sudo_calls(),
        ["-n @dnf -q -y upgrade --security openssl bash"]
    );
    assert_eq!(output.stderr, b"upgraded\n");
    let report = report.borrow().clone().unwrap();
    assert!(report.reboot_required, "needs-restarting exited 1");
    let packages: Vec<_> = report
        .packages
        .iter()
        .map(|p| {
            (
                p.name.as_str(),
                p.version.as_str(),
                p.upgradable_version.as_str(),
            )
        })
        .collect();
    assert_eq!(
        packages,
        [
            ("bash", "5.1.8-9.el9", ""),
            ("openssl", "1:3.0.7-27.el9", "1:3.2.2-6.el9_5")
        ]
    );

    let fakes = Fakes::new(0, 1, 0);
    let (handler, _) = apply_handler(&fakes, OsFamily::Rhel);
    let output = handler.run(&payload(&[], false, false)).await;
    assert_eq!(output.exit_code, 1);
    assert_eq!(fakes.sudo_calls(), ["-n @dnf -q -y upgrade"]);
}

#[tokio::test]
async fn bad_payloads_are_rejected_before_running_anything() {
    let fakes = Fakes::new(0, 0, 0);
    let (handler, report) = apply_handler(&fakes, OsFamily::Debian);
    let garbage = handler.run(b"\xff\xff").await;
    assert!(garbage.rejected);
    for name in ["-o", "a b", "x;reboot", "", "$(id)"] {
        let output: Output = handler.run(&payload(&[name], false, false)).await;
        assert!(output.rejected, "{name}");
    }
    assert!(fakes.sudo_calls().is_empty());
    assert!(report.borrow().is_none());
}

#[tokio::test]
async fn a_missing_tool_reports_nothing() {
    let missing = Path::new("/nonexistent/dpkg-query");
    let tools = Tools {
        dpkg_query: missing.into(),
        ..Fakes::new(0, 0, 0).tools()
    };
    let (reports, rx) = Reports::new(for_family(OsFamily::Debian, tools));
    reports.collect().await;
    assert!(rx.borrow().is_none());
}

#[test]
fn package_names_cannot_be_options_or_shell() {
    for good in [
        "libc6",
        "python3.11",
        "g++",
        "libstdc++6:amd64",
        "NetworkManager",
    ] {
        assert!(valid_name(good), "{good}");
    }
    for bad in ["", "-y", "--force", "a b", "a/b", "a;b", &"a".repeat(129)] {
        assert!(!valid_name(bad), "{bad}");
    }
}
