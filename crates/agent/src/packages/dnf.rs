//! dnf and rpm, for the RHEL family.

use std::collections::HashMap;

use monitorflow_common::proto::v1::{ApplyUpdates, Package};

use super::exec::{Run, Tools};
use super::{PackageError, PackageManager};

const RPM_FORMAT: &str = "%{NAME}\\t%|EPOCH?{%{EPOCH}:}:{}|%{VERSION}-%{RELEASE}\\t%{ARCH}\\n";
/// `dnf check-update`: updates are available.
const UPDATES_AVAILABLE: i32 = 100;

#[derive(Debug, Clone)]
pub struct Dnf {
    tools: Tools,
}

impl Dnf {
    pub fn new(tools: Tools) -> Self {
        Self { tools }
    }
}

#[async_trait::async_trait]
impl PackageManager for Dnf {
    /// `dnf upgrade` refreshes expired metadata itself.
    async fn refresh(&self) -> Result<Option<Run>, PackageError> {
        Ok(None)
    }

    async fn upgrade(&self, request: &ApplyUpdates) -> Result<Option<Run>, PackageError> {
        let mut args = vec!["-q", "-y", "upgrade"];
        if request.security_only {
            args.push("--security");
        }
        args.extend(request.packages.iter().map(String::as_str));
        let run = self.tools.privileged(&[], &self.tools.dnf, &args).await?;
        Ok(Some(run))
    }

    async fn inventory(&self) -> Result<Vec<Package>, PackageError> {
        let run = self
            .tools
            .read(&self.tools.rpm, &["-qa", "--queryformat", RPM_FORMAT])
            .await?;
        if !run.success() {
            return Err(PackageError::failed(&run));
        }
        // Cached metadata only: refreshing needs root and the network.
        let updates = self
            .tools
            .read(&self.tools.dnf, &["-q", "-C", "check-update"])
            .await?;
        if !matches!(updates.code, 0 | UPDATES_AVAILABLE) {
            return Err(PackageError::failed(&updates));
        }
        let upgrades = parse_check_update(&String::from_utf8_lossy(&updates.stdout));
        let mut packages = parse_rpm(&String::from_utf8_lossy(&run.stdout));
        for package in &mut packages {
            if let Some(version) = upgrades.get(&(package.name.clone(), package.arch.clone())) {
                package.upgradable_version = version.clone();
            }
        }
        Ok(packages)
    }

    /// `dnf needs-restarting -r` exits 1 when a reboot is needed.
    async fn reboot_required(&self) -> Result<bool, PackageError> {
        let run = self
            .tools
            .read(&self.tools.dnf, &["-q", "needs-restarting", "-r"])
            .await?;
        match run.code {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(PackageError::failed(&run)),
        }
    }
}

/// `name\t[epoch:]version-release\tarch` lines; `gpg-pubkey` entries,
/// which have no architecture, are not packages.
pub fn parse_rpm(text: &str) -> Vec<Package> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let (name, version, arch) = (fields.next()?, fields.next()?, fields.next()?);
            (arch != "(none)").then(|| Package {
                name: name.to_owned(),
                version: version.to_owned(),
                arch: arch.to_owned(),
                upgradable_version: String::new(),
            })
        })
        .collect()
}

/// `name.arch  version  repository` lines, up to the obsoletes section.
pub fn parse_check_update(text: &str) -> HashMap<(String, String), String> {
    text.lines()
        .take_while(|line| !line.starts_with("Obsoleting"))
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let (package, version, _repo) = (fields.next()?, fields.next()?, fields.next()?);
            let (name, arch) = package.rsplit_once('.')?;
            Some(((name.to_owned(), arch.to_owned()), version.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpm_lines_skip_keys() {
        let text = "bash\t5.1.8-9.el9\tx86_64\n\
            gpg-pubkey\t3228467c-613798eb\t(none)\n\
            shadow-utils\t2:4.9-9.el9\tx86_64\n";
        let packages = parse_rpm(text);
        let names: Vec<_> = packages
            .iter()
            .map(|p| (p.name.as_str(), p.version.as_str()))
            .collect();
        assert_eq!(
            names,
            [("bash", "5.1.8-9.el9"), ("shadow-utils", "2:4.9-9.el9")]
        );
    }

    #[test]
    fn check_update_lines_give_versions_by_name_and_arch() {
        let text = "\nbash.x86_64    5.1.8-10.el9    baseos\n\
            python3-libs.x86_64   3.9.21-1.el9_5   appstream\n\
            Obsoleting Packages\n\
            grub2-tools.x86_64  1:2.06-94.el9  baseos\n";
        let updates = parse_check_update(text);
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[&("bash".into(), "x86_64".into())], "5.1.8-10.el9");
        assert_eq!(
            updates[&("python3-libs".into(), "x86_64".into())],
            "3.9.21-1.el9_5"
        );
    }
}
