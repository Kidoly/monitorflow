//! The root side of AI root actions (ADR-0181): `monitorflow-agent
//! root-action`, run by sudo. In order, before running anything:
//!
//! 1. the host's consent, `commands.root_actions` read from `agent.toml`,
//!    which only root writes, without the environment;
//! 2. the command on stdin, signed by the central server for this machine,
//!    of kind `root_action`, not expired and not run before (ADR-0108);
//! 3. the action and its parameters ([`RootAction::from_proto`]).
//!
//! Each action then runs fixed programs with fixed options, in the C
//! locale; only the journal size and the service name come from the
//! command, both checked.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use monitorflow_common::command::ROOT_ACTION;
use monitorflow_common::config::{self, AgentConfig, Env};
use monitorflow_common::root_action::RootAction;

use crate::helper_trust::{authorize, HelperRequest, Place};

/// Where the programs are. [`Default`] is the real system; tests point
/// them at fakes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Programs {
    pub apt_get: PathBuf,
    pub dnf: PathBuf,
    pub zypper: PathBuf,
    pub journalctl: PathBuf,
    pub docker: PathBuf,
    pub systemctl: PathBuf,
}

impl Default for Programs {
    fn default() -> Self {
        Self {
            apt_get: "/usr/bin/apt-get".into(),
            dnf: "/usr/bin/dnf".into(),
            zypper: "/usr/bin/zypper".into(),
            journalctl: "/usr/bin/journalctl".into(),
            docker: "/usr/bin/docker".into(),
            systemctl: "/usr/bin/systemctl".into(),
        }
    }
}

/// One program the helper ran.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ran {
    pub line: String,
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// What an action did: every run in order, and why it failed, if it did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    pub runs: Vec<Ran>,
    pub error: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum RootActionError {
    #[error("AI root actions are not allowed on this server: set `root_actions = true` under [commands] in {0}, or run `monitorflow-agent ai-commands root-actions on`")]
    NotAllowed(String),
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    #[error(transparent)]
    Trust(#[from] crate::helper_trust::TrustError),
    #[error("{0}")]
    Invalid(String),
}

/// Checks the consent in `config_path`, then the signed command read from
/// `input`, and returns the action to run.
pub fn authorized(
    config_path: &Path,
    input: &mut impl Read,
    place: impl FnOnce(&AgentConfig) -> Place,
    now_unix_ms: i64,
) -> Result<RootAction, RootActionError> {
    // The file alone: sudo resets the environment, and the consent must
    // not come from anywhere the agent's user can write.
    let config: AgentConfig = config::load_with_env(config_path, &Env::new())?;
    if !config.commands.root_actions {
        return Err(RootActionError::NotAllowed(
            config_path.display().to_string(),
        ));
    }
    let request = HelperRequest::read(input)?;
    let command = authorize(&place(&config), &request, ROOT_ACTION, now_unix_ms)?;
    let message = <monitorflow_common::proto::v1::RootAction as prost::Message>::decode(
        command.payload.as_slice(),
    )
    .map_err(|e| RootActionError::Invalid(format!("invalid RootAction payload: {e}")))?;
    RootAction::from_proto(&message).map_err(RootActionError::Invalid)
}

/// Runs `action` with `programs`.
pub async fn execute(action: &RootAction, programs: &Programs) -> Outcome {
    let mut outcome = Outcome::default();
    match action {
        RootAction::CleanPackageCache => {
            let (program, args): (&Path, &[&str]) = if programs.apt_get.exists() {
                (&programs.apt_get, &["clean"])
            } else if programs.dnf.exists() {
                (&programs.dnf, &["clean", "packages"])
            } else if programs.zypper.exists() {
                (&programs.zypper, &["--non-interactive", "clean"])
            } else {
                outcome.error = Some(
                    "no package manager MonitorFlow knows (apt, dnf, zypper) is installed".into(),
                );
                return outcome;
            };
            outcome.step(program, args).await;
        }
        RootAction::VacuumJournal { max_megabytes } => {
            let size = format!("--vacuum-size={max_megabytes}M");
            outcome.step(&programs.journalctl, &[&size]).await;
        }
        RootAction::PruneDocker => {
            if !programs.docker.exists() {
                outcome.error = Some("Docker is not installed on this server".into());
                return outcome;
            }
            if outcome
                .step(&programs.docker, &["image", "prune", "--all", "--force"])
                .await
            {
                outcome
                    .step(&programs.docker, &["builder", "prune", "--all", "--force"])
                    .await;
            }
        }
        RootAction::RestartService { unit } => {
            outcome
                .step(&programs.systemctl, &["restart", unit.as_str()])
                .await;
        }
    }
    outcome
}

impl Outcome {
    /// Runs one program; false, with the error set, when it fails.
    async fn step(&mut self, program: &Path, args: &[&str]) -> bool {
        let line = format!("{} {}", program.display(), args.join(" "));
        let output = tokio::process::Command::new(program)
            .args(args)
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .output()
            .await;
        let ran = match output {
            Ok(output) => Ran {
                line,
                code: output.status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            },
            Err(error) => {
                self.error = Some(format!("cannot run {}: {error}", program.display()));
                return false;
            }
        };
        let ok = ran.code == 0;
        if !ok {
            let last = ran
                .stderr
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("no error output");
            self.error = Some(format!("`{}` exited with {}: {last}", ran.line, ran.code));
        }
        self.runs.push(ran);
        ok
    }
}
