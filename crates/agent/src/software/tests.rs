//! The software switch, `software_off`, and on Linux the handler and the
//! root helper against fakes: a fake `sudo` that logs the exact lines
//! sudoers must allow, a fake helper that saves its stdin, and fake
//! package tools that log their arguments.

use std::path::Path;
use std::sync::Arc;

use monitorflow_common::proto::v1::{CommandAcceptance, SoftwareOff};
use prost::Message;

use super::*;

#[test]
fn the_web_interface_only_takes_the_power_away() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!turned_off(dir.path()));
    assert!(SoftwareSwitch::load(true, dir.path()).allowed());
    assert!(!SoftwareSwitch::load(false, dir.path()).allowed());
    std::fs::write(dir.path().join(OFF_FILE), "anything").unwrap();
    // Whatever it holds, the file means off, with or without consent.
    assert!(turned_off(dir.path()));
    let switch = SoftwareSwitch::load(true, dir.path());
    assert!(!switch.allowed() && switch.consent() && switch.is_turned_off());
    assert_eq!(switch.refusal(), Some(TURNED_OFF));
    assert_eq!(switch.acceptance(), CommandAcceptance::Disabled);
    assert_eq!(
        SoftwareSwitch::load(false, dir.path()).refusal(),
        Some(NOT_ALLOWED)
    );
    // The host's administrator turns it on again.
    assert!(forget(dir.path()).unwrap());
    assert!(!forget(dir.path()).unwrap());
    assert!(SoftwareSwitch::load(true, dir.path()).allowed());
}

#[tokio::test]
async fn software_off_is_stored_obeyed_and_reported() {
    let dir = tempfile::tempdir().unwrap();
    let switch = Arc::new(SoftwareSwitch::load(true, dir.path()));
    let handler = SoftwareOffHandler::new(switch.clone());
    assert_eq!(handler.acceptance(), CommandAcceptance::Accepted);
    let mut changes = switch.subscribe();
    let output = handler.run(&SoftwareOff {}.encode_to_vec()).await;
    assert!(
        output.error.is_none() && output.exit_code == 0,
        "{output:?}"
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("are off"));
    assert!(!switch.allowed());
    assert!(changes.has_changed().unwrap());
    changes.mark_unchanged();
    // Kept across restarts, and again is not a change.
    assert!(!SoftwareSwitch::load(true, dir.path()).allowed());
    let again = handler.run(&[]).await;
    assert!(String::from_utf8_lossy(&again.stdout).contains("already off"));
    assert!(!changes.has_changed().unwrap());
    // Still accepted: the server may always turn it off.
    assert_eq!(handler.acceptance(), CommandAcceptance::Accepted);
    assert!(handler.run(b"\xff").await.rejected);
}

#[tokio::test]
async fn a_failed_write_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing");
    let switch = Arc::new(SoftwareSwitch::load(true, &missing));
    let output = SoftwareOffHandler::new(switch.clone()).run(&[]).await;
    assert!(!output.rejected && output.exit_code == 1, "{output:?}");
    assert!(switch.allowed());
}

#[cfg(unix)]
mod linux {
    use std::path::PathBuf;

    use monitorflow_common::command::{SHELL, SOFTWARE};
    use monitorflow_common::proto::v1::{
        software_change::Action, Command, PackageManager as Manager, SoftwareChange,
        SoftwarePackage,
    };
    use monitorflow_sensors::testing::write_executable;
    use tempfile::TempDir;

    use super::super::helper::{self, SoftwareHelperError};
    use super::super::linux::{LinuxSoftwareHandler, OLD_SUDO_RULES};
    use super::*;
    use crate::commands::Handler;
    use crate::helper_trust::tests::{sent, unsigned, Ca, Host, NOW};
    use crate::helper_trust::HelperRequest;
    use crate::os::OsFamily;
    use crate::packages::{PackageError, PackageManager, Reports, Tools};
    use monitorflow_common::proto::v1::Package;

    struct FakeManager {
        reboot: bool,
    }

    #[async_trait::async_trait]
    impl PackageManager for FakeManager {
        async fn inventory(&self) -> Result<Vec<Package>, PackageError> {
            Ok(Vec::new())
        }
        async fn reboot_required(&self) -> Result<bool, PackageError> {
            Ok(self.reboot)
        }
    }

    struct Fakes {
        dir: TempDir,
    }

    impl Fakes {
        /// `sudo` refuses `-l` unless `rule`; the helper exits with `exit`.
        fn new(rule: bool, exit: i32) -> Self {
            let dir = TempDir::new().unwrap();
            let path = |name: &str| dir.path().join(name).display().to_string();
            write_executable(
                &dir.path().join("sudo"),
                &format!(
                    "#!/bin/sh\necho \"$*\" >> '{calls}'\n[ \"$1\" = -n ] || exit 1\nshift\n\
                     if [ \"$1\" = -l ]; then [ {rule} = true ] && exit 0; \
                     echo 'sudo: a password is required' >&2; exit 1; fi\nexec \"$@\"\n",
                    calls = path("calls"),
                ),
                0o755,
            );
            write_executable(
                &dir.path().join("monitorflow-agent"),
                &format!(
                    "#!/bin/sh\ncat > '{}'\necho '$ /usr/bin/apt-get install nginx'\n\
                     [ {exit} -eq 0 ] || echo 'Error: apt-get failed' >&2\nexit {exit}\n",
                    path("stdin")
                ),
                0o755,
            );
            write_executable(&dir.path().join("shutdown"), "#!/bin/sh\nexit 0\n", 0o755);
            // dpkg-query and rpm list what is installed; apt-get and dnf
            // log their arguments and environment.
            write_executable(
                &dir.path().join("dpkg-query"),
                "#!/bin/sh\nprintf 'curl curl:amd64 ii \\nvim vim ii \\ngone gone rc \\n'\n",
                0o755,
            );
            write_executable(
                &dir.path().join("rpm"),
                "#!/bin/sh\nprintf 'curl\\nvim\\n'\n",
                0o755,
            );
            for name in ["apt-get", "dnf"] {
                write_executable(
                    &dir.path().join(name),
                    &format!(
                        "#!/bin/sh\necho \"{name} $* ${{DEBIAN_FRONTEND:-}}\" >> '{}'\n\
                         exit $(cat '{}' 2>/dev/null || echo 0)\n",
                        path("programs"),
                        path(&format!("{name}-exit")),
                    ),
                    0o755,
                );
            }
            Self { dir }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.dir.path().join(name)
        }

        fn tools(&self) -> Tools {
            Tools {
                sudo: self.path("sudo"),
                agent: self.path("monitorflow-agent"),
                shutdown: self.path("shutdown"),
                apt_get: self.path("apt-get"),
                dpkg_query: self.path("dpkg-query"),
                dnf: self.path("dnf"),
                rpm: self.path("rpm"),
                ..Tools::default()
            }
        }

        fn handler(
            &self,
            switch: SoftwareSwitch,
            rule: bool,
            reboot: bool,
        ) -> LinuxSoftwareHandler {
            let manager: Arc<dyn PackageManager> = Arc::new(FakeManager { reboot });
            let (reports, _) = Reports::new(manager.clone());
            LinuxSoftwareHandler::new(
                Arc::new(switch),
                OsFamily::Debian,
                rule,
                manager,
                reports,
                self.tools(),
            )
        }

        fn lines(&self, file: &str) -> Vec<String> {
            let dir = format!("{}/", self.dir.path().display());
            std::fs::read_to_string(self.path(file))
                .unwrap_or_default()
                .lines()
                .map(|l| l.replace(&dir, "@").trim_end().to_owned())
                .collect()
        }

        /// `agent.toml` with or without the consent, its data directory
        /// the fakes' own.
        fn config(&self, software: bool) -> PathBuf {
            let path = self.path("agent.toml");
            std::fs::write(
                &path,
                format!(
                    "upstream_url = \"https://mf.example:7443\"\ndata_dir = \"{}\"\n\n\
                     [commands]\nsoftware = {software}\n",
                    self.dir.path().display()
                ),
            )
            .unwrap();
            path
        }
    }

    fn change(manager: Manager, action: Action, names: &[&str], reboot: bool) -> SoftwareChange {
        SoftwareChange {
            action: action.into(),
            manager: manager.into(),
            packages: names
                .iter()
                .map(|n| SoftwarePackage {
                    id: n.to_string(),
                    source: String::new(),
                })
                .collect(),
            allow_reboot: reboot,
        }
    }

    fn apt_install(names: &[&str]) -> Vec<u8> {
        change(Manager::Apt, Action::Install, names, true).encode_to_vec()
    }

    #[tokio::test]
    async fn the_handler_sends_the_signed_command_to_the_one_sudo_line_then_restarts() {
        let fakes = Fakes::new(true, 0);
        let command = unsigned(SOFTWARE, &apt_install(&["nginx"]));
        let handler = fakes.handler(SoftwareSwitch::fixed(true), true, true);
        assert_eq!(handler.acceptance(), CommandAcceptance::Accepted);
        let output = handler.run_command(&command).await;
        assert_eq!((output.exit_code, output.error.as_deref()), (0, None));
        assert_eq!(
            fakes.lines("calls"),
            [
                "-n @monitorflow-agent packages software",
                "-n @shutdown -r +1"
            ]
        );
        assert_eq!(sent(&std::fs::read(fakes.path("stdin")).unwrap()), command);
        assert!(String::from_utf8_lossy(&output.stdout).ends_with("restarting in one minute\n"));
    }

    #[tokio::test]
    async fn nothing_runs_without_consent_once_off_or_for_another_manager() {
        let fakes = Fakes::new(true, 0);
        let good = unsigned(SOFTWARE, &apt_install(&["nginx"]));
        let refused = fakes.handler(SoftwareSwitch::fixed(false), true, false);
        assert_eq!(refused.acceptance(), CommandAcceptance::Disabled);
        let output = refused.run_command(&good).await;
        assert!(output.rejected);
        assert_eq!(output.error.as_deref(), Some(NOT_ALLOWED));
        let dnf = change(Manager::Dnf, Action::Install, &["httpd"], false).encode_to_vec();
        let handler = fakes.handler(SoftwareSwitch::fixed(true), true, false);
        let output = handler.run_command(&unsigned(SOFTWARE, &dnf)).await;
        assert!(output.rejected, "{output:?}");
        assert!(output
            .error
            .unwrap()
            .contains("dnf packages are not for debian"));
        assert!(
            handler
                .run_command(&unsigned(SOFTWARE, b"\xff"))
                .await
                .rejected
        );
        assert!(fakes.lines("calls").is_empty());
    }

    #[tokio::test]
    async fn old_sudo_rules_are_named() {
        let fakes = Fakes::new(false, 1);
        let handler = fakes.handler(SoftwareSwitch::fixed(true), false, false);
        assert_eq!(handler.acceptance(), CommandAcceptance::Unsupported);
        let output = handler
            .run_command(&unsigned(SOFTWARE, &apt_install(&["nginx"])))
            .await;
        assert_eq!(output.error.as_deref(), Some(OLD_SUDO_RULES));
        // The helper's own error is kept.
        let fakes = Fakes::new(true, 100);
        let handler = fakes.handler(SoftwareSwitch::fixed(true), true, true);
        let output = handler
            .run_command(&unsigned(SOFTWARE, &apt_install(&["nginx"])))
            .await;
        assert_eq!(output.exit_code, 100);
        assert!(output.error.unwrap().contains("apt-get failed"));
        // No restart after a failure.
        assert_eq!(
            fakes.lines("calls"),
            ["-n @monitorflow-agent packages software"]
        );
    }

    fn signed(ca: &Ca, id: &str, kind: &str, payload: Vec<u8>) -> Command {
        ca.sign(Command {
            command_id: id.into(),
            kind: kind.into(),
            payload,
            issued_at_unix_ms: NOW - 1_000,
            expires_at_unix_ms: NOW + 60_000,
            agent_id: "web-01".into(),
            signature: Vec::new(),
        })
    }

    #[test]
    fn the_helper_needs_the_consent_no_off_and_a_signed_command_of_its_kind() {
        let fakes = Fakes::new(true, 0);
        let ca = Ca::new();
        let host = Host::pinned(&ca);
        let authorized = |config: &Path, command: &Command, family: OsFamily| {
            let input = HelperRequest::of(command).framed();
            helper::authorized(
                config,
                &mut input.as_slice(),
                |_| host.place.clone(),
                family,
                NOW,
            )
        };
        let install = signed(&ca, "c-1", SOFTWARE, apt_install(&["nginx"]));
        let refused = authorized(&fakes.config(false), &install, OsFamily::Debian).unwrap_err();
        assert!(
            matches!(refused, SoftwareHelperError::NotAllowed(_)),
            "{refused}"
        );

        let allowed = fakes.config(true);
        std::fs::write(fakes.path(OFF_FILE), "off\n").unwrap();
        assert!(matches!(
            authorized(&allowed, &install, OsFamily::Debian),
            Err(SoftwareHelperError::TurnedOff)
        ));
        std::fs::remove_file(fakes.path(OFF_FILE)).unwrap();

        // apt packages on a RHEL host: checked again here.
        let on_rhel = signed(&ca, "c-6", SOFTWARE, apt_install(&["nginx"]));
        assert!(matches!(
            authorized(&allowed, &on_rhel, OsFamily::Rhel),
            Err(SoftwareHelperError::Invalid(_))
        ));
        let change = authorized(&allowed, &install, OsFamily::Debian).unwrap();
        assert_eq!(change.packages[0].id, "nginx");
        // Once only.
        assert!(matches!(
            authorized(&allowed, &install, OsFamily::Debian),
            Err(SoftwareHelperError::Trust(_))
        ));
        let mut unsigned_command = signed(&ca, "c-2", SOFTWARE, apt_install(&["nginx"]));
        unsigned_command.signature.clear();
        let shell = signed(&ca, "c-3", SHELL, apt_install(&["nginx"]));
        let other = signed(&Ca::new(), "c-4", SOFTWARE, apt_install(&["nginx"]));
        for command in [&unsigned_command, &shell, &other] {
            assert!(matches!(
                authorized(&allowed, command, OsFamily::Debian),
                Err(SoftwareHelperError::Trust(_))
            ));
        }
        let option = signed(&ca, "c-5", SOFTWARE, apt_install(&["--allow-downgrades"]));
        assert!(matches!(
            authorized(&allowed, &option, OsFamily::Debian),
            Err(SoftwareHelperError::Invalid(_))
        ));
    }

    #[tokio::test]
    async fn apt_installs_and_removes_with_fixed_lines_skipping_what_is_done() {
        let fakes = Fakes::new(true, 0);
        let tools = fakes.tools();
        let install = change(
            Manager::Apt,
            Action::Install,
            &["curl:amd64", "nginx", "gone"],
            false,
        );
        let outcome = helper::execute(&install, OsFamily::Debian, &tools).await;
        assert!(outcome.error.is_none(), "{:?}", outcome.error);
        assert_eq!(outcome.notes, ["curl:amd64 is installed already"]);
        assert_eq!(
            fakes.lines("programs"),
            [
                "apt-get -q update",
                "apt-get -q -y -o Dpkg::Options::=--force-confdef -o \
                 Dpkg::Options::=--force-confold --no-upgrade install nginx gone noninteractive",
            ]
        );
        std::fs::remove_file(fakes.path("programs")).unwrap();
        let remove = change(Manager::Apt, Action::Remove, &["vim", "nginx"], false);
        let outcome = helper::execute(&remove, OsFamily::Debian, &tools).await;
        assert_eq!(outcome.notes, ["nginx is not installed"]);
        assert_eq!(
            fakes.lines("programs"),
            ["apt-get -q -y -o Dpkg::Options::=--force-confdef -o \
              Dpkg::Options::=--force-confold remove vim noninteractive"]
        );
        // Nothing left to do: nothing runs.
        std::fs::remove_file(fakes.path("programs")).unwrap();
        let done = change(Manager::Apt, Action::Install, &["vim"], false);
        let outcome = helper::execute(&done, OsFamily::Debian, &tools).await;
        assert!(outcome.error.is_none() && outcome.runs.is_empty());
        assert!(fakes.lines("programs").is_empty());
    }

    #[tokio::test]
    async fn dnf_installs_and_removes_and_failures_keep_the_code() {
        let fakes = Fakes::new(true, 0);
        let tools = fakes.tools();
        let install = change(Manager::Dnf, Action::Install, &["httpd", "curl"], false);
        let outcome = helper::execute(&install, OsFamily::Rhel, &tools).await;
        assert!(outcome.error.is_none());
        let remove = change(Manager::Dnf, Action::Remove, &["vim"], false);
        helper::execute(&remove, OsFamily::Rhel, &tools).await;
        assert_eq!(
            fakes.lines("programs"),
            ["dnf -q -y install httpd", "dnf -q -y remove vim"]
        );
        std::fs::write(fakes.path("dnf-exit"), "1").unwrap();
        let outcome = helper::execute(&install, OsFamily::Rhel, &tools).await;
        assert_eq!(outcome.exit_code(), 1);
        assert!(outcome.error.is_some());
    }

    #[test]
    fn installed_lists_are_parsed() {
        let dpkg = helper::parse_dpkg_installed("a a:amd64 ii \nb b rc \nc c iU \n");
        assert!(dpkg.contains("a") && dpkg.contains("a:amd64"));
        assert!(!dpkg.contains("b") && !dpkg.contains("c"));
        let rpm = helper::parse_rpm_installed("bash\n\n  vim \n");
        assert_eq!(rpm.len(), 2);
        assert!(rpm.contains("vim"));
    }
}
