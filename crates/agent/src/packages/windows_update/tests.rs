use super::*;

fn names(n: &[&str]) -> Vec<String> {
    n.iter().map(|s| s.to_string()).collect()
}

fn install(kind: Kind, updates: &[&str]) -> Request {
    Request::Install {
        kind,
        updates: names(updates),
        classifications: vec![],
        hold: vec![],
    }
}

#[test]
fn requests_round_trip_as_json() {
    for (request, json) in [
        (Request::Scan, r#"{"op":"scan"}"#),
        (Request::Status, r#"{"op":"status"}"#),
        (Request::Reboot, r#"{"op":"reboot"}"#),
        (install(Kind::All, &[]), r#"{"op":"install","kind":"all"}"#),
        (
            install(Kind::Named, &["KB5034441"]),
            r#"{"op":"install","kind":"named","updates":["KB5034441"]}"#,
        ),
        (Request::WingetScan, r#"{"op":"winget_scan"}"#),
        (
            Request::WingetUpgrade {
                ids: names(&["Example.App"]),
                hold: vec![],
            },
            r#"{"op":"winget_upgrade","ids":["Example.App"]}"#,
        ),
        (
            Request::Install {
                kind: Kind::All,
                updates: vec![],
                classifications: names(&["Updates"]),
                hold: names(&["KB1"]),
            },
            r#"{"op":"install","kind":"all","classifications":["Updates"],"hold":["KB1"]}"#,
        ),
        (
            Request::WingetInstall {
                packages: vec![WingetPackage {
                    id: "Example.App".into(),
                    source: "winget".into(),
                }],
            },
            r#"{"op":"winget_install","packages":[{"id":"Example.App","source":"winget"}]}"#,
        ),
        (
            Request::WingetRemove {
                packages: vec![WingetPackage {
                    id: "Example.App".into(),
                    source: String::new(),
                }],
            },
            r#"{"op":"winget_remove","packages":[{"id":"Example.App"}]}"#,
        ),
        (
            Request::signed(&Command::default(), Step::Software, None),
            r#"{"op":"signed","command":"CgA=","step":"software"}"#,
        ),
        (
            Request::signed(&Command::default(), Step::SoftwareMsi, None),
            r#"{"op":"signed","command":"CgA=","step":"software_msi"}"#,
        ),
        (
            Request::MsiRemove {
                packages: vec![crate::packages::msi::MsiPackage {
                    product_code: "{0D3D4C9E-6C7B-4F5B-9F65-2B8F1E4A7C11}".into(),
                    name: String::new(),
                    sha256: String::new(),
                    size: 0,
                    urls: vec![],
                    properties: vec![],
                }],
            },
            r#"{"op":"msi_remove","packages":[{"product_code":"{0D3D4C9E-6C7B-4F5B-9F65-2B8F1E4A7C11}"}]}"#,
        ),
        (
            Request::signed(&Command::default(), Step::InstallApproved, None),
            r#"{"op":"signed","command":"CgA=","step":"install_approved"}"#,
        ),
        (
            Request::signed(&Command::default(), Step::Winget, None),
            r#"{"op":"signed","command":"CgA=","step":"winget"}"#,
        ),
    ] {
        assert_eq!(serde_json::to_string(&request).unwrap(), json);
        assert_eq!(serde_json::from_str::<Request>(json).unwrap(), request);
    }
    for bad in [
        r#"{"op":"run","command":"cmd.exe"}"#,
        r#"{"op":"install","kind":"everything"}"#,
        r#"{"op":"install","kind":"all","source":"http://evil"}"#,
        r#"{"kind":"all"}"#,
        r#"{"op":"install","kind":"named","updates":"KB1"}"#,
        r#"{"op":"winget_upgrade","ids":["Example.App"],"force":true}"#,
    ] {
        assert!(serde_json::from_str::<Request>(bad).is_err(), "{bad}");
    }
    let reply = Reply::Rebooting(());
    assert_eq!(
        serde_json::to_string(&reply).unwrap(),
        r#"{"rebooting":null}"#
    );
    let reply: Reply = serde_json::from_str(r#"{"error":"no"}"#).unwrap();
    assert_eq!(reply, Reply::Error("no".into()));
    let reply = Reply::Apps(crate::packages::winget::Apps::absent("none"));
    assert_eq!(
        serde_json::to_string(&reply).unwrap(),
        r#"{"apps":{"state":"absent","version":"","error":"none","apps":[]}}"#
    );
}

#[test]
fn winget_ids_are_checked() {
    let upgrade = |ids: &[&str]| Request::WingetUpgrade {
        ids: names(ids),
        hold: vec![],
    };
    assert_eq!(upgrade(&[]).validate(), Ok(()));
    assert_eq!(
        upgrade(&["Example.App", "Example.Other"]).validate(),
        Ok(())
    );
    for bad in ["KB5034441", "Example.App'", "-x.y", "Example App"] {
        assert_eq!(
            upgrade(&[bad]).validate(),
            Err(RequestError::InvalidWingetId(
                bad.escape_debug().to_string()
            ))
        );
    }
    let many: Vec<String> = (0..=MAX_NAMES).map(|i| format!("Example.App{i}")).collect();
    assert_eq!(
        Request::WingetUpgrade {
            ids: many,
            hold: vec![]
        }
        .validate(),
        Err(RequestError::TooManyNames)
    );
}

#[test]
fn self_updates_round_trip_and_are_checked() {
    let json = r#"{"op":"self_update","command_id":"c-1","version":"0.3.0","data_dir":"C:\\ProgramData\\MonitorFlow\\agent","allow_downgrade":false,"health_timeout_ms":300000}"#;
    let request: Request = serde_json::from_str(json).unwrap();
    let Request::SelfUpdate(update) = &request else {
        panic!("{request:?}");
    };
    assert_eq!(update.command_id, "c-1");
    assert_eq!(serde_json::to_string(&request).unwrap(), json);
    let extra = r#"{"op":"self_update","command_id":"c-1","version":"0.3.0","data_dir":"x","allow_downgrade":false,"health_timeout_ms":0,"exe":"x"}"#;
    assert!(serde_json::from_str::<Request>(extra).is_err());
    for (command_id, timeout) in [("../x", 0), ("c-1", 3_600_001)] {
        let bad = Request::SelfUpdate(crate::self_update::swap::Request {
            command_id: command_id.into(),
            version: "0.3.0".into(),
            data_dir: std::env::temp_dir(),
            allow_downgrade: false,
            health_timeout_ms: timeout,
        });
        assert!(
            matches!(bad.validate(), Err(RequestError::SelfUpdate(_))),
            "{bad:?}"
        );
    }
    let reply = Reply::SelfUpdate(SelfUpdated {
        restarting: true,
        message: "installed".into(),
    });
    let json = serde_json::to_string(&reply).unwrap();
    assert_eq!(
        json,
        r#"{"self_update":{"restarting":true,"message":"installed"}}"#
    );
}

#[test]
fn requests_are_checked() {
    assert_eq!(install(Kind::All, &[]).validate(), Ok(()));
    assert_eq!(install(Kind::Security, &[]).validate(), Ok(()));
    assert_eq!(
        install(
            Kind::Named,
            &["kb1", "4f2fc5ad-8a6b-4b61-a0f7-1a2e0f3b9c01"]
        )
        .validate(),
        Ok(())
    );
    assert_eq!(
        install(Kind::All, &["KB1"]).validate(),
        Err(RequestError::UnexpectedNames("all"))
    );
    assert_eq!(
        install(Kind::Named, &[]).validate(),
        Err(RequestError::NoNames)
    );
    for bad in ["openssl", "KB1;shutdown", "-KB1", "KB", ""] {
        assert!(
            matches!(
                install(Kind::Named, &[bad]).validate(),
                Err(RequestError::InvalidName(_))
            ),
            "{bad}"
        );
    }
    let many: Vec<String> = (0..=MAX_NAMES).map(|i| format!("KB{i}")).collect();
    let many = Request::Install {
        kind: Kind::Named,
        updates: many,
        classifications: vec![],
        hold: vec![],
    };
    assert_eq!(many.validate(), Err(RequestError::TooManyNames));
}

#[test]
fn apply_updates_maps_to_installs() {
    let apply = |packages: &[&str], security_only| ApplyUpdates {
        packages: names(packages),
        security_only,
        allow_reboot: false,
        ..ApplyUpdates::default()
    };
    assert_eq!(
        Request::install(&apply(&[], false)),
        install(Kind::All, &[])
    );
    assert_eq!(
        Request::install(&apply(&["KB1"], false)),
        install(Kind::Named, &["KB1"])
    );
    assert_eq!(
        Request::install(&apply(&[], true)),
        install(Kind::Security, &[])
    );
    assert_eq!(
        Request::install(&apply(&["KB1"], true)),
        install(Kind::Security, &["KB1"])
    );
}

#[tokio::test]
async fn frames_carry_one_message_and_respect_their_limit() {
    let (mut a, mut b) = tokio::io::duplex(1 << 16);
    write_frame(&mut a, &install(Kind::Named, &["KB1"]))
        .await
        .unwrap();
    let request: Request = read_frame(&mut b, MAX_REQUEST_BYTES).await.unwrap();
    assert_eq!(request, install(Kind::Named, &["KB1"]));

    write_frame(&mut a, &Request::Scan).await.unwrap();
    let error = read_frame::<_, Request>(&mut b, 4).await.unwrap_err();
    assert!(matches!(error, FrameError::TooLarge(4)), "{error}");

    let (mut a, mut b) = tokio::io::duplex(64);
    a.write_all(&3u32.to_be_bytes()).await.unwrap();
    a.write_all(b"{x}").await.unwrap();
    let error = read_frame::<_, Request>(&mut b, 64).await.unwrap_err();
    assert!(matches!(error, FrameError::Json(_)), "{error}");
    drop(a);
    let error = read_frame::<_, Request>(&mut b, 64).await.unwrap_err();
    assert!(matches!(error, FrameError::Io(_)), "{error}");
}

fn found(kb: &str, classification: &str, installed: bool) -> Found {
    Found {
        title: format!("{kb} title"),
        kb_ids: if kb.is_empty() {
            Vec::new()
        } else {
            vec![kb.into()]
        },
        update_id: "4F2FC5AD-8A6B-4B61-A0F7-1A2E0F3B9C01".into(),
        revision: 200,
        classification_id: classification.to_uppercase(),
        classification_name: "Mises à jour".into(),
        installed,
        ..Found::default()
    }
}

#[test]
fn updates_are_named_by_kb_with_english_classifications() {
    let update = found("5034441", SECURITY_UPDATES, false);
    assert_eq!(update.name(), "KB5034441");
    assert_eq!(update.classification(), "Security Updates");
    assert!(update.matches("kb5034441"));
    assert!(update.matches("4f2fc5ad-8a6b-4b61-a0f7-1a2e0f3b9c01"));
    assert!(!update.matches("KB503444"));
    let no_kb = found("", "00000000-0000-0000-0000-000000000000", false);
    assert_eq!(no_kb.name(), "4f2fc5ad-8a6b-4b61-a0f7-1a2e0f3b9c01");
    assert_eq!(no_kb.classification(), "Mises à jour");
}

#[test]
fn selection_follows_the_kind() {
    let rollup = "28bc880e-0592-4cbf-8f95-c79b17911d5f";
    let updates = [
        found("1", SECURITY_UPDATES, false),
        found("2", rollup, false),
        found("3", UPGRADES, false),
        found("4", SECURITY_UPDATES, true),
        found("5", SECURITY_UPDATES, false),
    ];
    let pick = |kind, n: &[&str]| {
        let names = names(n);
        let (picked, not_pending, held) = select(
            Selection {
                kind,
                names: &names,
                classifications: &[],
                hold: &[],
            },
            &updates,
        );
        assert!(held.is_empty());
        (picked, not_pending)
    };
    assert_eq!(pick(Kind::All, &[]), (vec![0, 1, 4], vec![]));
    assert_eq!(pick(Kind::Security, &[]), (vec![0, 4], vec![]));
    assert_eq!(
        pick(Kind::Security, &["KB5", "KB2", "KB9"]),
        (vec![4], names(&["KB9"]))
    );
    // Feature upgrades only when named; installed updates never.
    assert_eq!(
        pick(Kind::Named, &["KB3", "KB4"]),
        (vec![2], names(&["KB4"]))
    );
}

#[test]
fn approvals_and_holds_narrow_the_selection() {
    let rollup = "28bc880e-0592-4cbf-8f95-c79b17911d5f";
    let updates = [
        found("1", SECURITY_UPDATES, false),
        found("2", rollup, false),
        found("3", UPGRADES, false),
        found("5", SECURITY_UPDATES, false),
    ];
    let pick = |kind, n: &[&str], classifications: &[&str], hold: &[&str]| {
        let (n, c, h) = (names(n), names(classifications), names(hold));
        select(
            Selection {
                kind,
                names: &n,
                classifications: &c,
                hold: &h,
            },
            &updates,
        )
    };
    // Approved classifications only; Upgrades only when approved.
    assert_eq!(
        pick(Kind::All, &[], &["Update Rollups"], &[]),
        (vec![1], vec![], vec![])
    );
    assert_eq!(
        pick(Kind::All, &[], &["Upgrades", "Security Updates"], &[]),
        (vec![0, 2, 3], vec![], vec![])
    );
    // Held updates are left out whatever selects them, named ones too.
    assert_eq!(
        pick(Kind::All, &[], &[], &["kb5"]),
        (vec![0, 1], vec![], names(&["KB5"]))
    );
    assert_eq!(
        pick(Kind::Named, &["KB1"], &[], &["KB1"]),
        (vec![], vec![], names(&["KB1"]))
    );
    assert_eq!(
        pick(Kind::Security, &[], &["Update Rollups"], &[]),
        (vec![], vec![], vec![])
    );
}

#[test]
fn approvals_are_checked_on_the_pipe() {
    let request = |classifications: &[&str], hold: &[&str]| Request::Install {
        kind: Kind::All,
        updates: vec![],
        classifications: names(classifications),
        hold: names(hold),
    };
    assert_eq!(request(&["Updates"], &["KB1"]).validate(), Ok(()));
    assert!(matches!(
        request(&["Drivers"], &[]).validate(),
        Err(RequestError::Approval(_))
    ));
    assert!(matches!(
        request(&[], &["openssl"]).validate(),
        Err(RequestError::Approval(_))
    ));
}

#[test]
fn software_requests_follow_the_command() {
    use monitorflow_common::proto::v1::SoftwarePackage;

    let change = |action: Action, source: &str| SoftwareChange {
        action: action.into(),
        manager: PackageManager::Winget.into(),
        packages: vec![SoftwarePackage {
            id: "Example.App".into(),
            source: source.into(),
            msi: None,
        }],
        allow_reboot: false,
    };
    let install = Request::software(&change(Action::Install, "winget"));
    assert!(
        matches!(&install, Request::WingetInstall { packages } if packages[0].source == "winget")
    );
    assert_eq!(install.validate(), Ok(()));
    let remove = Request::software(&change(Action::Remove, ""));
    assert!(matches!(&remove, Request::WingetRemove { .. }));
    assert_eq!(remove.validate(), Ok(()));
    assert!(matches!(
        Request::software(&change(Action::Install, "msstore")).validate(),
        Err(RequestError::Software(_))
    ));
    // Software needs the host's consent; nothing else does.
    assert_eq!(consented(&install, true), Ok(()));
    assert_eq!(consented(&install, false), Err(SOFTWARE_NOT_ALLOWED.into()));
    assert_eq!(consented(&remove, false), Err(SOFTWARE_NOT_ALLOWED.into()));
    assert_eq!(consented(&Request::Scan, false), Ok(()));
}

#[test]
fn changes_are_described() {
    let installed = Installed {
        results: vec![
            InstallResult {
                name: "Example.App".into(),
                title: "Example.App".into(),
                result: Outcome::Succeeded,
                hresult: 0,
            },
            InstallResult {
                name: "Example.Bad".into(),
                title: "Example.Bad: hash mismatch".into(),
                result: Outcome::Failed,
                hresult: -1978335215,
            },
        ],
        unchanged: names(&["Example.There"]),
        reboot_required: true,
        ..Installed::default()
    };
    let (out, error) = describe_change(&installed, true);
    assert_eq!(
        out,
        "Example.App succeeded: Example.App\n\
         Example.Bad failed (0x8a150011): Example.Bad: hash mismatch\n\
         Example.There is installed already\n\
         a restart is required\n"
    );
    assert_eq!(
        error.as_deref(),
        Some("1 of 2 packages did not install: Example.Bad (0x8a150011)")
    );
    let (out, error) = describe_change(
        &Installed {
            unchanged: names(&["Example.Gone"]),
            ..Installed::default()
        },
        false,
    );
    assert_eq!(out, "Example.Gone is not installed\n");
    assert_eq!(error, None);
    let held = Installed {
        held: names(&["KB1"]),
        ..Installed::default()
    };
    assert!(describe(&held).0.contains("KB1 is held\n"));
}

#[test]
fn updates_become_packages_one_per_name() {
    let update = |name: &str, installed, size, reboot| Update {
        name: name.into(),
        title: format!("{name} {installed}"),
        classification: "Updates".into(),
        severity: String::new(),
        size_bytes: size,
        revision: if installed { 1 } else { 2 },
        installed,
        reboot_required: reboot,
    };
    let packages = to_packages(&[
        update("KB1", true, 10, false),
        update("KB2", false, 5, false),
        update("KB1", false, 20, true),
        update("KB1", false, 30, false),
        update("KB3", true, 0, true),
    ]);
    let summary: Vec<(&str, &str, &str, u64, bool, &str)> = packages
        .iter()
        .map(|p| {
            (
                p.name.as_str(),
                p.version.as_str(),
                p.upgradable_version.as_str(),
                p.size_bytes,
                p.reboot_required,
                p.title.as_str(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("KB1", "", "2", 50, true, "KB1 false"),
            ("KB2", "", "2", 5, false, "KB2 false"),
            ("KB3", "1", "", 0, true, "KB3 true"),
        ]
    );
    assert!(packages.iter().all(|p| p.arch.is_empty()));
}

#[test]
fn install_output_names_failures() {
    let result = |name: &str, result, hresult| InstallResult {
        name: name.into(),
        title: "t".into(),
        result,
        hresult,
    };
    let installed = Installed {
        results: vec![
            result("KB1", Outcome::Succeeded, 0),
            result("KB2", Outcome::Failed, 0x80070643_u32 as i32),
            result("KB3", Outcome::Aborted, 0x80240022_u32 as i32),
        ],
        not_pending: names(&["KB9"]),
        reboot_required: true,
        ..Installed::default()
    };
    let (text, error) = describe(&installed);
    assert_eq!(
        text,
        "KB1 succeeded: t\nKB2 failed (0x80070643): t\nKB3 aborted (0x80240022): t\n\
         KB9 is not pending\na restart is required\n"
    );
    assert_eq!(
        error.as_deref(),
        Some("2 of 3 updates did not install: KB2 (0x80070643), KB3 (0x80240022)")
    );
    assert_eq!(
        describe(&Installed::default()),
        ("nothing to install\n".into(), None)
    );
}

#[test]
fn service_sids_match_windows() {
    // `sc.exe showsid TrustedInstaller`.
    assert_eq!(
        service_sid("TrustedInstaller"),
        "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464"
    );
    assert_eq!(
        service_sid("trustedinstaller"),
        service_sid("TrustedInstaller")
    );
    assert_eq!(
        pipe_sddl("S-1-5-80-1"),
        "D:P(A;;GA;;;SY)(A;;GRGW;;;S-1-5-80-1)"
    );
}

#[test]
fn only_system_or_administrators_may_own_the_pipe() {
    for owner in TRUSTED_PIPE_OWNERS {
        assert_eq!(
            check_pipe_owner(PIPE_NAME, owner, Some(4), &TRUSTED_PIPE_OWNERS),
            Ok(())
        );
    }
    let user = "S-1-5-21-1-2-3-1001";
    let refused = check_pipe_owner(PIPE_NAME, user, Some(4242), &TRUSTED_PIPE_OWNERS).unwrap_err();
    assert!(
        refused.starts_with(r"refusing \\.\pipe\MonitorFlowUpdate: "),
        "{refused}"
    );
    assert!(
        refused.contains("process 4242 owned by S-1-5-21-1-2-3-1001"),
        "{refused}"
    );
    assert!(refused.contains("nothing was sent"), "{refused}");
    let refused = check_pipe_owner(PIPE_NAME, user, None, &TRUSTED_PIPE_OWNERS).unwrap_err();
    assert!(refused.contains("held by a process owned by"), "{refused}");
    // The agent's own service SID may write the pipe, not own it.
    assert!(check_pipe_owner(
        PIPE_NAME,
        &service_sid(AGENT_SERVICE),
        None,
        &TRUSTED_PIPE_OWNERS
    )
    .is_err());
}

#[test]
fn a_taken_pipe_name_is_explained() {
    let error = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
    let message = pipe_taken(PIPE_NAME, &error);
    assert!(
        message.contains("another process holds the name"),
        "{message}"
    );
    assert!(message.contains("restarted"), "{message}");
}

/// The service's side of ADR-0108: what its pipe received against its own
/// trust anchor.
mod signed {
    use monitorflow_common::proto::v1::{SelfUpdate, ThirdPartySelection};

    use super::*;
    use crate::helper_trust::tests::{unsigned, Ca, Host};
    use crate::self_update::unix_ms;

    fn apply(packages: &[&str], allow_reboot: bool) -> Vec<u8> {
        ApplyUpdates {
            packages: names(packages),
            security_only: false,
            allow_reboot,
            ..ApplyUpdates::default()
        }
        .encode_to_vec()
    }

    /// The data directory of the service's `agent.toml`.
    fn configured() -> std::path::PathBuf {
        std::env::temp_dir().join("configured")
    }

    /// A command valid now, signed by `ca`.
    fn command(ca: &Ca, id: &str, kind: &str, payload: &[u8]) -> Command {
        let mut command = unsigned(kind, payload);
        command.command_id = id.into();
        command.expires_at_unix_ms = unix_ms() + 60_000;
        ca.sign(command)
    }

    #[test]
    fn reads_pass_and_anything_else_needs_its_signed_command() {
        let ca = Ca::new();
        let host = Host::pinned(&ca);
        let check = |request: &Request| authorized(request, &host.place, &configured(), unix_ms());
        assert_eq!(check(&Request::Scan), Ok(Request::Scan));
        assert_eq!(check(&Request::Status), Ok(Request::Status));

        // What the agent's account could ask before, in its own words.
        let update = crate::self_update::swap::Request {
            command_id: "c-1".into(),
            version: "0.1.0".into(),
            data_dir: std::env::temp_dir(),
            allow_downgrade: true,
            health_timeout_ms: 0,
        };
        for bare in [
            install(Kind::All, &[]),
            Request::Reboot,
            Request::SelfUpdate(update),
        ] {
            let error = check(&bare).unwrap_err();
            assert!(error.contains("signed"), "{error}");
        }

        let unsigned_command = unsigned(APPLY_UPDATES, &apply(&[], true));
        let forged = Ca::new().sign(unsigned_command.clone());
        for bad in [unsigned_command, forged] {
            for step in [Step::Install, Step::Reboot] {
                assert!(check(&Request::signed(&bad, step, None)).is_err());
            }
        }
        let garbage = Request::Signed {
            command: "not base64".into(),
            step: Step::Install,
            data_dir: None,
        };
        assert!(check(&garbage).unwrap_err().contains("does not decode"));
    }

    #[test]
    fn an_update_installs_then_restarts_once_each_and_only_when_allowed() {
        let ca = Ca::new();
        let host = Host::pinned(&ca);
        let check = |request: &Request| authorized(request, &host.place, &configured(), unix_ms());

        let allowed = command(&ca, "c-1", APPLY_UPDATES, &apply(&["KB5034441"], true));
        let step = |step| Request::signed(&allowed, step, None);
        assert_eq!(
            check(&step(Step::Install)),
            Ok(install(Kind::Named, &["KB5034441"]))
        );
        assert_eq!(check(&step(Step::Reboot)), Ok(Request::Reboot));
        for again in [Step::Install, Step::Reboot] {
            assert!(check(&step(again)).unwrap_err().contains("already run"));
        }
        // An `apply_updates` is no `self_update`.
        let as_update = Request::signed(&allowed, Step::SelfUpdate, Some(&std::env::temp_dir()));
        assert!(check(&as_update).unwrap_err().contains("self_update"));

        let quiet = command(&ca, "c-2", APPLY_UPDATES, &apply(&[], false));
        let reboot = Request::signed(&quiet, Step::Reboot, None);
        assert!(check(&reboot).unwrap_err().contains("does not allow"));
        let named = command(&ca, "c-3", APPLY_UPDATES, &apply(&["openssl"], false));
        let bad_name = Request::signed(&named, Step::Install, None);
        assert!(check(&bad_name).unwrap_err().contains("not a KB number"));
    }

    fn winget(ids: &[&str], skip_os: bool) -> Vec<u8> {
        ApplyUpdates {
            skip_os,
            allow_reboot: true,
            third_party: vec![ThirdPartySelection {
                manager: PackageManager::Winget.into(),
                packages: names(ids),
                ..ThirdPartySelection::default()
            }],
            ..ApplyUpdates::default()
        }
        .encode_to_vec()
    }

    #[test]
    fn approvals_and_holds_come_from_the_signed_command() {
        let ca = Ca::new();
        let host = Host::pinned(&ca);
        let check = |request: &Request| authorized(request, &host.place, &configured(), unix_ms());
        let payload = ApplyUpdates {
            classifications: names(&["Security Updates"]),
            hold: names(&["KB5034441"]),
            third_party: vec![ThirdPartySelection {
                manager: PackageManager::Winget.into(),
                hold: names(&["Example.Held"]),
                ..ThirdPartySelection::default()
            }],
            ..ApplyUpdates::default()
        };
        let signed = command(&ca, "a-1", APPLY_UPDATES, &payload.encode_to_vec());
        assert_eq!(Step::install_for(&payload), Step::InstallApproved);
        assert_eq!(Step::winget_for(&payload), Step::WingetApproved);
        assert_eq!(
            check(&Request::signed(&signed, Step::InstallApproved, None)),
            Ok(Request::Install {
                kind: Kind::All,
                updates: vec![],
                classifications: names(&["Security Updates"]),
                hold: names(&["KB5034441"]),
            })
        );
        assert_eq!(
            check(&Request::signed(&signed, Step::WingetApproved, None)),
            Ok(Request::WingetUpgrade {
                ids: vec![],
                hold: names(&["Example.Held"]),
            })
        );
        assert_eq!(Step::install_for(&ApplyUpdates::default()), Step::Install);
        assert_eq!(Step::winget_for(&ApplyUpdates::default()), Step::Winget);
    }

    fn software(action: Action, allow_reboot: bool) -> Vec<u8> {
        SoftwareChange {
            action: action.into(),
            manager: PackageManager::Winget.into(),
            packages: vec![monitorflow_common::proto::v1::SoftwarePackage {
                id: "Example.App".into(),
                source: "winget".into(),
                msi: None,
            }],
            allow_reboot,
        }
        .encode_to_vec()
    }

    #[test]
    fn software_takes_its_packages_from_the_signed_command_once() {
        let ca = Ca::new();
        let host = Host::pinned(&ca);
        let check = |request: &Request| authorized(request, &host.place, &configured(), unix_ms());
        let bare = Request::WingetInstall {
            packages: vec![WingetPackage {
                id: "Example.App".into(),
                source: "winget".into(),
            }],
        };
        assert!(check(&bare).unwrap_err().contains("signed"));

        let install = command(&ca, "s-1", SOFTWARE, &software(Action::Install, true));
        let step = |step| Request::signed(&install, step, None);
        assert_eq!(check(&step(Step::Software)), Ok(bare.clone()));
        assert!(check(&step(Step::Software))
            .unwrap_err()
            .contains("already run"));
        assert_eq!(check(&step(Step::Reboot)), Ok(Request::Reboot));
        // A software command is no update.
        assert!(check(&step(Step::Install)).is_err());

        let remove = command(&ca, "s-2", SOFTWARE, &software(Action::Remove, false));
        assert!(matches!(
            check(&Request::signed(&remove, Step::Software, None)),
            Ok(Request::WingetRemove { .. })
        ));
        let error = check(&Request::signed(&remove, Step::Reboot, None)).unwrap_err();
        assert!(error.contains("does not allow"), "{error}");
        // An update is no software command.
        let update = command(&ca, "s-3", APPLY_UPDATES, &apply(&[], false));
        assert!(check(&Request::signed(&update, Step::Software, None)).is_err());
    }

    #[test]
    fn msi_packages_take_their_own_step_of_the_signed_command() {
        use crate::packages::msi::MsiPackage;
        use monitorflow_common::proto::v1::{MsiFile, SoftwarePackage};

        let ca = Ca::new();
        let host = Host::pinned(&ca);
        let check = |request: &Request| authorized(request, &host.place, &configured(), unix_ms());
        let code = "{0D3D4C9E-6C7B-4F5B-9F65-2B8F1E4A7C11}";
        let mut change =
            SoftwareChange::decode(software(Action::Install, false).as_slice()).unwrap();
        change.packages.push(SoftwarePackage {
            id: code.into(),
            source: String::new(),
            msi: Some(MsiFile {
                sha256: "ab".repeat(32),
                size: 10,
                urls: vec!["http://relay.example:7480/software/x.msi".into()],
                name: "Tiny".into(),
                ..MsiFile::default()
            }),
        });
        let signed = command(&ca, "m-1", SOFTWARE, &change.encode_to_vec());
        let step = |step| Request::signed(&signed, step, None);
        // Each step takes its own packages.
        assert_eq!(
            check(&step(Step::Software)),
            Ok(Request::WingetInstall {
                packages: vec![WingetPackage {
                    id: "Example.App".into(),
                    source: "winget".into(),
                }],
            })
        );
        let msi = MsiPackage::of(&change.packages[1]).unwrap();
        assert_eq!(
            check(&step(Step::SoftwareMsi)),
            Ok(Request::MsiInstall {
                packages: vec![msi.clone()]
            })
        );
        assert!(check(&step(Step::SoftwareMsi))
            .unwrap_err()
            .contains("already run"));
        // Never from the pipe alone, and only with the host's consent.
        let bare = Request::MsiInstall {
            packages: vec![msi],
        };
        assert!(check(&bare).unwrap_err().contains("signed"));
        assert_eq!(consented(&bare, false), Err(SOFTWARE_NOT_ALLOWED.into()));
        assert_eq!(consented(&bare, true), Ok(()));
        // A command without MSI packages has no such step.
        let winget = command(&ca, "m-2", SOFTWARE, &software(Action::Install, false));
        let error = check(&Request::signed(&winget, Step::SoftwareMsi, None)).unwrap_err();
        assert!(error.contains("no MSI package"), "{error}");
        // A removal names the ProductCode only.
        let mut remove = change.clone();
        remove.action = Action::Remove.into();
        remove.packages[0].source.clear();
        remove.packages[1].msi = Some(MsiFile {
            name: "Tiny".into(),
            ..MsiFile::default()
        });
        let removal = command(&ca, "m-3", SOFTWARE, &remove.encode_to_vec());
        assert!(matches!(
            check(&Request::signed(&removal, Step::SoftwareMsi, None)),
            Ok(Request::MsiRemove { packages }) if packages[0].product_code == code
        ));
        // A bad package fails the whole command, as winget's do.
        let mut bad = change;
        bad.packages[1].msi.as_mut().unwrap().properties =
            vec![monitorflow_common::proto::v1::MsiProperty {
                name: "REBOOT".into(),
                value: "Force".into(),
            }];
        let bad = command(&ca, "m-4", SOFTWARE, &bad.encode_to_vec());
        assert!(check(&Request::signed(&bad, Step::SoftwareMsi, None)).is_err());
    }

    #[test]
    fn winget_updates_take_their_apps_from_the_signed_command_once() {
        let ca = Ca::new();
        let host = Host::pinned(&ca);
        let check = |request: &Request| authorized(request, &host.place, &configured(), unix_ms());
        assert_eq!(check(&Request::WingetScan), Ok(Request::WingetScan));
        let bare = Request::WingetUpgrade {
            ids: names(&["Example.App"]),
            hold: vec![],
        };
        assert!(check(&bare).unwrap_err().contains("signed"));

        let both = command(&ca, "w-1", APPLY_UPDATES, &winget(&["Example.App"], false));
        let step = |step| Request::signed(&both, step, None);
        assert_eq!(check(&step(Step::Install)), Ok(install(Kind::All, &[])));
        assert_eq!(
            check(&step(Step::Winget)),
            Ok(Request::WingetUpgrade {
                ids: names(&["Example.App"]),
                hold: vec![],
            })
        );
        assert!(check(&step(Step::Winget))
            .unwrap_err()
            .contains("already run"));
        assert_eq!(check(&step(Step::Reboot)), Ok(Request::Reboot));

        // A command that skips the OS installs no Windows update, and
        // every pending app when it names none.
        let only = command(&ca, "w-2", APPLY_UPDATES, &winget(&[], true));
        let error = check(&Request::signed(&only, Step::Install, None)).unwrap_err();
        assert!(error.contains("skips Windows updates"), "{error}");
        assert_eq!(
            check(&Request::signed(&only, Step::Winget, None)),
            Ok(Request::WingetUpgrade {
                ids: vec![],
                hold: vec![]
            })
        );

        // An OS-only command selects no app; a bad ID or another
        // platform's manager is refused.
        let os = command(&ca, "w-3", APPLY_UPDATES, &apply(&[], false));
        let error = check(&Request::signed(&os, Step::Winget, None)).unwrap_err();
        assert!(error.contains("no winget apps"), "{error}");
        let hostile = command(&ca, "w-4", APPLY_UPDATES, &winget(&["a.b'; x"], true));
        assert!(check(&Request::signed(&hostile, Step::Winget, None)).is_err());
        let linux = ApplyUpdates {
            skip_os: true,
            third_party: vec![ThirdPartySelection {
                manager: PackageManager::Snap.into(),
                ..ThirdPartySelection::default()
            }],
            ..ApplyUpdates::default()
        };
        let linux = command(&ca, "w-5", APPLY_UPDATES, &linux.encode_to_vec());
        assert!(check(&Request::signed(&linux, Step::Winget, None)).is_err());
    }

    #[test]
    fn a_self_update_takes_its_version_and_downgrade_from_the_signed_command() {
        let ca = Ca::new();
        let host = Host::pinned(&ca);
        let spec = SelfUpdate {
            version: "0.1.0".into(),
            base_urls: Vec::new(),
            allow_downgrade: true,
            health_timeout_seconds: 60,
        };
        let check = |request: &Request| authorized(request, &host.place, &configured(), unix_ms());
        let expected = |id: &str| {
            Ok(Request::SelfUpdate(crate::self_update::swap::Request {
                command_id: id.into(),
                version: "0.1.0".into(),
                data_dir: configured(),
                allow_downgrade: true,
                health_timeout_ms: 60_000,
            }))
        };

        // The data directory is the service's, whatever the pipe says or
        // does not say: the agent's account cannot point LocalSystem at a
        // folder it controls or at another host.
        for (id, said) in [
            ("c-1", None),
            ("c-2", Some(std::path::PathBuf::from(r"\\attacker\share\x"))),
            ("c-3", Some(std::env::temp_dir().join("junction"))),
        ] {
            let signed = command(&ca, id, SELF_UPDATE, &spec.encode_to_vec());
            let request = Request::signed(&signed, Step::SelfUpdate, said.as_deref());
            assert_eq!(check(&request), expected(id), "{said:?}");
        }
        // A configured network path is refused.
        let signed = command(&ca, "c-4", SELF_UPDATE, &spec.encode_to_vec());
        let request = Request::signed(&signed, Step::SelfUpdate, None);
        let unc = std::path::Path::new(r"\\host\share\agent");
        let error = authorized(&request, &host.place, unc, unix_ms()).unwrap_err();
        assert!(error.contains("network"), "{error}");
    }

    #[test]
    fn signed_requests_keep_their_wire_format_both_ways() {
        // An older agent sends its data directory; a newer one too, for an
        // older service. Both still parse.
        let old = r#"{"op":"signed","command":"AA==","step":"self_update","data_dir":"C:\\ProgramData\\MonitorFlow\\agent"}"#;
        let request: Request = serde_json::from_str(old).unwrap();
        assert_eq!(serde_json::to_string(&request).unwrap(), old);
        let install = r#"{"op":"signed","command":"AA==","step":"install"}"#;
        let request: Request = serde_json::from_str(install).unwrap();
        assert_eq!(serde_json::to_string(&request).unwrap(), install);
    }
}
