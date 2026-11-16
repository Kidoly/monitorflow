//! Third-party managers on Linux (ADR-0140): Flatpak's system-wide
//! installation and snaps, collected with every package report as the
//! agent's own user. A manager that fails is reported unusable and never
//! fails the OS part of the report (ADR-0138).

use monitorflow_common::proto::v1::package_manager_status::State;
use monitorflow_common::proto::v1::{PackageManager, PackageManagerStatus, ThirdPartyPackage};

use super::exec::Tools;
use super::flatpak::Flatpak;
use super::snap::Snap;
use super::{PackageError, ThirdParty};

/// Most apps reported per manager, pending ones first (ADR-0138).
pub const MAX_PER_MANAGER: usize = 5000;

/// Both managers' apps and statuses, Flatpak first.
pub async fn collect(tools: &Tools) -> ThirdParty {
    let mut report = ThirdParty::default();
    let flatpak = Flatpak::new(tools.clone());
    let status = if flatpak.present() {
        let version = flatpak.version().await.unwrap_or_default();
        add(
            &mut report,
            PackageManager::Flatpak,
            version,
            flatpak.inventory().await,
        )
    } else {
        absent(PackageManager::Flatpak)
    };
    report.managers.push(status);

    let snap = Snap::new(tools.clone());
    let status = if snap.present() {
        let system = snap.system().await.unwrap_or_default();
        if !system.timer.is_empty() || !system.hold.is_empty() {
            tracing::debug!(
                timer = %system.timer,
                hold = %system.hold,
                next = %system.next,
                "snapd refreshes snaps on its own schedule"
            );
        }
        add(
            &mut report,
            PackageManager::Snap,
            system.version,
            snap.inventory().await,
        )
    } else {
        absent(PackageManager::Snap)
    };
    report.managers.push(status);
    report
}

fn absent(manager: PackageManager) -> PackageManagerStatus {
    PackageManagerStatus {
        manager: manager.into(),
        state: State::Absent.into(),
        ..PackageManagerStatus::default()
    }
}

/// Adds `inventory`'s apps to `report`; the manager's status.
fn add(
    report: &mut ThirdParty,
    manager: PackageManager,
    version: String,
    inventory: Result<Vec<ThirdPartyPackage>, PackageError>,
) -> PackageManagerStatus {
    let mut status = PackageManagerStatus {
        manager: manager.into(),
        version,
        ..PackageManagerStatus::default()
    };
    match inventory {
        Ok(packages) => {
            report.packages.extend(bounded(packages));
            status.state = State::Available.into();
        }
        Err(error) => {
            tracing::warn!(?manager, %error, "cannot list third-party apps");
            status.state = State::Unusable.into();
            status.error = error.to_string();
        }
    }
    status
}

/// At most [`MAX_PER_MANAGER`] apps, pending ones kept first.
pub fn bounded(mut packages: Vec<ThirdPartyPackage>) -> Vec<ThirdPartyPackage> {
    if packages.len() > MAX_PER_MANAGER {
        packages.sort_by_key(|p| p.available_version.is_empty());
        packages.truncate(MAX_PER_MANAGER);
    }
    packages
}

#[cfg(test)]
mod tests {
    use monitorflow_common::proto::v1::PackageScope;
    use monitorflow_sensors::testing::write_executable;
    use tempfile::TempDir;

    use super::*;

    fn package(id: usize, pending: bool) -> ThirdPartyPackage {
        ThirdPartyPackage {
            manager: PackageManager::Snap.into(),
            id: format!("p{id}"),
            available_version: if pending { "2".into() } else { String::new() },
            scope: PackageScope::Machine.into(),
            ..ThirdPartyPackage::default()
        }
    }

    #[test]
    fn large_inventories_keep_pending_apps_first() {
        let small: Vec<_> = (0..3).map(|i| package(i, false)).collect();
        assert_eq!(bounded(small.clone()), small);
        let many: Vec<_> = (0..MAX_PER_MANAGER + 10)
            .map(|i| package(i, i >= MAX_PER_MANAGER))
            .collect();
        let kept = bounded(many);
        assert_eq!(kept.len(), MAX_PER_MANAGER);
        assert!(kept[..10].iter().all(|p| !p.available_version.is_empty()));
    }

    /// `flatpak` that knows its version and fails `list`; snapd's socket
    /// a plain file, so connecting to it fails.
    fn broken(dir: &TempDir) -> Tools {
        let script = "#!/bin/sh\n[ \"$1\" = --version ] && { echo 'Flatpak 1.12.7'; exit 0; }\n\
                      echo 'error: No such installation' >&2\nexit 1\n";
        write_executable(&dir.path().join("flatpak"), script, 0o755);
        for file in ["snap", "snapd.socket"] {
            std::fs::write(dir.path().join(file), "").unwrap();
        }
        Tools {
            flatpak: dir.path().join("flatpak"),
            snap: dir.path().join("snap"),
            snapd_socket: dir.path().join("snapd.socket"),
            ..Tools::default()
        }
    }

    #[tokio::test]
    async fn managers_that_fail_are_unusable_not_absent() {
        let dir = TempDir::new().unwrap();
        let report = collect(&broken(&dir)).await;
        assert!(report.packages.is_empty());
        let [flatpak, snap] = &report.managers[..] else {
            panic!("{:?}", report.managers)
        };
        assert_eq!(flatpak.manager(), PackageManager::Flatpak);
        assert_eq!(flatpak.state(), State::Unusable);
        assert_eq!(flatpak.version, "1.12.7");
        assert!(
            flatpak.error.contains("No such installation"),
            "{}",
            flatpak.error
        );
        assert_eq!(snap.manager(), PackageManager::Snap);
        assert_eq!(snap.state(), State::Unusable);
        assert!(snap.error.contains("cannot reach snapd"), "{}", snap.error);
    }
}
