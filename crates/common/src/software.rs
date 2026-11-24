//! Software deployment and update approvals (ADR-0188): the Windows
//! Update classifications an update may approve, the check of the
//! approvals and holds of an `ApplyUpdates`, and the check of a
//! `SoftwareChange`, which installs or removes software from the central
//! server's catalogue. The API, the agent and the update service all
//! check with these.

use std::collections::HashSet;

use crate::command::valid_windows_update_name;
use crate::proto::v1::software_change::Action;
use crate::proto::v1::{ApplyUpdates, PackageManager, SoftwareChange};
use crate::third_party::{self, valid_id};

/// The Windows Update classifications an update can approve, by the
/// English names the agent reports (ADR-0049). Drivers are not scanned,
/// so they are not listed.
pub const WINDOWS_CLASSIFICATIONS: [&str; 9] = [
    "Security Updates",
    "Critical Updates",
    "Definition Updates",
    "Update Rollups",
    "Updates",
    "Feature Packs",
    "Service Packs",
    "Tools",
    "Upgrades",
];

/// Most held updates in one `ApplyUpdates`.
pub const MAX_HOLDS: usize = 1024;
/// Most packages one `SoftwareChange` installs or removes.
pub const MAX_PACKAGES: usize = 100;
/// winget's store source, which MonitorFlow never uses (ADR-0138).
pub const STORE_SOURCE: &str = "msstore";

/// The name of `action` in the API and command output.
pub fn action_key(action: Action) -> Option<&'static str> {
    match action {
        Action::Install => Some("install"),
        Action::Remove => Some("remove"),
        Action::Unspecified => None,
    }
}

/// The action named `key`, the reverse of [`action_key`].
pub fn parse_action(key: &str) -> Option<Action> {
    match key {
        "install" => Some(Action::Install),
        "remove" => Some(Action::Remove),
        _ => None,
    }
}

/// Whether `apply` holds anything an agent before ADR-0188 would ignore:
/// approved classifications, held updates or held apps. The server sends
/// those only to agents that know them.
pub fn has_approvals(apply: &ApplyUpdates) -> bool {
    !apply.classifications.is_empty()
        || !apply.hold.is_empty()
        || apply.third_party.iter().any(|s| !s.hold.is_empty())
}

/// Why the approvals of an `ApplyUpdates` are refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApprovalError {
    #[error("unknown Windows Update classification `{0}`")]
    UnknownClassification(String),
    #[error("the classification `{0}` is approved twice")]
    DuplicateClassification(String),
    #[error("classifications narrow all or security updates, not named ones")]
    ClassificationsWithNames,
    #[error("`{0}` is not a KB number or update ID")]
    InvalidHold(String),
    #[error("more than {MAX_HOLDS} held updates")]
    TooManyHolds,
    #[error("`{0}` is both named and held")]
    HeldAndNamed(String),
    #[error("approvals and holds are for Windows updates, which this command skips")]
    WithoutOs,
}

/// Checks the classifications and held updates of `apply` (the held
/// apps are checked by [`third_party::check`]).
pub fn check_approvals(apply: &ApplyUpdates) -> Result<(), ApprovalError> {
    if apply.skip_os && (!apply.classifications.is_empty() || !apply.hold.is_empty()) {
        return Err(ApprovalError::WithoutOs);
    }
    let mut seen = HashSet::new();
    for name in &apply.classifications {
        if !WINDOWS_CLASSIFICATIONS.contains(&name.as_str()) {
            return Err(ApprovalError::UnknownClassification(
                name.escape_debug().to_string(),
            ));
        }
        if !seen.insert(name.as_str()) {
            return Err(ApprovalError::DuplicateClassification(name.clone()));
        }
    }
    if !apply.classifications.is_empty() && !apply.packages.is_empty() && !apply.security_only {
        return Err(ApprovalError::ClassificationsWithNames);
    }
    if apply.hold.len() > MAX_HOLDS {
        return Err(ApprovalError::TooManyHolds);
    }
    if let Some(bad) = apply.hold.iter().find(|n| !valid_windows_update_name(n)) {
        return Err(ApprovalError::InvalidHold(bad.escape_debug().to_string()));
    }
    if let Some(both) = apply
        .hold
        .iter()
        .find(|h| apply.packages.iter().any(|p| p.eq_ignore_ascii_case(h)))
    {
        return Err(ApprovalError::HeldAndNamed(both.clone()));
    }
    Ok(())
}

/// Whether `name` is a winget source MonitorFlow may install from: 1 to
/// 64 of ASCII letters, digits and `._-`, starting with a letter or
/// digit, and not the store's.
pub fn valid_source(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        && !name.eq_ignore_ascii_case(STORE_SOURCE)
}

/// Why a `SoftwareChange` is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SoftwareError {
    #[error("the action must be install or remove")]
    UnknownAction,
    #[error("software is installed with winget only for now")]
    UnsupportedManager,
    #[error("software deployment is not available on {0} servers yet")]
    WrongPlatform(String),
    #[error("no software to {0}")]
    Empty(&'static str),
    #[error("at most {MAX_PACKAGES} packages at once")]
    TooMany,
    #[error("invalid winget package ID: {0}")]
    InvalidId(String),
    #[error("`{0}` is listed twice")]
    Duplicate(String),
    #[error("invalid winget source for `{0}`")]
    InvalidSource(String),
}

/// Checks `change`: a known action, winget, valid and distinct IDs, a
/// valid source for every install. With `os_family`, it must be a
/// platform that runs it: Windows.
pub fn check_change(change: &SoftwareChange, os_family: Option<&str>) -> Result<(), SoftwareError> {
    let action = Action::try_from(change.action)
        .ok()
        .and_then(action_key)
        .ok_or(SoftwareError::UnknownAction)?;
    if change.manager != i32::from(PackageManager::Winget) {
        return Err(SoftwareError::UnsupportedManager);
    }
    if let Some(family) = os_family {
        if !third_party::managers_for(family).contains(&PackageManager::Winget) {
            return Err(SoftwareError::WrongPlatform(family.to_owned()));
        }
    }
    if change.packages.is_empty() {
        return Err(SoftwareError::Empty(action));
    }
    if change.packages.len() > MAX_PACKAGES {
        return Err(SoftwareError::TooMany);
    }
    let mut seen = HashSet::new();
    for package in &change.packages {
        if !valid_id(PackageManager::Winget, &package.id) {
            return Err(SoftwareError::InvalidId(
                package.id.escape_debug().to_string(),
            ));
        }
        if !seen.insert(package.id.to_ascii_lowercase()) {
            return Err(SoftwareError::Duplicate(package.id.clone()));
        }
        let install = change.action == i32::from(Action::Install);
        if (install || !package.source.is_empty()) && !valid_source(&package.source) {
            return Err(SoftwareError::InvalidSource(package.id.clone()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::v1::{SoftwarePackage, ThirdPartySelection};

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    #[test]
    fn actions_round_trip() {
        for action in [Action::Install, Action::Remove] {
            assert_eq!(parse_action(action_key(action).unwrap()), Some(action));
        }
        assert_eq!(action_key(Action::Unspecified), None);
        assert_eq!(parse_action("upgrade"), None);
    }

    #[test]
    fn approvals_are_detected() {
        assert!(!has_approvals(&ApplyUpdates::default()));
        for apply in [
            ApplyUpdates {
                classifications: strings(&["Updates"]),
                ..ApplyUpdates::default()
            },
            ApplyUpdates {
                hold: strings(&["KB5034441"]),
                ..ApplyUpdates::default()
            },
            ApplyUpdates {
                third_party: vec![ThirdPartySelection {
                    manager: PackageManager::Winget.into(),
                    hold: strings(&["Mozilla.Firefox"]),
                    ..ThirdPartySelection::default()
                }],
                ..ApplyUpdates::default()
            },
        ] {
            assert!(has_approvals(&apply), "{apply:?}");
        }
    }

    #[test]
    fn approvals_are_checked() {
        let apply = |classifications: &[&str], hold: &[&str], packages: &[&str]| ApplyUpdates {
            classifications: strings(classifications),
            hold: strings(hold),
            packages: strings(packages),
            ..ApplyUpdates::default()
        };
        assert_eq!(
            check_approvals(&apply(
                &["Security Updates", "Updates"],
                &["KB1", "kb22"],
                &[]
            )),
            Ok(())
        );
        assert_eq!(
            check_approvals(&apply(&["Drivers"], &[], &[])),
            Err(ApprovalError::UnknownClassification("Drivers".into()))
        );
        assert_eq!(
            check_approvals(&apply(&["Updates", "Updates"], &[], &[])),
            Err(ApprovalError::DuplicateClassification("Updates".into()))
        );
        assert_eq!(
            check_approvals(&apply(&["Updates"], &[], &["KB1"])),
            Err(ApprovalError::ClassificationsWithNames)
        );
        let security = ApplyUpdates {
            security_only: true,
            ..apply(&["Security Updates"], &[], &["KB1"])
        };
        assert_eq!(check_approvals(&security), Ok(()));
        assert_eq!(
            check_approvals(&apply(&[], &["libc6"], &[])),
            Err(ApprovalError::InvalidHold("libc6".into()))
        );
        assert_eq!(
            check_approvals(&apply(&[], &["kb1"], &["KB1"])),
            Err(ApprovalError::HeldAndNamed("kb1".into()))
        );
        let skip = ApplyUpdates {
            skip_os: true,
            ..apply(&[], &["KB1"], &[])
        };
        assert_eq!(check_approvals(&skip), Err(ApprovalError::WithoutOs));
        let many: Vec<String> = (0..=MAX_HOLDS).map(|i| format!("KB{i}")).collect();
        let many = ApplyUpdates {
            hold: many,
            ..ApplyUpdates::default()
        };
        assert_eq!(check_approvals(&many), Err(ApprovalError::TooManyHolds));
    }

    #[test]
    fn sources() {
        for good in ["winget", "contoso-internal", "Repo_2.local"] {
            assert!(valid_source(good), "{good}");
        }
        for bad in [
            "",
            "msstore",
            "MSStore",
            "-x",
            "a b",
            "a/b",
            &"a".repeat(65),
        ] {
            assert!(!valid_source(bad), "{bad}");
        }
    }

    fn change(action: Action, packages: &[(&str, &str)]) -> SoftwareChange {
        SoftwareChange {
            action: action.into(),
            manager: PackageManager::Winget.into(),
            packages: packages
                .iter()
                .map(|(id, source)| SoftwarePackage {
                    id: id.to_string(),
                    source: source.to_string(),
                })
                .collect(),
            allow_reboot: false,
        }
    }

    #[test]
    fn changes_are_checked() {
        let install = change(Action::Install, &[("7zip.7zip", "winget")]);
        assert_eq!(check_change(&install, Some("windows")), Ok(()));
        assert_eq!(
            check_change(&install, Some("debian")),
            Err(SoftwareError::WrongPlatform("debian".into()))
        );
        assert_eq!(
            check_change(&change(Action::Remove, &[("7zip.7zip", "")]), None),
            Ok(())
        );
        assert_eq!(
            check_change(&change(Action::Install, &[("7zip.7zip", "")]), None),
            Err(SoftwareError::InvalidSource("7zip.7zip".into()))
        );
        assert_eq!(
            check_change(&change(Action::Install, &[("7zip.7zip", "msstore")]), None),
            Err(SoftwareError::InvalidSource("7zip.7zip".into()))
        );
        assert_eq!(
            check_change(
                &change(Action::Unspecified, &[("7zip.7zip", "winget")]),
                None
            ),
            Err(SoftwareError::UnknownAction)
        );
        assert_eq!(
            check_change(&change(Action::Install, &[]), None),
            Err(SoftwareError::Empty("install"))
        );
        assert_eq!(
            check_change(
                &change(
                    Action::Install,
                    &[("7zip.7zip", "winget"), ("7ZIP.7zip", "winget")]
                ),
                None
            ),
            Err(SoftwareError::Duplicate("7ZIP.7zip".into()))
        );
        assert_eq!(
            check_change(&change(Action::Install, &[("--force", "winget")]), None),
            Err(SoftwareError::InvalidId("--force".into()))
        );
        let flatpak = SoftwareChange {
            manager: PackageManager::Flatpak.into(),
            ..install.clone()
        };
        assert_eq!(
            check_change(&flatpak, None),
            Err(SoftwareError::UnsupportedManager)
        );
        let ids: Vec<(String, &str)> = (0..=MAX_PACKAGES)
            .map(|i| (format!("Vendor.App{i}"), "winget"))
            .collect();
        let ids: Vec<(&str, &str)> = ids.iter().map(|(i, s)| (i.as_str(), *s)).collect();
        assert_eq!(
            check_change(&change(Action::Install, &ids), None),
            Err(SoftwareError::TooMany)
        );
    }
}
