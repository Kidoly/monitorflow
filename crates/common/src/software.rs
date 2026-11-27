//! Software deployment and update approvals (ADR-0188): the Windows
//! Update classifications an update may approve, the check of the
//! approvals and holds of an `ApplyUpdates`, and the check of a
//! `SoftwareChange`, which installs or removes software from the central
//! server's catalogue. The API, the agent and the update service all
//! check with these.

use std::collections::HashSet;

use crate::command::valid_windows_update_name;
use crate::proto::v1::software_change::Action;
use crate::proto::v1::{ApplyUpdates, MsiFile, PackageManager, SoftwareChange, SoftwarePackage};
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

/// Largest MSI file: Windows Installer's own limit, 2 GiB.
pub const MAX_MSI_BYTES: u64 = 2 << 30;
/// Most URLs one MSI file is downloaded from.
pub const MAX_MSI_URLS: usize = 4;
/// Longest URL of an MSI file.
pub const MAX_MSI_URL: usize = 2048;
/// Most public properties of one MSI install.
pub const MAX_MSI_PROPERTIES: usize = 32;
/// Longest value of an MSI property.
pub const MAX_MSI_VALUE: usize = 256;
/// Longest name of an MSI package, for the output.
pub const MAX_MSI_NAME: usize = 128;
/// Properties never set from the central server: they change what
/// `msiexec` does (remove features, apply transforms or patches, restart)
/// rather than configure the product.
pub const RESERVED_MSI_PROPERTIES: [&str; 10] = [
    "ACTION",
    "ADVERTISE",
    "MSIPATCHREMOVE",
    "PATCH",
    "REBOOT",
    "REBOOTPROMPT",
    "REINSTALL",
    "REINSTALLMODE",
    "REMOVE",
    "TRANSFORMS",
];

/// Whether `code` is a ProductCode as Windows Installer writes it: a GUID
/// in upper case between braces.
pub fn valid_product_code(code: &str) -> bool {
    let bytes = code.as_bytes();
    bytes.len() == 38
        && bytes[0] == b'{'
        && bytes[37] == b'}'
        && code[1..37].split('-').map(str::len).eq([8, 4, 4, 4, 12])
        && code[1..37]
            .bytes()
            .all(|b| b == b'-' || b.is_ascii_digit() || (b'A'..=b'F').contains(&b))
}

/// Whether `hex` is a SHA-256 digest in lower-case hex.
pub fn valid_sha256(hex: &str) -> bool {
    hex.len() == 64
        && hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Whether `name` may be passed to `msiexec` as a public property: an
/// upper-case letter, then up to 63 of upper-case letters, digits and
/// `_`, and not one of [`RESERVED_MSI_PROPERTIES`].
pub fn valid_msi_property(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name.as_bytes()[0].is_ascii_uppercase()
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && !RESERVED_MSI_PROPERTIES.contains(&name)
}

/// Whether `value` may be a property's value: up to [`MAX_MSI_VALUE`]
/// printable ASCII characters but `"` and `;`. `msiexec` takes it
/// between double quotes, and `;` separates properties in the web
/// interface.
pub fn valid_msi_value(value: &str) -> bool {
    value.len() <= MAX_MSI_VALUE
        && value
            .bytes()
            .all(|b| (b' '..=b'~').contains(&b) && b != b'"' && b != b';')
}

/// Whether `url` is where an MSI file may be downloaded from: http or
/// https, without spaces, quotes or control characters.
pub fn valid_msi_url(url: &str) -> bool {
    url.len() <= MAX_MSI_URL
        && (url.starts_with("http://") || url.starts_with("https://"))
        && url
            .bytes()
            .all(|b| b.is_ascii_graphic() && !b"\"'<>\\`".contains(&b))
}

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

/// The manager that installs software from the catalogue on a host of
/// `os_family`, as `Hello.os_family` says it: winget on Windows, apt on
/// the Debian family, dnf on the RHEL family. None elsewhere.
pub fn manager_for(os_family: &str) -> Option<PackageManager> {
    match os_family {
        "windows" => Some(PackageManager::Winget),
        "debian" => Some(PackageManager::Apt),
        "rhel" => Some(PackageManager::Dnf),
        _ => None,
    }
}

/// Whether `manager` installs software from the catalogue.
pub fn is_software_manager(manager: PackageManager) -> bool {
    matches!(
        manager,
        PackageManager::Winget | PackageManager::Apt | PackageManager::Dnf
    )
}

/// Why a `SoftwareChange` is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SoftwareError {
    #[error("the action must be install or remove")]
    UnknownAction,
    #[error("software is installed with winget, apt or dnf")]
    UnsupportedManager,
    #[error("software deployment is not available on {0} servers yet")]
    WrongPlatform(String),
    #[error("{0} packages are not for {1} servers")]
    WrongManager(&'static str, String),
    #[error("no software to {0}")]
    Empty(&'static str),
    #[error("at most {MAX_PACKAGES} packages at once")]
    TooMany,
    #[error("invalid {0} package: {1}")]
    InvalidId(&'static str, String),
    #[error("`{0}` is listed twice")]
    Duplicate(String),
    #[error("invalid winget source for `{0}`")]
    InvalidSource(String),
    #[error("`{0}` takes no source: apt and dnf install from the server's own sources")]
    UnexpectedSource(String),
    #[error("MSI packages are for Windows servers")]
    MsiOffWindows,
    #[error("invalid MSI package `{0}`: {1}")]
    InvalidMsi(String, &'static str),
}

/// Checks `change`: a known action, winget, apt or dnf, valid and
/// distinct IDs in its grammar, a valid source for every winget install
/// and none for apt and dnf. With `os_family`, it must be the manager of
/// that platform ([`manager_for`]).
pub fn check_change(change: &SoftwareChange, os_family: Option<&str>) -> Result<(), SoftwareError> {
    let action = Action::try_from(change.action)
        .ok()
        .and_then(action_key)
        .ok_or(SoftwareError::UnknownAction)?;
    let manager = PackageManager::try_from(change.manager)
        .ok()
        .filter(|m| is_software_manager(*m))
        .ok_or(SoftwareError::UnsupportedManager)?;
    let name = third_party::key(manager).ok_or(SoftwareError::UnsupportedManager)?;
    if let Some(family) = os_family {
        match manager_for(family) {
            None => return Err(SoftwareError::WrongPlatform(family.to_owned())),
            Some(own) if own != manager => {
                return Err(SoftwareError::WrongManager(name, family.to_owned()))
            }
            Some(_) => {}
        }
    }
    if change.packages.is_empty() {
        return Err(SoftwareError::Empty(action));
    }
    if change.packages.len() > MAX_PACKAGES {
        return Err(SoftwareError::TooMany);
    }
    let winget = manager == PackageManager::Winget;
    let install = change.action == i32::from(Action::Install);
    let mut seen = HashSet::new();
    for package in &change.packages {
        if let Some(msi) = &package.msi {
            if !winget {
                return Err(SoftwareError::MsiOffWindows);
            }
            check_msi(package, msi, install)?;
            if !seen.insert(package.id.to_ascii_lowercase()) {
                return Err(SoftwareError::Duplicate(package.id.clone()));
            }
            continue;
        }
        if !valid_id(manager, &package.id) {
            return Err(SoftwareError::InvalidId(
                name,
                package.id.escape_debug().to_string(),
            ));
        }
        // winget IDs compare without case; package names of the
        // distributions are case-sensitive.
        let key = if winget {
            package.id.to_ascii_lowercase()
        } else {
            package.id.clone()
        };
        if !seen.insert(key) {
            return Err(SoftwareError::Duplicate(package.id.clone()));
        }
        if !winget {
            if !package.source.is_empty() {
                return Err(SoftwareError::UnexpectedSource(package.id.clone()));
            }
            continue;
        }
        if (install || !package.source.is_empty()) && !valid_source(&package.source) {
            return Err(SoftwareError::InvalidSource(package.id.clone()));
        }
    }
    Ok(())
}

/// Checks the MSI package `package`: a ProductCode, no source, and for an
/// install the file's digest, size, URLs and properties; a removal names
/// the ProductCode only.
fn check_msi(package: &SoftwarePackage, msi: &MsiFile, install: bool) -> Result<(), SoftwareError> {
    let bad = |why| {
        Err(SoftwareError::InvalidMsi(
            package.id.escape_debug().to_string(),
            why,
        ))
    };
    if !valid_product_code(&package.id) {
        return bad("the ID must be its ProductCode, an upper-case GUID between braces");
    }
    if !package.source.is_empty() {
        return bad("an MSI package takes no source");
    }
    if msi.name.chars().count() > MAX_MSI_NAME || msi.name.chars().any(char::is_control) {
        return bad("the name is at most 128 characters");
    }
    if !install {
        if !msi.sha256.is_empty()
            || msi.size != 0
            || !msi.urls.is_empty()
            || !msi.properties.is_empty()
        {
            return bad("a removal takes only the ProductCode");
        }
        return Ok(());
    }
    if !valid_sha256(&msi.sha256) {
        return bad("the SHA-256 must be 64 lower-case hex digits");
    }
    if msi.size == 0 || msi.size > MAX_MSI_BYTES {
        return bad("the file is empty or larger than 2 GiB");
    }
    if msi.urls.is_empty()
        || msi.urls.len() > MAX_MSI_URLS
        || !msi.urls.iter().all(|u| valid_msi_url(u))
    {
        return bad("it needs 1 to 4 http or https URLs");
    }
    if msi.properties.len() > MAX_MSI_PROPERTIES {
        return bad("at most 32 properties");
    }
    let mut names = HashSet::new();
    for property in &msi.properties {
        if !valid_msi_property(&property.name) {
            return bad("a property name is not a public property, or is reserved");
        }
        if !valid_msi_value(&property.value) {
            return bad("a property value has a quote, a semicolon or a control character, or is over 256 characters");
        }
        if !names.insert(property.name.as_str()) {
            return bad("a property is set twice");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::v1::{MsiProperty, ThirdPartySelection};

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
                    msi: None,
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
            Err(SoftwareError::WrongManager("winget", "debian".into()))
        );
        assert_eq!(
            check_change(&install, Some("linux")),
            Err(SoftwareError::WrongPlatform("linux".into()))
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
            Err(SoftwareError::InvalidId("winget", "--force".into()))
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

    fn linux(manager: PackageManager, action: Action, names: &[&str]) -> SoftwareChange {
        SoftwareChange {
            manager: manager.into(),
            ..change(action, &names.iter().map(|n| (*n, "")).collect::<Vec<_>>())
        }
    }

    #[test]
    fn linux_changes_name_packages_of_the_distribution() {
        let apt = linux(
            PackageManager::Apt,
            Action::Install,
            &["nginx", "libstdc++6:amd64"],
        );
        assert_eq!(check_change(&apt, Some("debian")), Ok(()));
        assert_eq!(
            check_change(&apt, Some("rhel")),
            Err(SoftwareError::WrongManager("apt", "rhel".into()))
        );
        assert_eq!(
            check_change(&apt, Some("windows")),
            Err(SoftwareError::WrongManager("apt", "windows".into()))
        );
        let dnf = linux(PackageManager::Dnf, Action::Remove, &["httpd"]);
        assert_eq!(check_change(&dnf, Some("rhel")), Ok(()));
        // Never an option, a path, a glob, a group or a removal marker.
        for bad in ["-y", "/tmp/x.deb", "ngin*", "@core", "nginx-", "a b", ""] {
            assert_eq!(
                check_change(&linux(PackageManager::Apt, Action::Install, &[bad]), None),
                Err(SoftwareError::InvalidId(
                    "apt",
                    bad.escape_debug().to_string()
                )),
                "{bad}"
            );
        }
        let mut sourced = apt.clone();
        sourced.packages[0].source = "winget".into();
        assert_eq!(
            check_change(&sourced, None),
            Err(SoftwareError::UnexpectedSource("nginx".into()))
        );
        // Case matters for the distributions' names.
        let cased = linux(PackageManager::Dnf, Action::Install, &["Foo", "foo"]);
        assert_eq!(check_change(&cased, None), Ok(()));
        let twice = linux(PackageManager::Dnf, Action::Install, &["foo", "foo"]);
        assert_eq!(
            check_change(&twice, None),
            Err(SoftwareError::Duplicate("foo".into()))
        );
    }

    #[test]
    fn each_platform_has_its_manager() {
        assert_eq!(manager_for("windows"), Some(PackageManager::Winget));
        assert_eq!(manager_for("debian"), Some(PackageManager::Apt));
        assert_eq!(manager_for("rhel"), Some(PackageManager::Dnf));
        assert_eq!(manager_for("linux"), None);
        assert!(!is_software_manager(PackageManager::Snap));
    }

    const CODE: &str = "{12345678-9ABC-DEF0-1234-56789ABCDEF0}";

    fn msi(action: Action, file: MsiFile) -> SoftwareChange {
        SoftwareChange {
            packages: vec![SoftwarePackage {
                id: CODE.into(),
                source: String::new(),
                msi: Some(file),
            }],
            ..change(action, &[])
        }
    }

    fn file() -> MsiFile {
        MsiFile {
            sha256: "ab".repeat(32),
            size: 4096,
            urls: strings(&["http://relay.example:7480/software/x.msi"]),
            properties: vec![MsiProperty {
                name: "INSTALLDIR".into(),
                value: r"C:\Program Files\Tiny".into(),
            }],
            name: "Tiny".into(),
        }
    }

    #[test]
    fn product_codes_digests_and_properties() {
        assert!(valid_product_code(CODE));
        for bad in [
            "12345678-9ABC-DEF0-1234-56789ABCDEF0",
            "{12345678-9abc-def0-1234-56789abcdef0}",
            "{12345678-9ABC-DEF0-1234-56789ABCDEF}",
            "{123456789-ABC-DEF0-1234-56789ABCDEF0}",
            "{12345678-9ABC-DEF0-1234-56789ABCDEFG}",
        ] {
            assert!(!valid_product_code(bad), "{bad}");
        }
        assert!(valid_sha256(&"0f".repeat(32)));
        assert!(!valid_sha256(&"0F".repeat(32)));
        assert!(!valid_sha256(&"0f".repeat(31)));
        for good in ["INSTALLDIR", "ADDLOCAL", "SERVER_URL2"] {
            assert!(valid_msi_property(good), "{good}");
        }
        for bad in [
            "",
            "installdir",
            "_X",
            "2X",
            "A-B",
            "REBOOT",
            "TRANSFORMS",
            "REMOVE",
        ] {
            assert!(!valid_msi_property(bad), "{bad}");
        }
        assert!(valid_msi_value(r"C:\Program Files\x, y=z"));
        assert!(valid_msi_value(""));
        for bad in [
            "a\"b",
            "a;b",
            "a\nb",
            "\u{e9}",
            &"x".repeat(MAX_MSI_VALUE + 1),
        ] {
            assert!(!valid_msi_value(bad), "{bad}");
        }
        assert!(valid_msi_url("https://mf.example/software/x.msi"));
        for bad in [
            "ftp://x/y",
            "http://x/a b",
            "http://x/\"",
            "file:///c:/x.msi",
        ] {
            assert!(!valid_msi_url(bad), "{bad}");
        }
    }

    #[test]
    fn msi_packages_are_checked() {
        let install = msi(Action::Install, file());
        assert_eq!(check_change(&install, Some("windows")), Ok(()));
        let remove = msi(Action::Remove, MsiFile::default());
        assert_eq!(check_change(&remove, Some("windows")), Ok(()));
        // With winget packages in the same change.
        let mut mixed = install.clone();
        mixed
            .packages
            .extend(change(Action::Install, &[("7zip.7zip", "winget")]).packages);
        assert_eq!(check_change(&mixed, Some("windows")), Ok(()));

        let invalid = |change: &SoftwareChange| {
            matches!(
                check_change(change, Some("windows")),
                Err(SoftwareError::InvalidMsi(..))
            )
        };
        let with = |edit: &dyn Fn(&mut MsiFile)| {
            let mut file = file();
            edit(&mut file);
            msi(Action::Install, file)
        };
        assert!(invalid(&with(&|f| f.sha256 = "AB".repeat(32))));
        assert!(invalid(&with(&|f| f.size = 0)));
        assert!(invalid(&with(&|f| f.size = MAX_MSI_BYTES + 1)));
        assert!(invalid(&with(&|f| f.urls.clear())));
        assert!(invalid(&with(&|f| f.urls = vec!["ftp://x/y".into()])));
        assert!(invalid(&with(
            &|f| f.urls = vec!["http://x/y".into(); MAX_MSI_URLS + 1]
        )));
        assert!(invalid(&with(&|f| f.properties[0].name = "REBOOT".into())));
        assert!(invalid(&with(&|f| f.properties[0].value = "a\"b".into())));
        assert!(invalid(&with(&|f| {
            let again = f.properties[0].clone();
            f.properties.push(again)
        })));
        assert!(invalid(&with(&|f| f.name = "x".repeat(MAX_MSI_NAME + 1))));
        // A removal takes the ProductCode only.
        assert!(invalid(&msi(Action::Remove, file())));
        let mut lower = install.clone();
        lower.packages[0].id = CODE.to_ascii_lowercase();
        assert!(invalid(&lower));
        let mut sourced = install.clone();
        sourced.packages[0].source = "winget".into();
        assert!(invalid(&sourced));
        let mut twice = install.clone();
        twice.packages.push(twice.packages[0].clone());
        assert_eq!(
            check_change(&twice, None),
            Err(SoftwareError::Duplicate(CODE.into()))
        );
        // Windows only.
        let apt = SoftwareChange {
            manager: PackageManager::Apt.into(),
            ..install.clone()
        };
        assert_eq!(check_change(&apt, None), Err(SoftwareError::MsiOffWindows));
        // A winget ID is no ProductCode, and the reverse: older agents
        // refuse MSI packages rather than ask winget for them.
        let mut plain = install;
        plain.packages[0].msi = None;
        assert_eq!(
            check_change(&plain, None),
            Err(SoftwareError::InvalidId("winget", CODE.into()))
        );
    }
}
