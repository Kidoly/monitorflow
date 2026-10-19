//! apt and dpkg, for the Debian family.

use std::collections::HashMap;

use monitorflow_common::proto::v1::{ApplyUpdates, Package};

use super::exec::{Run, Tools};
use super::{PackageError, PackageManager};

/// Keeps local configuration files and never asks.
const UPGRADE_OPTIONS: [&str; 6] = [
    "-q",
    "-y",
    "-o",
    "Dpkg::Options::=--force-confdef",
    "-o",
    "Dpkg::Options::=--force-confold",
];
const NONINTERACTIVE: [(&str, &str); 1] = [("DEBIAN_FRONTEND", "noninteractive")];
const DPKG_FORMAT: &str = "${Package}\\t${Version}\\t${Architecture}\\t${db:Status-Abbrev}\\n";

#[derive(Debug, Clone)]
pub struct Apt {
    tools: Tools,
}

impl Apt {
    pub fn new(tools: Tools) -> Self {
        Self { tools }
    }

    async fn simulate(&self) -> Result<Vec<Candidate>, PackageError> {
        let run = self
            .tools
            .read(&self.tools.apt_get, &["-s", "-q", "dist-upgrade"])
            .await?;
        if !run.success() {
            return Err(PackageError::failed(&run));
        }
        Ok(parse_simulation(&String::from_utf8_lossy(&run.stdout)))
    }
}

#[async_trait::async_trait]
impl PackageManager for Apt {
    async fn refresh(&self) -> Result<Option<Run>, PackageError> {
        let run = self
            .tools
            .privileged(&[], &self.tools.apt_get, &["-q", "update"])
            .await?;
        Ok(Some(run))
    }

    async fn upgrade(&self, request: &ApplyUpdates) -> Result<Option<Run>, PackageError> {
        let mut packages = request.packages.clone();
        if request.security_only {
            let security: Vec<String> = self
                .simulate()
                .await?
                .into_iter()
                .filter(|c| c.security && c.installed.is_some())
                .map(|c| c.name)
                .filter(|name| packages.is_empty() || packages.contains(name))
                .collect();
            if security.is_empty() {
                return Ok(None);
            }
            packages = security;
        }
        let mut args: Vec<&str> = UPGRADE_OPTIONS.to_vec();
        if packages.is_empty() {
            args.push("upgrade");
        } else {
            args.extend(["install", "--only-upgrade"]);
            args.extend(packages.iter().map(String::as_str));
        }
        let run = self
            .tools
            .privileged(&NONINTERACTIVE, &self.tools.apt_get, &args)
            .await?;
        Ok(Some(run))
    }

    async fn inventory(&self) -> Result<Vec<Package>, PackageError> {
        let run = self
            .tools
            .read(&self.tools.dpkg_query, &["-W", "-f", DPKG_FORMAT])
            .await?;
        if !run.success() {
            return Err(PackageError::failed(&run));
        }
        let upgrades: HashMap<(String, String), String> = self
            .simulate()
            .await?
            .into_iter()
            .map(|c| ((c.name, c.arch), c.version))
            .collect();
        let mut packages = parse_dpkg(&String::from_utf8_lossy(&run.stdout));
        for package in &mut packages {
            if let Some(version) = upgrades.get(&(package.name.clone(), package.arch.clone())) {
                package.upgradable_version = version.clone();
            }
        }
        Ok(packages)
    }

    async fn reboot_required(&self) -> Result<bool, PackageError> {
        Ok(tokio::fs::try_exists(&self.tools.reboot_required_file)
            .await
            .unwrap_or(false))
    }
}

/// A package `apt-get -s dist-upgrade` would install or upgrade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub name: String,
    /// `None` for a new package.
    pub installed: Option<String>,
    pub version: String,
    pub arch: String,
    /// The candidate comes from a `-security` suite.
    pub security: bool,
}

/// `Inst <name> [<installed>] (<version> <origins…> [<arch>])` lines.
pub fn parse_simulation(text: &str) -> Vec<Candidate> {
    text.lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("Inst ")?;
            let (name, rest) = rest.split_once(' ')?;
            let (installed, rest) = match rest.strip_prefix('[') {
                Some(rest) => {
                    let (old, rest) = rest.split_once("] ")?;
                    (Some(old.to_owned()), rest)
                }
                None => (None, rest),
            };
            let inner = rest.strip_prefix('(')?.split_once(')')?.0;
            let (version, rest) = inner.split_once(' ')?;
            let (origins, arch) = rest.rsplit_once(' ')?;
            let arch = arch.strip_prefix('[')?.strip_suffix(']')?;
            Some(Candidate {
                name: name.to_owned(),
                installed,
                version: version.to_owned(),
                arch: arch.to_owned(),
                security: origins.contains("-security"),
            })
        })
        .collect()
}

/// `name\tversion\tarch\tstatus` lines; only installed packages.
pub fn parse_dpkg(text: &str) -> Vec<Package> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let (name, version, arch, status) = (
                fields.next()?,
                fields.next()?,
                fields.next()?,
                fields.next()?,
            );
            // The second letter is the current state: `i` for installed.
            (status.as_bytes().get(1) == Some(&b'i')).then(|| Package {
                name: name.to_owned(),
                version: version.to_owned(),
                arch: arch.to_owned(),
                upgradable_version: String::new(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simulation_lines_give_candidates() {
        let text = "NOTE: This is only a simulation!\n\
            Inst libc6 [2.36-9] (2.36-9+deb12u4 Debian-Security:12/stable-security [amd64])\n\
            Conf libc6 (2.36-9+deb12u4 Debian-Security:12/stable-security [amd64])\n\
            Inst tzdata [2024a-0+deb12u1] (2025b-0+deb12u1 Debian:12.11/stable, Debian:12.11/stable [all])\n\
            Inst linux-image-6.1.0-37-amd64 (6.1.140-1 Debian:12.11/stable [amd64])\n\
            Inst broken\n";
        let candidates = parse_simulation(text);
        assert_eq!(
            candidates,
            [
                Candidate {
                    name: "libc6".into(),
                    installed: Some("2.36-9".into()),
                    version: "2.36-9+deb12u4".into(),
                    arch: "amd64".into(),
                    security: true,
                },
                Candidate {
                    name: "tzdata".into(),
                    installed: Some("2024a-0+deb12u1".into()),
                    version: "2025b-0+deb12u1".into(),
                    arch: "all".into(),
                    security: false,
                },
                Candidate {
                    name: "linux-image-6.1.0-37-amd64".into(),
                    installed: None,
                    version: "6.1.140-1".into(),
                    arch: "amd64".into(),
                    security: false,
                },
            ]
        );
    }

    #[test]
    fn dpkg_lines_keep_installed_packages_only() {
        let text = "bash\t5.2.15-2+b8\tamd64\tii \n\
            old-conf\t1.0\tamd64\trc \n\
            held\t2.0\tall\thi \n\
            garbage\n";
        let names: Vec<_> = parse_dpkg(text)
            .into_iter()
            .map(|p| (p.name, p.arch))
            .collect();
        assert_eq!(
            names,
            [
                ("bash".to_owned(), "amd64".to_owned()),
                ("held".into(), "all".into())
            ]
        );
    }
}
