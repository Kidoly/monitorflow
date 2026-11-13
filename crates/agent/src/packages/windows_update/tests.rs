use super::*;

fn names(n: &[&str]) -> Vec<String> {
    n.iter().map(|s| s.to_string()).collect()
}

fn install(kind: Kind, updates: &[&str]) -> Request {
    Request::Install {
        kind,
        updates: names(updates),
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
    };
    assert_eq!(many.validate(), Err(RequestError::TooManyNames));
}

#[test]
fn apply_updates_maps_to_installs() {
    let apply = |packages: &[&str], security_only| ApplyUpdates {
        packages: names(packages),
        security_only,
        allow_reboot: false,
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
    let pick = |kind, n: &[&str]| select(kind, &names(n), &updates);
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
    use monitorflow_common::proto::v1::SelfUpdate;

    use super::*;
    use crate::helper_trust::tests::{unsigned, Ca, Host};
    use crate::self_update::unix_ms;

    fn apply(packages: &[&str], allow_reboot: bool) -> Vec<u8> {
        ApplyUpdates {
            packages: names(packages),
            security_only: false,
            allow_reboot,
        }
        .encode_to_vec()
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
        let check = |request: &Request| authorized(request, &host.place, unix_ms());
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
        let check = |request: &Request| authorized(request, &host.place, unix_ms());

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
        let signed = command(&ca, "c-1", SELF_UPDATE, &spec.encode_to_vec());
        let data_dir = std::env::temp_dir();
        let check = |request: &Request| authorized(request, &host.place, unix_ms());

        let missing = Request::signed(&signed, Step::SelfUpdate, None);
        assert!(check(&missing).is_err());
        let signed = command(&ca, "c-2", SELF_UPDATE, &spec.encode_to_vec());
        let request = Request::signed(&signed, Step::SelfUpdate, Some(&data_dir));
        assert_eq!(
            check(&request),
            Ok(Request::SelfUpdate(crate::self_update::swap::Request {
                command_id: "c-2".into(),
                version: "0.1.0".into(),
                data_dir,
                allow_downgrade: true,
                health_timeout_ms: 60_000,
            }))
        );
    }
}
