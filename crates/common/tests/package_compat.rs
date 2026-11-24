//! `PackageReport.third_party`, `PackageReport.managers`,
//! `ApplyUpdates.skip_os` and `ApplyUpdates.third_party` (ADR-0138)
//! between versions. `OldPackageReport` and `OldApplyUpdates` are the
//! messages before ADR-0138, field for field (`storage` left out of the
//! report: it is a nested message these tests do not need). Relays decode
//! every envelope and encode it again, and prost keeps no unknown fields.

use monitorflow_common::proto::v1::{
    package_manager_status::State, ApplyUpdates, Package, PackageManager, PackageManagerStatus,
    PackageReport, PackageScope, ThirdPartyPackage, ThirdPartySelection,
};
use prost::Message;

#[derive(Clone, PartialEq, Message)]
struct OldPackageReport {
    #[prost(message, repeated, tag = "1")]
    packages: Vec<Package>,
    #[prost(bool, tag = "2")]
    reboot_required: bool,
    #[prost(int64, tag = "3")]
    collected_at_unix_ms: i64,
}

#[derive(Clone, PartialEq, Message)]
struct OldApplyUpdates {
    #[prost(string, repeated, tag = "1")]
    packages: Vec<String>,
    #[prost(bool, tag = "2")]
    security_only: bool,
    #[prost(bool, tag = "3")]
    allow_reboot: bool,
}

fn os_package() -> Package {
    Package {
        name: "openssl".into(),
        version: "3.0.11-1".into(),
        arch: "amd64".into(),
        upgradable_version: "3.0.13-1".into(),
        ..Package::default()
    }
}

fn report() -> PackageReport {
    PackageReport {
        packages: vec![os_package()],
        reboot_required: true,
        collected_at_unix_ms: 1_800_000_000_000,
        third_party: vec![ThirdPartyPackage {
            manager: PackageManager::Flatpak.into(),
            id: "app/org.mozilla.firefox/x86_64/stable".into(),
            name: "Firefox".into(),
            version: "131.0".into(),
            available_version: "132.0".into(),
            scope: PackageScope::Machine.into(),
            origin: "flathub".into(),
        }],
        managers: vec![
            PackageManagerStatus {
                manager: PackageManager::Flatpak.into(),
                state: State::Available.into(),
                version: "1.14.4".into(),
                error: String::new(),
            },
            PackageManagerStatus {
                manager: PackageManager::Snap.into(),
                state: State::Absent.into(),
                ..PackageManagerStatus::default()
            },
        ],
        ..PackageReport::default()
    }
}

#[test]
fn a_new_report_round_trips() {
    let new = report();
    assert_eq!(
        PackageReport::decode(new.encode_to_vec().as_slice()).unwrap(),
        new
    );
}

#[test]
fn an_old_reader_takes_the_os_part_of_a_new_report() {
    let new = report();
    let old = OldPackageReport::decode(new.encode_to_vec().as_slice()).unwrap();
    assert_eq!(old.packages, new.packages);
    assert!(old.reboot_required);
    assert_eq!(old.collected_at_unix_ms, new.collected_at_unix_ms);
}

/// An older relay drops both fields together, which the server reads as
/// "not reported", never as "no apps".
#[test]
fn an_old_relay_drops_third_party_and_managers_together() {
    let new = report();
    let old = OldPackageReport::decode(new.encode_to_vec().as_slice()).unwrap();
    let passed_on = PackageReport::decode(old.encode_to_vec().as_slice()).unwrap();
    assert_eq!(
        passed_on,
        PackageReport {
            third_party: Vec::new(),
            managers: Vec::new(),
            ..new
        }
    );
}

#[test]
fn an_old_agent_reports_no_managers() {
    let old = OldPackageReport {
        packages: vec![os_package()],
        reboot_required: false,
        collected_at_unix_ms: 1,
    };
    let report = PackageReport::decode(old.encode_to_vec().as_slice()).unwrap();
    assert!(report.managers.is_empty());
    assert!(report.third_party.is_empty());
    assert_eq!(report.packages, old.packages);
}

/// A manager, scope or state from a newer agent decodes, and reads as
/// unspecified, so the server can skip it.
#[test]
fn unknown_values_survive_decoding() {
    let mut new = report();
    new.third_party[0].manager = 99;
    new.third_party[0].scope = 7;
    new.managers[0].state = 9;
    let decoded = PackageReport::decode(new.encode_to_vec().as_slice()).unwrap();
    assert_eq!(decoded.third_party[0].manager, 99);
    assert_eq!(
        decoded.third_party[0].manager(),
        PackageManager::Unspecified
    );
    assert_eq!(decoded.third_party[0].scope(), PackageScope::Unspecified);
    assert_eq!(decoded.managers[0].state(), State::Unspecified);
}

fn command() -> ApplyUpdates {
    ApplyUpdates {
        packages: vec!["openssl".into()],
        security_only: true,
        allow_reboot: true,
        skip_os: false,
        third_party: vec![
            ThirdPartySelection {
                manager: PackageManager::Winget.into(),
                packages: vec!["7zip.7zip".into()],
                hold: vec!["Mozilla.Firefox".into()],
            },
            ThirdPartySelection {
                manager: PackageManager::Snap.into(),
                packages: Vec::new(),
                hold: Vec::new(),
            },
        ],
        classifications: vec!["Security Updates".into()],
        hold: vec!["KB5034441".into()],
    }
}

#[test]
fn a_new_command_round_trips() {
    let new = ApplyUpdates {
        skip_os: true,
        packages: Vec::new(),
        ..command()
    };
    assert_eq!(
        ApplyUpdates::decode(new.encode_to_vec().as_slice()).unwrap(),
        new
    );
}

/// An older agent reads a new command as its OS part, which is why the
/// server never sends `skip_os` to one (ADR-0138).
#[test]
fn an_old_agent_reads_the_os_part_of_a_new_command() {
    let new = ApplyUpdates {
        skip_os: true,
        ..command()
    };
    let old = OldApplyUpdates::decode(new.encode_to_vec().as_slice()).unwrap();
    assert_eq!(old.packages, new.packages);
    assert!(old.security_only && old.allow_reboot);
}

/// A command from an older server is an OS update, unchanged.
#[test]
fn an_old_command_is_os_only() {
    let old = OldApplyUpdates {
        packages: vec!["libc6".into()],
        security_only: true,
        allow_reboot: false,
    };
    let new = ApplyUpdates::decode(old.encode_to_vec().as_slice()).unwrap();
    assert!(!new.skip_os);
    assert!(new.third_party.is_empty());
    assert_eq!(new.packages, old.packages);
    assert_eq!(
        monitorflow_common::third_party::check(&new, Some("debian")),
        Ok(())
    );
}

/// The bytes of a command without the new fields do not change, so a
/// signature over an OS-only command is the same from either version.
#[test]
fn an_os_only_command_encodes_as_before() {
    let old = OldApplyUpdates {
        packages: vec!["libc6".into()],
        security_only: true,
        allow_reboot: true,
    };
    let new = ApplyUpdates {
        packages: old.packages.clone(),
        security_only: true,
        allow_reboot: true,
        ..ApplyUpdates::default()
    };
    assert_eq!(new.encode_to_vec(), old.encode_to_vec());
}
