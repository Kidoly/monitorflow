//! Flatpak (ADR-0140): the applications and runtimes of the system-wide
//! installation and their updates, read as the agent's own user. Updates
//! go through the root helper ([`super::app_update`]).

use std::collections::HashMap;

use monitorflow_common::proto::v1::{PackageManager, PackageScope, ThirdPartyPackage};
use monitorflow_common::third_party::valid_id;

use super::exec::Tools;
use super::PackageError;

/// `flatpak list` columns. Its `ref` column leaves the kind out, so the
/// agent lists applications and runtimes apart and writes the ref itself.
pub const LIST_COLUMNS: &str = "--columns=application,arch,branch,name,version,active,origin";
/// `flatpak remote-ls` columns; it has no `ref` column either.
pub const UPDATE_COLUMNS: &str = "--columns=application,arch,branch,version,commit";

#[derive(Debug, Clone)]
pub struct Flatpak {
    tools: Tools,
}

/// What `flatpak list --app` or `--runtime` lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    App,
    Runtime,
}

impl Kind {
    fn flag(self) -> &'static str {
        match self {
            Self::App => "--app",
            Self::Runtime => "--runtime",
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Runtime => "runtime",
        }
    }
}

impl Flatpak {
    pub fn new(tools: Tools) -> Self {
        Self { tools }
    }

    /// Whether Flatpak is installed.
    pub fn present(&self) -> bool {
        self.tools.flatpak.exists()
    }

    /// `flatpak --version`: `Flatpak 1.14.10` gives `1.14.10`.
    pub async fn version(&self) -> Option<String> {
        let run = self
            .tools
            .read(&self.tools.flatpak, &["--version"])
            .await
            .ok()?;
        let text = String::from_utf8_lossy(&run.stdout);
        run.success()
            .then(|| text.trim().trim_start_matches("Flatpak").trim().to_owned())
            .filter(|v| !v.is_empty())
    }

    /// The system-wide installation's refs, as the root helper also reads
    /// them to find what is installed.
    pub async fn installed(&self) -> Result<Vec<ThirdPartyPackage>, PackageError> {
        let mut packages = Vec::new();
        for kind in [Kind::App, Kind::Runtime] {
            let run = self
                .tools
                .read(
                    &self.tools.flatpak,
                    &["list", "--system", kind.flag(), LIST_COLUMNS],
                )
                .await?;
            if !run.success() {
                return Err(PackageError::failed(&run));
            }
            packages.extend(parse_list(kind, &String::from_utf8_lossy(&run.stdout)));
        }
        Ok(packages)
    }

    /// [`Self::installed`] with the version each remote offers when it
    /// differs. `remote-ls --updates` fetches each remote's summary, which
    /// Flatpak caches in the agent's home.
    pub async fn inventory(&self) -> Result<Vec<ThirdPartyPackage>, PackageError> {
        let mut packages = self.installed().await?;
        let run = self
            .tools
            .read(
                &self.tools.flatpak,
                &["remote-ls", "--updates", "--system", UPDATE_COLUMNS],
            )
            .await?;
        if !run.success() {
            return Err(PackageError::failed(&run));
        }
        let offers = parse_updates(&String::from_utf8_lossy(&run.stdout));
        for package in &mut packages {
            if let Some(offer) = offers.get(&key(&package.id)) {
                package.available_version = offer.against(&package.version);
            }
        }
        Ok(packages)
    }
}

/// `(id, arch, branch)` of a ref `kind/id/arch/branch`.
fn key(flatpak_ref: &str) -> (String, String, String) {
    let mut parts = flatpak_ref.split('/').skip(1).map(str::to_owned);
    (
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
    )
}

/// `id\tarch\tbranch\tname\tversion\tcommit\torigin` lines, without a
/// header when not on a terminal. The version is the one the application
/// declares, often empty for runtimes: the commit stands for it then.
/// Lines whose ref the grammar refuses are left out.
pub fn parse_list(kind: Kind, text: &str) -> Vec<ThirdPartyPackage> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let (id, arch, branch, name, version, commit, origin) = (
                fields.next()?,
                fields.next()?,
                fields.next()?,
                fields.next()?,
                fields.next()?,
                fields.next()?,
                fields.next()?,
            );
            let flatpak_ref = format!("{}/{id}/{arch}/{branch}", kind.prefix());
            valid_id(PackageManager::Flatpak, &flatpak_ref).then(|| ThirdPartyPackage {
                manager: PackageManager::Flatpak.into(),
                id: flatpak_ref,
                name: name.trim().to_owned(),
                version: version_or_commit(version, commit),
                available_version: String::new(),
                scope: PackageScope::Machine.into(),
                origin: origin.trim().to_owned(),
            })
        })
        .collect()
}

/// What a remote offers for an installed ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    /// Empty when the remote declares none.
    pub version: String,
    pub commit: String,
}

impl Offer {
    /// The available version to show next to `installed`: the offered
    /// version, or its commit when there is none or it is the same, as
    /// for a rebuild.
    pub fn against(&self, installed: &str) -> String {
        if self.version.is_empty() || self.version == installed {
            self.commit.clone()
        } else {
            self.version.clone()
        }
    }
}

/// `id\tarch\tbranch\tversion\tcommit` lines of `remote-ls --updates`:
/// what is offered for `(id, arch, branch)`.
pub fn parse_updates(text: &str) -> HashMap<(String, String, String), Offer> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let (id, arch, branch, version, commit) = (
                fields.next()?,
                fields.next()?,
                fields.next()?,
                fields.next()?,
                fields.next()?,
            );
            let commit = commit.trim();
            (!commit.is_empty()).then(|| {
                (
                    (id.to_owned(), arch.to_owned(), branch.to_owned()),
                    Offer {
                        version: version.trim().to_owned(),
                        commit: commit.to_owned(),
                    },
                )
            })
        })
        .collect()
}

fn version_or_commit(version: &str, commit: &str) -> String {
    let version = version.trim();
    if version.is_empty() {
        commit.trim().to_owned()
    } else {
        version.to_owned()
    }
}

#[cfg(test)]
mod tests;
