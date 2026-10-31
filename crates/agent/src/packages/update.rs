//! The `packages update` root helper (ADR-0036): the agent runs `sudo -n
//! /usr/bin/monitorflow-agent packages update` with an [`UpdateSpec`] as
//! JSON on stdin, and the helper, as root, checks it and runs apt or dnf
//! itself. Sudoers then names one fixed command line, whatever packages
//! the update names.

use monitorflow_common::proto::v1::ApplyUpdates;
use serde::{Deserialize, Serialize};

use super::apt::{Apt, Candidate};
use super::exec::{Run, Tools};
use super::{valid_name, PackageError};
use crate::os::OsFamily;

/// Longest spec read on stdin.
pub const MAX_SPEC_BYTES: usize = 64 * 1024;
/// Most package names in one spec.
pub const MAX_PACKAGES: usize = 1024;

/// Keeps local configuration files and never asks.
pub const APT_UPGRADE_OPTIONS: [&str; 6] = [
    "-q",
    "-y",
    "-o",
    "Dpkg::Options::=--force-confdef",
    "-o",
    "Dpkg::Options::=--force-confold",
];
const NONINTERACTIVE: [(&str, &str); 1] = [("DEBIAN_FRONTEND", "noninteractive")];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateKind {
    /// Every upgradable package.
    All,
    /// Security updates only, of `packages` when not empty.
    Security,
    /// The named `packages`, at least one.
    Named,
}

impl UpdateKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Security => "security",
            Self::Named => "named",
        }
    }
}

/// What to update, as the helper reads it on stdin:
/// `{"kind": "named", "packages": ["openssl"]}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateSpec {
    pub kind: UpdateKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub packages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpecError {
    #[error("the spec is larger than {MAX_SPEC_BYTES} bytes")]
    TooLarge,
    #[error("the spec is not an update spec: {0}")]
    Json(String),
    #[error("`{0}` updates take no package names")]
    UnexpectedPackages(&'static str),
    #[error("`named` updates need at least one package name")]
    NoPackages,
    #[error("more than {MAX_PACKAGES} package names")]
    TooManyPackages,
    #[error("invalid package name `{0}`")]
    InvalidName(String),
}

impl UpdateSpec {
    /// The spec for an `apply_updates` command; check it with
    /// [`Self::validate`].
    pub fn from_request(request: &ApplyUpdates) -> Self {
        let kind = if request.security_only {
            UpdateKind::Security
        } else if request.packages.is_empty() {
            UpdateKind::All
        } else {
            UpdateKind::Named
        };
        Self {
            kind,
            packages: request.packages.clone(),
        }
    }

    /// Reads and checks a spec from the helper's stdin.
    pub fn parse(bytes: &[u8]) -> Result<Self, SpecError> {
        if bytes.len() > MAX_SPEC_BYTES {
            return Err(SpecError::TooLarge);
        }
        let spec: Self =
            serde_json::from_slice(bytes).map_err(|e| SpecError::Json(e.to_string()))?;
        spec.validate()?;
        Ok(spec)
    }

    /// Package names follow the distributions' grammar ([`valid_name`]),
    /// so none can be an option or a path, and only `named` and
    /// `security` take them.
    pub fn validate(&self) -> Result<(), SpecError> {
        match self.kind {
            UpdateKind::All if !self.packages.is_empty() => {
                return Err(SpecError::UnexpectedPackages(self.kind.as_str()))
            }
            UpdateKind::Named if self.packages.is_empty() => return Err(SpecError::NoPackages),
            _ => {}
        }
        if self.packages.len() > MAX_PACKAGES {
            return Err(SpecError::TooManyPackages);
        }
        match self.packages.iter().find(|p| !valid_name(p)) {
            Some(bad) => Err(SpecError::InvalidName(bad.escape_debug().to_string())),
            None => Ok(()),
        }
    }

    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a spec always serializes")
    }
}

/// `apt-get` arguments refreshing the metadata.
pub fn apt_refresh() -> Vec<String> {
    vec!["-q".into(), "update".into()]
}

/// `apt-get` arguments upgrading everything, or only `packages`.
pub fn apt_upgrade(packages: &[String]) -> Vec<String> {
    let mut args: Vec<String> = APT_UPGRADE_OPTIONS.iter().map(|a| a.to_string()).collect();
    if packages.is_empty() {
        args.push("upgrade".into());
    } else {
        args.extend(["install".into(), "--only-upgrade".into()]);
        args.extend(packages.iter().cloned());
    }
    args
}

/// The installed packages whose candidate comes from a `-security` suite,
/// among `only` when not empty. apt has no security switch: this is the
/// list `apt_upgrade` then names.
pub fn apt_security(candidates: &[Candidate], only: &[String]) -> Vec<String> {
    candidates
        .iter()
        .filter(|c| c.security && c.installed.is_some() && valid_name(&c.name))
        .filter(|c| only.is_empty() || only.contains(&c.name))
        .map(|c| c.name.clone())
        .collect()
}

/// `dnf` arguments for `spec`.
pub fn dnf_upgrade(spec: &UpdateSpec) -> Vec<String> {
    let mut args = vec!["-q".to_owned(), "-y".into(), "upgrade".into()];
    if spec.kind == UpdateKind::Security {
        args.push("--security".into());
    }
    args.extend(spec.packages.iter().cloned());
    args
}

/// What the helper ran.
#[derive(Debug, Default)]
pub struct Outcome {
    /// Every run, in order; the last one failed when `error` is set.
    pub runs: Vec<Run>,
    /// False when nothing matched, so no upgrade ran.
    pub upgraded: bool,
    pub error: Option<PackageError>,
}

impl Outcome {
    /// Records `run`; false when it failed.
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

/// Runs the update `spec`, already checked, with the host's package
/// manager, as the current user (root in the helper). Stops at the first
/// failure.
pub async fn run(spec: &UpdateSpec, family: OsFamily, tools: &Tools) -> Outcome {
    let mut outcome = Outcome::default();
    match family {
        OsFamily::Debian => {
            if !outcome.step(tools.direct(&[], &tools.apt_get, &apt_refresh()).await) {
                return outcome;
            }
            let packages = match spec.kind {
                UpdateKind::All => Vec::new(),
                UpdateKind::Named => spec.packages.clone(),
                UpdateKind::Security => match Apt::new(tools.clone()).simulate().await {
                    Ok(candidates) => apt_security(&candidates, &spec.packages),
                    Err(error) => {
                        outcome.error = Some(error);
                        return outcome;
                    }
                },
            };
            if spec.kind == UpdateKind::Security && packages.is_empty() {
                return outcome;
            }
            let args = apt_upgrade(&packages);
            outcome.upgraded = true;
            outcome.step(tools.direct(&NONINTERACTIVE, &tools.apt_get, &args).await);
        }
        OsFamily::Rhel => {
            outcome.upgraded = true;
            outcome.step(tools.direct(&[], &tools.dnf, &dnf_upgrade(spec)).await);
        }
        OsFamily::Linux => outcome.error = Some(PackageError::Unavailable),
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(kind: UpdateKind, packages: &[&str]) -> UpdateSpec {
        UpdateSpec {
            kind,
            packages: packages.iter().map(|p| p.to_string()).collect(),
        }
    }

    #[tokio::test]
    async fn an_unknown_distribution_runs_nothing() {
        let outcome = run(
            &spec(UpdateKind::All, &[]),
            OsFamily::Linux,
            &Tools::default(),
        )
        .await;
        assert_eq!(outcome.error, Some(PackageError::Unavailable));
        assert!(outcome.runs.is_empty());
        assert!(!outcome.upgraded);
        assert_eq!(outcome.exit_code(), 1);
    }

    #[test]
    fn specs_parse_from_json() {
        assert_eq!(
            UpdateSpec::parse(br#"{"kind":"all"}"#),
            Ok(spec(UpdateKind::All, &[]))
        );
        assert_eq!(
            UpdateSpec::parse(br#"{"kind":"security"}"#),
            Ok(spec(UpdateKind::Security, &[]))
        );
        assert_eq!(
            UpdateSpec::parse(br#"{"kind":"security","packages":["libc6"]}"#),
            Ok(spec(UpdateKind::Security, &["libc6"]))
        );
        assert_eq!(
            UpdateSpec::parse(br#"{"kind":"named","packages":["openssl","libstdc++6:amd64"]}"#),
            Ok(spec(UpdateKind::Named, &["openssl", "libstdc++6:amd64"]))
        );
    }

    #[test]
    fn malformed_specs_are_refused() {
        for bad in [
            &b""[..],
            b"\xff",
            b"[]",
            br#"{"packages":["a"]}"#,
            br#"{"kind":"dist-upgrade"}"#,
            br#"{"kind":"ALL"}"#,
            br#"{"kind":"all","options":["-o"]}"#,
            br#"{"kind":"named","packages":"openssl"}"#,
        ] {
            let error = UpdateSpec::parse(bad).unwrap_err();
            assert!(matches!(error, SpecError::Json(_)), "{error:?}");
        }
        let large = vec![b' '; MAX_SPEC_BYTES + 1];
        assert_eq!(UpdateSpec::parse(&large), Err(SpecError::TooLarge));
    }

    #[test]
    fn specs_are_checked() {
        assert_eq!(
            UpdateSpec::parse(br#"{"kind":"all","packages":["bash"]}"#),
            Err(SpecError::UnexpectedPackages("all"))
        );
        assert_eq!(
            UpdateSpec::parse(br#"{"kind":"named","packages":[]}"#),
            Err(SpecError::NoPackages)
        );
        assert_eq!(
            UpdateSpec::parse(br#"{"kind":"named"}"#),
            Err(SpecError::NoPackages)
        );
        for bad in [
            "-o",
            "--allow-downgrades",
            "/tmp/evil.deb",
            "./evil.rpm",
            "a b",
            "x;reboot",
            "$(id)",
            "",
            "a\n",
        ] {
            let error = spec(UpdateKind::Named, &["bash", bad])
                .validate()
                .unwrap_err();
            assert!(matches!(error, SpecError::InvalidName(_)), "{bad}");
        }
        let many: Vec<String> = (0..=MAX_PACKAGES).map(|i| format!("p{i}")).collect();
        let too_many = UpdateSpec {
            kind: UpdateKind::Named,
            packages: many,
        };
        assert_eq!(too_many.validate(), Err(SpecError::TooManyPackages));
    }

    #[test]
    fn requests_map_to_specs_that_round_trip() {
        let request = |packages: &[&str], security_only| ApplyUpdates {
            packages: packages.iter().map(|p| p.to_string()).collect(),
            security_only,
            allow_reboot: true,
        };
        for (request, expected, json) in [
            (
                request(&[], false),
                spec(UpdateKind::All, &[]),
                r#"{"kind":"all"}"#,
            ),
            (
                request(&["bash"], false),
                spec(UpdateKind::Named, &["bash"]),
                r#"{"kind":"named","packages":["bash"]}"#,
            ),
            (
                request(&[], true),
                spec(UpdateKind::Security, &[]),
                r#"{"kind":"security"}"#,
            ),
            (
                request(&["bash"], true),
                spec(UpdateKind::Security, &["bash"]),
                r#"{"kind":"security","packages":["bash"]}"#,
            ),
        ] {
            let spec = UpdateSpec::from_request(&request);
            assert_eq!(spec, expected);
            assert_eq!(String::from_utf8(spec.to_json()).unwrap(), json);
            assert_eq!(UpdateSpec::parse(json.as_bytes()), Ok(spec));
        }
    }

    #[test]
    fn apt_command_lines() {
        assert_eq!(apt_refresh(), ["-q", "update"]);
        let options = APT_UPGRADE_OPTIONS.join(" ");
        assert_eq!(apt_upgrade(&[]).join(" "), format!("{options} upgrade"));
        assert_eq!(
            apt_upgrade(&["openssl".into(), "libssl3".into()]).join(" "),
            format!("{options} install --only-upgrade openssl libssl3")
        );
    }

    #[test]
    fn apt_security_picks_installed_security_candidates() {
        let candidate = |name: &str, installed: bool, security| Candidate {
            name: name.into(),
            installed: installed.then(|| "1".into()),
            version: "2".into(),
            arch: "amd64".into(),
            security,
        };
        let candidates = [
            candidate("libc6", true, true),
            candidate("tzdata", true, false),
            candidate("linux-image-new", false, true),
            candidate("openssl", true, true),
            candidate("-weird", true, true),
        ];
        assert_eq!(apt_security(&candidates, &[]), ["libc6", "openssl"]);
        assert_eq!(
            apt_security(&candidates, &["openssl".into(), "tzdata".into()]),
            ["openssl"]
        );
    }

    #[test]
    fn dnf_command_lines() {
        let line = |kind, packages: &[&str]| dnf_upgrade(&spec(kind, packages)).join(" ");
        assert_eq!(line(UpdateKind::All, &[]), "-q -y upgrade");
        assert_eq!(line(UpdateKind::Security, &[]), "-q -y upgrade --security");
        assert_eq!(
            line(UpdateKind::Security, &["openssl"]),
            "-q -y upgrade --security openssl"
        );
        assert_eq!(
            line(UpdateKind::Named, &["openssl", "bash"]),
            "-q -y upgrade openssl bash"
        );
    }
}
