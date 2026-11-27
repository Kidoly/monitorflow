//! Third-party package managers (ADR-0138): their names, the platforms
//! they belong to, the grammar of their package IDs and the check of the
//! third-party part of an `ApplyUpdates`. The API, the agent and the
//! privileged helpers all check with these.

use std::collections::HashSet;

use crate::proto::v1::{ApplyUpdates, PackageManager};

/// Most package IDs one `ApplyUpdates` may name, all managers together.
pub const MAX_IDS: usize = 1024;

/// The name of `manager` in the API, the database and command output.
pub fn key(manager: PackageManager) -> Option<&'static str> {
    match manager {
        PackageManager::Winget => Some("winget"),
        PackageManager::Flatpak => Some("flatpak"),
        PackageManager::Snap => Some("snap"),
        PackageManager::Apt => Some("apt"),
        PackageManager::Dnf => Some("dnf"),
        PackageManager::Unspecified => None,
    }
}

/// Whether `manager` is a third-party one, which updates select. apt and
/// dnf are the distributions' own: only software from the catalogue
/// names them (ADR-0188).
pub fn is_third_party(manager: PackageManager) -> bool {
    matches!(
        manager,
        PackageManager::Winget | PackageManager::Flatpak | PackageManager::Snap
    )
}

/// The manager named `key`, the reverse of [`key`].
pub fn parse(key: &str) -> Option<PackageManager> {
    match key {
        "winget" => Some(PackageManager::Winget),
        "flatpak" => Some(PackageManager::Flatpak),
        "snap" => Some(PackageManager::Snap),
        "apt" => Some(PackageManager::Apt),
        "dnf" => Some(PackageManager::Dnf),
        _ => None,
    }
}

/// The managers of a host of `os_family` (as `Hello.os_family` says it):
/// winget on Windows, Flatpak and Snap on every Linux family.
pub fn managers_for(os_family: &str) -> &'static [PackageManager] {
    if os_family == "windows" {
        &[PackageManager::Winget]
    } else {
        &[PackageManager::Flatpak, PackageManager::Snap]
    }
}

/// Whether `id` is a package ID `manager` may receive on a command line.
/// None of the grammars lets an ID start with `-`, hold a space, a path
/// separator other than a Flatpak ref's own `/`, or a shell character.
pub fn valid_id(manager: PackageManager, id: &str) -> bool {
    match manager {
        PackageManager::Winget => valid_winget_id(id),
        PackageManager::Flatpak => valid_flatpak_ref(id),
        PackageManager::Snap => valid_snap_name(id),
        PackageManager::Apt | PackageManager::Dnf => crate::command::valid_package_name(id),
        PackageManager::Unspecified => false,
    }
}

/// A winget package identifier: 2 to 8 segments joined by `.`, each 1 to
/// 32 of ASCII letters, digits and `_+-`, starting with a letter or digit;
/// at most 128 bytes.
fn valid_winget_id(id: &str) -> bool {
    let segments: Vec<&str> = id.split('.').collect();
    id.len() <= 128
        && (2..=8).contains(&segments.len())
        && segments.iter().all(|s| {
            (1..=32).contains(&s.len())
                && s.as_bytes()[0].is_ascii_alphanumeric()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_+-".contains(&b))
        })
}

/// A Flatpak ref: `app/` or `runtime/`, an ID, an arch and a branch.
fn valid_flatpak_ref(reference: &str) -> bool {
    let mut parts = reference.split('/');
    let (Some(kind), Some(id), Some(arch), Some(branch), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return false;
    };
    matches!(kind, "app" | "runtime")
        && valid_flatpak_id(id)
        && (1..=32).contains(&arch.len())
        && arch
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        && (1..=64).contains(&branch.len())
        && branch.as_bytes()[0].is_ascii_alphanumeric()
        && branch
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// Flatpak's application ID rules: 3 to 255 bytes, at least three
/// elements, each a letter or `_` and then letters, digits and `_`, with
/// `-` also allowed in the last one.
fn valid_flatpak_id(id: &str) -> bool {
    let elements: Vec<&str> = id.split('.').collect();
    let last = elements.len().saturating_sub(1);
    (3..=255).contains(&id.len())
        && elements.len() >= 3
        && elements.iter().enumerate().all(|(i, e)| {
            e.as_bytes()
                .first()
                .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
                && e.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || (i == last && b == b'-'))
        })
}

/// A snap name, 1 to 40 of lowercase letters, digits and `-` with at least
/// one letter, no `-` at either end nor `--`; then optionally `_` and an
/// instance key of 1 to 10 lowercase letters or digits.
fn valid_snap_name(id: &str) -> bool {
    let (name, key) = match id.split_once('_') {
        Some((name, key)) => (name, Some(key)),
        None => (id, None),
    };
    let lower_or_digit = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    (1..=40).contains(&name.len())
        && name.bytes().all(|b| lower_or_digit(b) || b == b'-')
        && name.bytes().any(|b| b.is_ascii_lowercase())
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && key.is_none_or(|k| (1..=10).contains(&k.len()) && k.bytes().all(lower_or_digit))
}

/// Why the third-party part of an `ApplyUpdates` is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SelectionError {
    #[error("nothing to update: the OS is skipped and no third-party manager is selected")]
    Empty,
    #[error("package names are for OS updates, which this command skips")]
    OsNamesWithoutOs,
    #[error("unknown package manager")]
    UnknownManager,
    #[error("{0} is selected twice")]
    Duplicate(&'static str),
    #[error("{0} is not available on {1} servers")]
    WrongPlatform(&'static str, String),
    #[error("invalid {0} package ID: {1}")]
    InvalidId(&'static str, String),
    #[error("more than {MAX_IDS} third-party package IDs")]
    TooMany,
    #[error("{0} `{1}` is both selected and held")]
    HeldAndNamed(&'static str, String),
}

/// Checks the third-party part of `apply`: known managers, each once,
/// valid IDs, something to do. With `os_family`, the managers must belong
/// to that platform.
pub fn check(apply: &ApplyUpdates, os_family: Option<&str>) -> Result<(), SelectionError> {
    if apply.skip_os && apply.third_party.is_empty() {
        return Err(SelectionError::Empty);
    }
    if apply.skip_os && !apply.packages.is_empty() {
        return Err(SelectionError::OsNamesWithoutOs);
    }
    let mut seen = HashSet::new();
    let mut ids = 0;
    for selection in &apply.third_party {
        let manager = PackageManager::try_from(selection.manager)
            .map_err(|_| SelectionError::UnknownManager)?;
        let name = key(manager)
            .filter(|_| is_third_party(manager))
            .ok_or(SelectionError::UnknownManager)?;
        if !seen.insert(manager) {
            return Err(SelectionError::Duplicate(name));
        }
        if let Some(family) = os_family {
            if !managers_for(family).contains(&manager) {
                return Err(SelectionError::WrongPlatform(name, family.to_owned()));
            }
        }
        ids += selection.packages.len() + selection.hold.len();
        if ids > MAX_IDS {
            return Err(SelectionError::TooMany);
        }
        let all = selection.packages.iter().chain(&selection.hold);
        if let Some(bad) = all.clone().find(|id| !valid_id(manager, id)) {
            return Err(SelectionError::InvalidId(name, bad.clone()));
        }
        // IDs compare without case, as winget does (ADR-0188).
        if let Some(both) = selection
            .hold
            .iter()
            .find(|h| selection.packages.iter().any(|p| p.eq_ignore_ascii_case(h)))
        {
            return Err(SelectionError::HeldAndNamed(name, both.clone()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::v1::ThirdPartySelection;

    #[test]
    fn keys_round_trip() {
        for manager in [
            PackageManager::Winget,
            PackageManager::Flatpak,
            PackageManager::Snap,
        ] {
            assert_eq!(parse(key(manager).unwrap()), Some(manager));
        }
        assert_eq!(key(PackageManager::Unspecified), None);
        assert_eq!(parse("os"), None);
        assert_eq!(parse("Winget"), None);
    }

    #[test]
    fn platforms() {
        assert_eq!(managers_for("windows"), &[PackageManager::Winget]);
        for family in ["debian", "rhel", "linux"] {
            assert_eq!(
                managers_for(family),
                &[PackageManager::Flatpak, PackageManager::Snap]
            );
        }
    }

    #[test]
    fn winget_ids() {
        for good in [
            "7zip.7zip",
            "Notepad++.Notepad++",
            "Python.Python.3.12",
            "Mozilla.Firefox.ESR",
            "OpenJS.NodeJS.LTS",
            "Kubernetes.kubectl",
            "a.b.c.d.e.f.g.h",
        ] {
            assert!(valid_id(PackageManager::Winget, good), "{good}");
        }
        for bad in [
            "",
            "7zip",
            "-x.y",
            "x.-y",
            "x..y",
            ".x.y",
            "x.y.",
            "x y.z",
            "x/y.z",
            "x\\y.z",
            "x.y;z",
            "x.\"y\"",
            "a.b.c.d.e.f.g.h.i",
            "9NBLGGH4NNS1",
            "Äpfel.App",
        ] {
            assert!(!valid_id(PackageManager::Winget, bad), "{bad}");
        }
        assert!(!valid_id(
            PackageManager::Winget,
            &format!("a.{}", "b".repeat(33))
        ));
    }

    #[test]
    fn flatpak_refs() {
        for good in [
            "app/org.mozilla.firefox/x86_64/stable",
            "runtime/org.freedesktop.Platform/x86_64/23.08",
            "runtime/org.freedesktop.Platform.GL.default/aarch64/23.08-extra",
            "app/com.example.My_App-beta/x86_64/master",
            "app/_org.example.app/x86_64/stable",
        ] {
            assert!(valid_id(PackageManager::Flatpak, good), "{good}");
        }
        for bad in [
            "",
            "org.mozilla.firefox",
            "app/org.mozilla.firefox",
            "app/org.mozilla.firefox/x86_64",
            "app/org.mozilla.firefox/x86_64/stable/extra",
            "extension/org.mozilla.firefox/x86_64/stable",
            "app/org.firefox/x86_64/stable",
            "app/org.mo-zilla.firefox/x86_64/stable",
            "app/org.1mozilla.firefox/x86_64/stable",
            "app/org..firefox/x86_64/stable",
            "app/org.mozilla.firefox/X86_64/stable",
            "app/org.mozilla.firefox/x86_64/-stable",
            "app/org.mozilla.firefox/x86_64/sta ble",
            "app/org.mozilla.firefox//stable",
            "app/../x86_64/stable",
        ] {
            assert!(!valid_id(PackageManager::Flatpak, bad), "{bad}");
        }
    }

    #[test]
    fn snap_names() {
        for good in ["firefox", "core22", "gnome-42-2204", "lxd_test", "a", "x_1"] {
            assert!(valid_id(PackageManager::Snap, good), "{good}");
        }
        for bad in [
            "",
            "123",
            "-firefox",
            "firefox-",
            "fire--fox",
            "Firefox",
            "fire fox",
            "firefox_",
            "firefox_TEST",
            "firefox_12345678901",
            "firefox_a_b",
            "fire.fox",
            &"a".repeat(41),
        ] {
            assert!(!valid_id(PackageManager::Snap, bad), "{bad}");
        }
    }

    #[test]
    fn unspecified_takes_nothing() {
        assert!(!valid_id(PackageManager::Unspecified, "firefox"));
    }

    fn select(manager: PackageManager, packages: &[&str]) -> ThirdPartySelection {
        ThirdPartySelection {
            manager: manager.into(),
            packages: packages.iter().map(|p| p.to_string()).collect(),
            ..ThirdPartySelection::default()
        }
    }

    #[test]
    fn selections() {
        let ok = |apply: &ApplyUpdates, family| check(apply, family);
        // An older command: OS only.
        assert_eq!(ok(&ApplyUpdates::default(), Some("debian")), Ok(()));
        let both = ApplyUpdates {
            security_only: true,
            third_party: vec![
                select(PackageManager::Flatpak, &[]),
                select(PackageManager::Snap, &["firefox"]),
            ],
            ..ApplyUpdates::default()
        };
        assert_eq!(ok(&both, Some("debian")), Ok(()));
        assert_eq!(ok(&both, None), Ok(()));
        assert_eq!(
            ok(&both, Some("windows")),
            Err(SelectionError::WrongPlatform("flatpak", "windows".into()))
        );
        assert_eq!(
            ok(
                &ApplyUpdates {
                    skip_os: true,
                    ..ApplyUpdates::default()
                },
                None
            ),
            Err(SelectionError::Empty)
        );
        assert_eq!(
            ok(
                &ApplyUpdates {
                    skip_os: true,
                    packages: vec!["openssl".into()],
                    third_party: vec![select(PackageManager::Snap, &[])],
                    ..ApplyUpdates::default()
                },
                None
            ),
            Err(SelectionError::OsNamesWithoutOs)
        );
        let twice = ApplyUpdates {
            third_party: vec![
                select(PackageManager::Snap, &[]),
                select(PackageManager::Snap, &["firefox"]),
            ],
            ..ApplyUpdates::default()
        };
        assert_eq!(ok(&twice, None), Err(SelectionError::Duplicate("snap")));
        for manager in [0, 99] {
            let unknown = ApplyUpdates {
                third_party: vec![ThirdPartySelection {
                    manager,
                    ..ThirdPartySelection::default()
                }],
                ..ApplyUpdates::default()
            };
            assert_eq!(ok(&unknown, None), Err(SelectionError::UnknownManager));
        }
        let bad = ApplyUpdates {
            skip_os: true,
            third_party: vec![select(PackageManager::Winget, &["7zip.7zip", "--force"])],
            ..ApplyUpdates::default()
        };
        assert_eq!(
            ok(&bad, Some("windows")),
            Err(SelectionError::InvalidId("winget", "--force".into()))
        );
        let ids: Vec<String> = (0..=MAX_IDS).map(|i| format!("snap{i}")).collect();
        let many = ApplyUpdates {
            third_party: vec![ThirdPartySelection {
                manager: PackageManager::Snap.into(),
                packages: ids,
                ..ThirdPartySelection::default()
            }],
            ..ApplyUpdates::default()
        };
        assert_eq!(ok(&many, None), Err(SelectionError::TooMany));
    }

    #[test]
    fn holds_are_checked() {
        let with_hold = |packages: &[&str], hold: &[&str]| ApplyUpdates {
            third_party: vec![ThirdPartySelection {
                manager: PackageManager::Winget.into(),
                packages: packages.iter().map(|p| p.to_string()).collect(),
                hold: hold.iter().map(|p| p.to_string()).collect(),
            }],
            ..ApplyUpdates::default()
        };
        assert_eq!(check(&with_hold(&[], &["Mozilla.Firefox"]), None), Ok(()));
        assert_eq!(
            check(&with_hold(&[], &["-x"]), None),
            Err(SelectionError::InvalidId("winget", "-x".into()))
        );
        assert_eq!(
            check(&with_hold(&["Mozilla.Firefox"], &["mozilla.firefox"]), None),
            Err(SelectionError::HeldAndNamed(
                "winget",
                "mozilla.firefox".into()
            ))
        );
    }
}
