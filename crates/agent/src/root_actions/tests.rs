//! Root actions against fakes: a fake `sudo` that logs the exact lines
//! sudoers must allow, a fake helper that saves its stdin, and fake
//! package managers, journalctl, docker and systemctl that log their
//! arguments. The end-to-end test signs a command as the central server,
//! sends it through the handler and sudo, and gives what the helper read
//! to the real helper code.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use monitorflow_common::command::{ROOT_ACTION, SHELL};
use monitorflow_common::proto::v1::{self, Command, CommandAcceptance};
use monitorflow_common::root_action::RootAction;
use monitorflow_sensors::testing::write_executable;
use prost::Message;
use tempfile::TempDir;

use super::helper::{self, Programs, RootActionError};
use super::*;
use crate::helper_trust::tests::{sent, unsigned, Ca, Host, NOW};

/// A minimal `agent.toml`, with or without the consent.
fn config(root_actions: bool) -> String {
    format!(
        "upstream_url = \"https://mf.example:7443\"\n\n[commands]\nroot_actions = {root_actions}\n"
    )
}

struct Fakes {
    dir: TempDir,
}

impl Fakes {
    /// `sudo` refuses `-l` unless `rule`; the helper exits with `exit`
    /// and `error` on stderr.
    fn new(rule: bool, exit: i32, error: &str) -> Self {
        let dir = TempDir::new().unwrap();
        let path = |name: &str| dir.path().join(name).display().to_string();
        write_executable(
            &dir.path().join("sudo"),
            &format!(
                "#!/bin/sh\necho \"$*\" >> '{calls}'\n[ \"$1\" = -n ] || exit 1\nshift\n\
                 if [ \"$1\" = -l ]; then [ {rule} = true ] && {{ echo \"$2 $3\"; exit 0; }}; \
                 echo 'sudo: a password is required' >&2; exit 1; fi\nexec \"$@\"\n",
                calls = path("calls"),
            ),
            0o755,
        );
        write_executable(
            &dir.path().join("monitorflow-agent"),
            &format!(
                "#!/bin/sh\ncat > '{}'\necho '$ /usr/bin/apt-get clean'\n\
                 [ {exit} -eq 0 ] || echo '{error}' >&2\nexit {exit}\n",
                path("stdin")
            ),
            0o755,
        );
        // Each program logs its arguments under its own name.
        for name in [
            "apt-get",
            "dnf",
            "zypper",
            "journalctl",
            "docker",
            "systemctl",
        ] {
            write_executable(
                &dir.path().join(name),
                &format!(
                    "#!/bin/sh\necho \"{name} $*\" >> '{}'\necho \"ran {name}\"\n\
                     [ \"$LC_ALL\" = C ] || exit 7\nexit $(cat '{}' 2>/dev/null || echo 0)\n",
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
            ..Tools::default()
        }
    }

    fn programs(&self) -> Programs {
        Programs {
            apt_get: self.path("apt-get"),
            dnf: self.path("dnf"),
            zypper: self.path("zypper"),
            journalctl: self.path("journalctl"),
            docker: self.path("docker"),
            systemctl: self.path("systemctl"),
        }
    }

    fn handler(&self, on: bool, allowed: bool) -> RootActionHandler {
        RootActionHandler::new(Arc::new(AiSwitch::fixed(on)), allowed, true, self.tools())
    }

    fn lines(&self, file: &str) -> Vec<String> {
        let dir = format!("{}/", self.dir.path().display());
        std::fs::read_to_string(self.path(file))
            .unwrap_or_default()
            .lines()
            .map(|l| l.replace(&dir, "@"))
            .collect()
    }

    fn remove(&self, name: &str) {
        std::fs::remove_file(self.path(name)).unwrap();
    }

    fn fails(&self, name: &str, code: i32) {
        std::fs::write(self.path(&format!("{name}-exit")), code.to_string()).unwrap();
    }

    /// `agent.toml` in the fake directory.
    fn config(&self, root_actions: bool) -> PathBuf {
        let path = self.path("agent.toml");
        std::fs::write(&path, config(root_actions)).unwrap();
        path
    }
}

fn payload(line: &str) -> Vec<u8> {
    let words: Vec<&str> = line.split_whitespace().collect();
    RootAction::parse(&words)
        .unwrap()
        .to_proto()
        .encode_to_vec()
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

#[tokio::test]
async fn the_handler_sends_the_signed_command_to_the_one_sudo_line() {
    let fakes = Fakes::new(true, 0, "");
    let command = unsigned(ROOT_ACTION, &payload("vacuum-journal 500M"));
    let output = fakes.handler(true, true).run_command(&command).await;
    assert_eq!(output.error, None);
    assert_eq!(output.exit_code, 0);
    assert_eq!(fakes.lines("calls"), ["-n @monitorflow-agent root-action"]);
    assert_eq!(sent(&std::fs::read(fakes.path("stdin")).unwrap()), command);
    assert!(String::from_utf8_lossy(&output.stdout).contains("apt-get clean"));
}

#[tokio::test]
async fn nothing_runs_without_ai_commands_the_consent_or_a_valid_payload() {
    let fakes = Fakes::new(true, 0, "");
    let good = unsigned(ROOT_ACTION, &payload("prune-docker"));
    let off = fakes.handler(false, true).run_command(&good).await;
    assert!(off.rejected, "{off:?}");
    let refused = fakes.handler(true, false).run_command(&good).await;
    assert!(refused.rejected);
    assert_eq!(refused.error.as_deref(), Some(NOT_ALLOWED));
    let handler = fakes.handler(true, true);
    let sshd = v1::RootAction {
        action: Some(v1::root_action::Action::RestartService(
            v1::RestartService {
                unit: "sshd.service".into(),
            },
        )),
    };
    for bad in [
        b"\xff\xff".to_vec(),
        Vec::new(),
        sshd.encode_to_vec(),
        v1::ShellCommand {
            command: "rm -rf /".into(),
            timeout_seconds: 1,
        }
        .encode_to_vec(),
    ] {
        let output = handler.run_command(&unsigned(ROOT_ACTION, &bad)).await;
        assert!(output.rejected, "{bad:?}: {output:?}");
    }
    assert!(handler.run(&payload("prune-docker")).await.rejected);
    assert!(fakes.lines("calls").is_empty(), "sudo never ran");
}

#[tokio::test]
async fn old_sudo_rules_are_named_and_the_helper_error_kept() {
    let fakes = Fakes::new(false, 1, "sudo: a password is required");
    let command = unsigned(ROOT_ACTION, &payload("clean-package-cache"));
    let output = fakes.handler(true, true).run_command(&command).await;
    assert_eq!(output.error.as_deref(), Some(OLD_SUDO_RULES));
    assert!(!output.rejected);

    let fakes = Fakes::new(true, 3, "Error: Docker is not installed on this server");
    let output = fakes.handler(true, true).run_command(&command).await;
    assert_eq!(output.exit_code, 3);
    assert_eq!(
        output.error.as_deref(),
        Some("Docker is not installed on this server")
    );
}

#[tokio::test]
async fn acceptance_follows_the_switch_the_consent_and_the_sudo_rule() {
    let with_rule = Fakes::new(true, 0, "");
    assert!(sudo_rule(&with_rule.tools()).await);
    assert_eq!(
        with_rule.lines("calls"),
        ["-n -l @monitorflow-agent root-action"]
    );
    assert!(!sudo_rule(&Fakes::new(false, 0, "").tools()).await);
    let missing = Tools {
        sudo: "/nonexistent/sudo".into(),
        ..Tools::default()
    };
    assert!(!sudo_rule(&missing).await);

    let tools = with_rule.tools();
    let state = |on, allowed, rule| {
        RootActionHandler::new(Arc::new(AiSwitch::fixed(on)), allowed, rule, tools.clone())
            .acceptance()
    };
    assert_eq!(state(true, true, true), CommandAcceptance::Accepted);
    assert_eq!(state(false, true, true), CommandAcceptance::Disabled);
    assert_eq!(state(true, false, true), CommandAcceptance::Disabled);
    assert_eq!(state(true, true, false), CommandAcceptance::Unsupported);
}

#[test]
fn the_helper_needs_the_consent_then_a_signed_command_of_its_kind() {
    let fakes = Fakes::new(true, 0, "");
    let ca = Ca::new();
    let host = Host::pinned(&ca);
    let authorized = |config: &Path, command: &Command| {
        let input = HelperRequest::of(command).framed();
        helper::authorized(config, &mut input.as_slice(), |_| host.place.clone(), NOW)
    };
    let journal = signed(&ca, "c-1", ROOT_ACTION, payload("vacuum-journal 1G"));

    let refused = authorized(&fakes.config(false), &journal).unwrap_err();
    assert!(
        matches!(refused, RootActionError::NotAllowed(_)),
        "{refused}"
    );

    let allowed = fakes.config(true);
    assert_eq!(
        authorized(&allowed, &journal).unwrap(),
        RootAction::VacuumJournal {
            max_megabytes: 1024
        }
    );
    // Once only.
    assert!(matches!(
        authorized(&allowed, &journal),
        Err(RootActionError::Trust(_))
    ));
    // Unsigned, or signed for another kind, or by another authority.
    let mut unsigned_command = signed(&ca, "c-2", ROOT_ACTION, payload("prune-docker"));
    unsigned_command.signature.clear();
    let shell = signed(&ca, "c-3", SHELL, payload("prune-docker"));
    let other = signed(&Ca::new(), "c-4", ROOT_ACTION, payload("prune-docker"));
    for command in [&unsigned_command, &shell, &other] {
        assert!(matches!(
            authorized(&allowed, command),
            Err(RootActionError::Trust(_))
        ));
    }
    // Signed, but a parameter the catalog refuses.
    let tiny = v1::RootAction {
        action: Some(v1::root_action::Action::VacuumJournal(v1::VacuumJournal {
            max_megabytes: 1,
        })),
    };
    let command = signed(&ca, "c-5", ROOT_ACTION, tiny.encode_to_vec());
    assert!(matches!(
        authorized(&allowed, &command),
        Err(RootActionError::Invalid(_))
    ));
}

#[tokio::test]
async fn each_action_runs_fixed_programs_with_fixed_options() {
    let fakes = Fakes::new(true, 0, "");
    let programs = fakes.programs();
    for (line, expected) in [
        ("clean-package-cache", vec!["apt-get clean"]),
        ("vacuum-journal 2G", vec!["journalctl --vacuum-size=2048M"]),
        (
            "prune-docker",
            vec![
                "docker image prune --all --force",
                "docker builder prune --all --force",
            ],
        ),
        (
            "restart-service nginx",
            vec!["systemctl restart nginx.service"],
        ),
    ] {
        std::fs::remove_file(fakes.path("programs")).ok();
        let action = RootAction::parse(&line.split_whitespace().collect::<Vec<_>>()).unwrap();
        let outcome = helper::execute(&action, &programs).await;
        assert_eq!(outcome.error, None, "{line}");
        assert_eq!(fakes.lines("programs"), expected, "{line}");
        assert_eq!(outcome.runs.len(), expected.len());
    }
    // dnf, then zypper, where apt is missing.
    fakes.remove("apt-get");
    std::fs::remove_file(fakes.path("programs")).ok();
    helper::execute(&RootAction::CleanPackageCache, &programs).await;
    fakes.remove("dnf");
    helper::execute(&RootAction::CleanPackageCache, &programs).await;
    assert_eq!(
        fakes.lines("programs"),
        ["dnf clean packages", "zypper --non-interactive clean"]
    );
    fakes.remove("zypper");
    let none = helper::execute(&RootAction::CleanPackageCache, &programs).await;
    assert!(none.error.unwrap().contains("no package manager"));
}

#[tokio::test]
async fn failures_stop_and_say_why() {
    let fakes = Fakes::new(true, 0, "");
    let programs = fakes.programs();
    fakes.fails("docker", 1);
    let outcome = helper::execute(&RootAction::PruneDocker, &programs).await;
    assert_eq!(
        outcome.runs.len(),
        1,
        "the build cache is left after a failure"
    );
    assert!(outcome.error.unwrap().contains("exited with 1"));
    fakes.remove("docker");
    let outcome = helper::execute(&RootAction::PruneDocker, &programs).await;
    assert_eq!(
        outcome.error.as_deref(),
        Some("Docker is not installed on this server")
    );
    assert!(outcome.runs.is_empty());
}

/// The whole path: the central server's signed command, the handler, the
/// exact sudo line, the helper's checks and the program it runs.
#[tokio::test]
async fn end_to_end_with_a_fake_sudo() {
    let fakes = Fakes::new(true, 0, "");
    let ca = Ca::new();
    let host = Host::pinned(&ca);
    let command = signed(
        &ca,
        "c-9",
        ROOT_ACTION,
        payload("restart-service php8.2-fpm"),
    );

    let output = fakes.handler(true, true).run_command(&command).await;
    assert_eq!(output.error, None);
    assert_eq!(fakes.lines("calls"), ["-n @monitorflow-agent root-action"]);

    let stdin = std::fs::read(fakes.path("stdin")).unwrap();
    let action = helper::authorized(
        &fakes.config(true),
        &mut stdin.as_slice(),
        |_| host.place.clone(),
        NOW,
    )
    .unwrap();
    let outcome = helper::execute(&action, &fakes.programs()).await;
    assert_eq!(outcome.error, None);
    assert_eq!(
        fakes.lines("programs"),
        ["systemctl restart php8.2-fpm.service"]
    );
    // The same request again is a replay.
    assert!(helper::authorized(
        &fakes.config(true),
        &mut stdin.as_slice(),
        |_| host.place.clone(),
        NOW,
    )
    .is_err());
}
