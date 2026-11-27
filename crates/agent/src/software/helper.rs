//! The root side of software installs on Linux (ADR-0188):
//! `monitorflow-agent packages software`, run by sudo. In order, before
//! running anything:
//!
//! 1. the host's consent, `commands.software` read from `agent.toml`,
//!    which only root writes, without the environment;
//! 2. that the web interface did not turn software installs off
//!    (`<data_dir>/software-off`, [`super::turned_off`]);
//! 3. the command on stdin, signed by the central server for this machine,
//!    of kind `software`, not expired and not run before (ADR-0108);
//! 4. the change itself ([`software::check_change`]) for the manager of
//!    this distribution.
//!
//! It then runs fixed command lines in the C locale, names checked by the
//! distributions' grammar, so none is an option, a path, a glob or a
//! group: `apt-get -q update`, then `apt-get -q -y -o
//! Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold
//! --no-upgrade install <names>` or `... remove <names>`, with
//! `DEBIAN_FRONTEND=noninteractive`; `dnf -q -y install <names>` or
//! `dnf -q -y remove <names>`. Never `purge`, `autoremove` or a file.

use std::collections::HashSet;
use std::io::Read;
use std::path::Path;

use monitorflow_common::command::SOFTWARE;
use monitorflow_common::config::{self, AgentConfig, Env};
use monitorflow_common::proto::v1::{software_change::Action, SoftwareChange};
use monitorflow_common::software;
use prost::Message;

use crate::helper_trust::{authorize, HelperRequest, Place};
use crate::os::OsFamily;
use crate::packages::update::APT_UPGRADE_OPTIONS;
use crate::packages::{PackageError, Run, Tools};

#[derive(Debug, thiserror::Error)]
pub enum SoftwareHelperError {
    #[error("software installs are not allowed on this server: set `software = true` under [commands] in {0}, or run `monitorflow-agent software on`")]
    NotAllowed(String),
    #[error("{}", super::TURNED_OFF)]
    TurnedOff,
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    #[error(transparent)]
    Trust(#[from] crate::helper_trust::TrustError),
    #[error("{0}")]
    Invalid(String),
}

/// Checks the consent in `config_path`, the web interface's "off", then
/// the signed command read from `input`, and returns the change, checked
/// for `family`.
pub fn authorized(
    config_path: &Path,
    input: &mut impl Read,
    place: impl FnOnce(&AgentConfig) -> Place,
    family: OsFamily,
    now_unix_ms: i64,
) -> Result<SoftwareChange, SoftwareHelperError> {
    // The file alone: sudo resets the environment, and the consent must
    // not come from anywhere the agent's user can write.
    let config: AgentConfig = config::load_with_env(config_path, &Env::new())?;
    if !config.commands.software {
        return Err(SoftwareHelperError::NotAllowed(
            config_path.display().to_string(),
        ));
    }
    if super::turned_off(&config.data_dir) {
        return Err(SoftwareHelperError::TurnedOff);
    }
    let request = HelperRequest::read(input)?;
    let command = authorize(&place(&config), &request, SOFTWARE, now_unix_ms)?;
    let change = SoftwareChange::decode(command.payload.as_slice()).map_err(|e| {
        SoftwareHelperError::Invalid(format!("invalid SoftwareChange payload: {e}"))
    })?;
    software::check_change(&change, Some(family.as_str()))
        .map_err(|e| SoftwareHelperError::Invalid(e.to_string()))?;
    Ok(change)
}

/// What the helper did.
#[derive(Debug, Default)]
pub struct Outcome {
    /// Lines to print first: packages skipped.
    pub notes: Vec<String>,
    /// Every run, in order; the last one failed when `error` is set.
    pub runs: Vec<Run>,
    pub error: Option<PackageError>,
}

impl Outcome {
    fn step(&mut self, run: Result<Run, PackageError>) -> bool {
        match run {
            Ok(run) => {
                let failed = (!run.success()).then(|| PackageError::failed(&run));
                self.runs.push(run);
                self.error = failed;
            }
            Err(error) => self.error = Some(error),
        }
        self.error.is_none()
    }

    /// The exit code for the helper: the failing tool's, or 1.
    pub fn exit_code(&self) -> i32 {
        match &self.error {
            None => 0,
            Some(PackageError::Failed { code, .. }) if *code > 0 => *code,
            Some(_) => 1,
        }
    }
}

const NONINTERACTIVE: [(&str, &str); 1] = [("DEBIAN_FRONTEND", "noninteractive")];

/// `apt-get` arguments installing or removing `names`.
pub fn apt_args(install: bool, names: &[String]) -> Vec<String> {
    let mut args: Vec<String> = APT_UPGRADE_OPTIONS.iter().map(|a| a.to_string()).collect();
    if install {
        // An installed package stays at its version: updates are campaigns'.
        args.extend(["--no-upgrade".into(), "install".into()]);
    } else {
        args.push("remove".into());
    }
    args.extend(names.iter().cloned());
    args
}

/// `dnf` arguments installing or removing `names`.
pub fn dnf_args(install: bool, names: &[String]) -> Vec<String> {
    let verb = if install { "install" } else { "remove" };
    let mut args = vec!["-q".to_owned(), "-y".into(), verb.into()];
    args.extend(names.iter().cloned());
    args
}

/// The installed package names of `dpkg-query -W -f '${Package}
/// ${binary:Package} ${db:Status-Abbrev}\n'`: both names of each package
/// whose status starts with `ii`.
pub fn parse_dpkg_installed(text: &str) -> HashSet<String> {
    let mut names = HashSet::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if let [name, binary, status, ..] = fields.as_slice() {
            if status.starts_with("ii") {
                names.insert((*name).to_owned());
                names.insert((*binary).to_owned());
            }
        }
    }
    names
}

/// The installed package names of `rpm -qa --qf '%{NAME}\n'`.
pub fn parse_rpm_installed(text: &str) -> HashSet<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

async fn installed(family: OsFamily, tools: &Tools) -> Result<HashSet<String>, PackageError> {
    match family {
        OsFamily::Debian => {
            let run = tools
                .read(
                    &tools.dpkg_query,
                    &[
                        "-W",
                        "-f",
                        "${Package} ${binary:Package} ${db:Status-Abbrev}\\n",
                    ],
                )
                .await?;
            Ok(parse_dpkg_installed(&String::from_utf8_lossy(&run.stdout)))
        }
        OsFamily::Rhel => {
            let run = tools
                .read(&tools.rpm, &["-qa", "--qf", "%{NAME}\\n"])
                .await?;
            if !run.success() {
                return Err(PackageError::failed(&run));
            }
            Ok(parse_rpm_installed(&String::from_utf8_lossy(&run.stdout)))
        }
        OsFamily::Linux => Err(PackageError::Unavailable),
    }
}

/// Runs `change`, already checked for `family`, as the current user (root
/// in the helper). An install skips the packages installed already, a
/// removal those not installed, each with a line; neither is an error.
pub async fn execute(change: &SoftwareChange, family: OsFamily, tools: &Tools) -> Outcome {
    let mut outcome = Outcome::default();
    let install = change.action == i32::from(Action::Install);
    let present = match installed(family, tools).await {
        Ok(present) => present,
        Err(error) => {
            outcome.error = Some(error);
            return outcome;
        }
    };
    let mut names = Vec::new();
    for package in &change.packages {
        let is_there = present.contains(&package.id);
        if install && is_there {
            outcome
                .notes
                .push(format!("{} is installed already", package.id));
        } else if !install && !is_there {
            outcome
                .notes
                .push(format!("{} is not installed", package.id));
        } else {
            names.push(package.id.clone());
        }
    }
    if names.is_empty() {
        return outcome;
    }
    match family {
        OsFamily::Debian => {
            if install
                && !outcome.step(
                    tools
                        .direct(&[], &tools.apt_get, &["-q".into(), "update".into()])
                        .await,
                )
            {
                return outcome;
            }
            outcome.step(
                tools
                    .direct(&NONINTERACTIVE, &tools.apt_get, &apt_args(install, &names))
                    .await,
            );
        }
        OsFamily::Rhel => {
            outcome.step(
                tools
                    .direct(&[], &tools.dnf, &dnf_args(install, &names))
                    .await,
            );
        }
        OsFamily::Linux => outcome.error = Some(PackageError::Unavailable),
    }
    outcome
}
