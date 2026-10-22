//! apt, dnf, the update helper and the `apply_updates` handler against
//! fake binaries: a fake `sudo` logs the exact command lines sudoers rules
//! must allow, the others the command lines the helper runs as root.

use std::path::{Path, PathBuf};

use monitorflow_common::proto::v1::ApplyUpdates;
use monitorflow_sensors::testing::write_executable;
use prost::Message;
use tempfile::TempDir;

use super::update::{self, UpdateKind, UpdateSpec};
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
        // The packages helper: saves its stdin, exits with `helper-exit`.
        let helper_exit = dir.path().join("helper-exit");
        fake(
            "monitorflow-agent",
            format!(
                "cat > '{}'
code=$(cat '{}' 2>/dev/null || echo 0)
echo '$ /usr/bin/apt-get -q update'
                 [ \"$code\" = 0 ] || {{ echo 'E: broken' >&2; echo 'Error: `apt-get upgrade` exited with '\"$code\"': E: broken' >&2; }}
exit \"$code\"",
                dir.path().join("helper-stdin").display(),
                helper_exit.display()
            ),
        );
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
            agent: self.path("monitorflow-agent"),
            reboot_required_file: self.path("reboot-required"),
        }
    }

    /// The lines `sudo` was called with, the fake directory shown as `@`.
    fn sudo_calls(&self) -> Vec<String> {
        self.calls_of("sudo")
    }

    /// The arguments `name` was called with, the fake directory shown as `@`.
    fn calls_of(&self, name: &str) -> Vec<String> {
        let calls = std::fs::read_to_string(self.path("calls")).unwrap_or_default();
        let dir = format!("{}/", self.dir.path().display());
        let prefix = format!("{name} ");
        calls
            .lines()
            .filter_map(|l| l.strip_prefix(&prefix))
            .map(|l| l.replace(&dir, "@"))
            .collect()
    }

    fn helper_exits(&self, code: i32) {
        std::fs::write(self.path("helper-exit"), code.to_string()).unwrap();
    }

    fn helper_stdin(&self) -> String {
        std::fs::read_to_string(self.path("helper-stdin")).unwrap()
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

const HELPER: &str = "-n @monitorflow-agent packages update";

#[tokio::test]
async fn updates_go_to_the_helper_then_report() {
    let fakes = Fakes::new(0, 0, 0);
    let (handler, report) = apply_handler(&fakes, OsFamily::Debian);
    let output = handler.run(&payload(&[], false, false)).await;
    assert_eq!((output.exit_code, &output.error), (0, &None), "{output:?}");
    assert_eq!(fakes.sudo_calls(), [HELPER]);
    assert_eq!(fakes.helper_stdin(), r#"{"kind":"all"}"#);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.starts_with("$ sudo -n ") && stdout.contains("$ /usr/bin/apt-get -q update"),
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
async fn named_and_security_updates_send_their_spec_and_reboot_when_allowed() {
    let fakes = Fakes::new(0, 0, 0);
    std::fs::write(fakes.path("reboot-required"), "").unwrap();
    let (handler, report) = apply_handler(&fakes, OsFamily::Debian);
    let output = handler.run(&payload(&["openssl"], true, true)).await;
    assert_eq!(output.error, None);
    assert_eq!(fakes.sudo_calls(), [HELPER, "-n @shutdown -r +1"]);
    assert_eq!(
        fakes.helper_stdin(),
        r#"{"kind":"security","packages":["openssl"]}"#
    );
    assert!(report.borrow().as_ref().unwrap().reboot_required);

    let fakes = Fakes::new(0, 0, 0);
    let (handler, _) = apply_handler(&fakes, OsFamily::Rhel);
    let output = handler
        .run(&payload(&["openssl", "bash"], false, true))
        .await;
    assert_eq!(output.error, None);
    // needs-restarting exits 0: no reboot.
    assert_eq!(fakes.sudo_calls(), [HELPER]);
    assert_eq!(
        fakes.helper_stdin(),
        r#"{"kind":"named","packages":["openssl","bash"]}"#
    );
}

#[tokio::test]
async fn a_failed_update_fails_with_the_helpers_error_but_still_reports() {
    let fakes = Fakes::new(0, 0, 0);
    fakes.helper_exits(100);
    let (handler, report) = apply_handler(&fakes, OsFamily::Debian);
    let output = handler.run(&payload(&["bash"], false, true)).await;
    assert_eq!(output.exit_code, 100);
    assert_eq!(
        output.error.as_deref(),
        Some("`apt-get upgrade` exited with 100: E: broken")
    );
    assert!(String::from_utf8_lossy(&output.stderr).starts_with("E: broken\n"));
    assert!(report.borrow().is_some());
    // No reboot after a failure, even when allowed.
    assert_eq!(fakes.sudo_calls(), [HELPER]);
}

#[tokio::test]
async fn bad_payloads_are_rejected_before_running_anything() {
    let fakes = Fakes::new(0, 0, 0);
    let (handler, report) = apply_handler(&fakes, OsFamily::Debian);
    let garbage = handler.run(b"\xff\xff").await;
    assert!(garbage.rejected);
    for name in ["-o", "a b", "x;reboot", "", "$(id)", "/tmp/x.deb"] {
        let output: Output = handler.run(&payload(&[name], false, false)).await;
        assert!(output.rejected, "{name}");
        let output: Output = handler.run(&payload(&[name], true, false)).await;
        assert!(output.rejected, "{name}");
    }
    assert!(fakes.sudo_calls().is_empty());
    assert!(report.borrow().is_none());
}

fn spec(kind: UpdateKind, packages: &[&str]) -> UpdateSpec {
    UpdateSpec {
        kind,
        packages: packages.iter().map(|p| p.to_string()).collect(),
    }
}

const APT_UPGRADE: &str =
    "-q -y -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold";

#[tokio::test]
async fn the_helper_runs_apt() {
    let fakes = Fakes::new(0, 0, 0);
    let outcome = update::run(
        &spec(UpdateKind::All, &[]),
        OsFamily::Debian,
        &fakes.tools(),
    )
    .await;
    assert!(outcome.upgraded && outcome.error.is_none(), "{outcome:?}");
    assert_eq!(
        fakes.calls_of("apt-get"),
        ["-q update".to_owned(), format!("{APT_UPGRADE} upgrade")]
    );
    let lines: Vec<_> = outcome.runs.iter().map(|r| r.line.as_str()).collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].ends_with("/apt-get -q update"), "{lines:?}");
    assert_eq!(
        outcome.runs[1].stdout,
        format!("apt {APT_UPGRADE} upgrade\n").as_bytes()
    );

    let fakes = Fakes::new(0, 0, 0);
    let named = spec(UpdateKind::Named, &["openssl", "libssl3"]);
    update::run(&named, OsFamily::Debian, &fakes.tools()).await;
    assert_eq!(
        fakes.calls_of("apt-get"),
        [
            "-q update".to_owned(),
            format!("{APT_UPGRADE} install --only-upgrade openssl libssl3")
        ]
    );
}

#[tokio::test]
async fn the_helper_upgrades_apt_security_candidates() {
    let fakes = Fakes::new(0, 0, 0);
    let outcome = update::run(
        &spec(UpdateKind::Security, &[]),
        OsFamily::Debian,
        &fakes.tools(),
    )
    .await;
    assert!(outcome.upgraded && outcome.error.is_none(), "{outcome:?}");
    assert_eq!(
        fakes.calls_of("apt-get"),
        [
            "-q update".to_owned(),
            "-s -q dist-upgrade".to_owned(),
            format!("{APT_UPGRADE} install --only-upgrade libc6")
        ]
    );

    // Named packages without a security update: nothing to do.
    let fakes = Fakes::new(0, 0, 0);
    let outcome = update::run(
        &spec(UpdateKind::Security, &["tzdata"]),
        OsFamily::Debian,
        &fakes.tools(),
    )
    .await;
    assert!(!outcome.upgraded && outcome.error.is_none(), "{outcome:?}");
    assert_eq!(outcome.exit_code(), 0);
    assert_eq!(
        fakes.calls_of("apt-get"),
        ["-q update", "-s -q dist-upgrade"]
    );
}

#[tokio::test]
async fn the_helper_stops_at_the_first_failure() {
    let fakes = Fakes::new(100, 0, 0);
    let outcome = update::run(
        &spec(UpdateKind::All, &[]),
        OsFamily::Debian,
        &fakes.tools(),
    )
    .await;
    assert_eq!(fakes.calls_of("apt-get"), ["-q update"]);
    assert_eq!(outcome.exit_code(), 100);
    let error = outcome.error.unwrap().to_string();
    assert!(
        error.contains("apt-get -q update` exited with 100"),
        "{error}"
    );

    let tools = Tools {
        dnf: "/nonexistent/dnf".into(),
        ..Fakes::new(0, 0, 0).tools()
    };
    let outcome = update::run(&spec(UpdateKind::All, &[]), OsFamily::Rhel, &tools).await;
    assert!(outcome.runs.is_empty());
    assert_eq!(outcome.exit_code(), 1);
}

#[tokio::test]
async fn the_helper_runs_dnf() {
    let fakes = Fakes::new(0, 0, 0);
    let security = spec(UpdateKind::Security, &["openssl", "bash"]);
    let outcome = update::run(&security, OsFamily::Rhel, &fakes.tools()).await;
    assert!(outcome.upgraded && outcome.error.is_none(), "{outcome:?}");
    assert_eq!(
        fakes.calls_of("dnf"),
        ["-q -y upgrade --security openssl bash"]
    );
    assert_eq!(outcome.runs[0].stderr, b"upgraded\n");

    let fakes = Fakes::new(0, 1, 0);
    let outcome = update::run(&spec(UpdateKind::All, &[]), OsFamily::Rhel, &fakes.tools()).await;
    assert_eq!(outcome.exit_code(), 1);
    assert_eq!(fakes.calls_of("dnf"), ["-q -y upgrade"]);
}

#[tokio::test]
async fn dnf_reports_from_rpm() {
    let fakes = Fakes::new(0, 0, 1);
    let (reports, report) = Reports::new(for_family(OsFamily::Rhel, fakes.tools()));
    reports.collect().await;
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
