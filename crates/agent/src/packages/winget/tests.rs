use super::*;

const SCAN: &[u8] = include_bytes!("fixtures/scan.json");

fn app(id: &str, available: &str) -> App {
    App {
        id: id.into(),
        name: format!("{id} app"),
        source: DEFAULT_SOURCE.into(),
        version: "1.0".into(),
        available: available.into(),
    }
}

fn ids(apps: &[&App]) -> Vec<String> {
    apps.iter().map(|a| a.id.clone()).collect()
}

fn strings(s: &[&str]) -> Vec<String> {
    s.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_scan_keeps_used_sources_and_valid_ids_pending_first() {
    let (apps, invalid) = parse_scan(SCAN, &Policy::default());
    assert_eq!(apps.state, Availability::Available);
    assert_eq!(apps.version, "1.29.380");
    assert_eq!(apps.error, "");
    // The store source is never used; a one-segment ID and a hostile one
    // fall outside the grammar.
    assert_eq!(invalid, 2);
    let listed: Vec<(&str, &str, &str)> = apps
        .apps
        .iter()
        .map(|a| (a.id.as_str(), a.version.as_str(), a.available.as_str()))
        .collect();
    assert_eq!(
        listed,
        [
            ("Example.Archiver", "23.01", "24.08"),
            ("Example.Internal", "5.2.0", "5.3.1"),
            ("Example.Editor", "Unknown", ""),
        ]
    );
    // Names keep their first line, without control characters.
    assert_eq!(apps.apps[2].name, "Example Editor");
    assert_eq!(apps.apps[1].source, "Internal-Source");
}

#[test]
fn policy_turns_off_the_community_source() {
    let policy = Policy {
        enable_default_source: Some(0),
        ..Policy::default()
    };
    assert_eq!(policy.excluded(), [STORE_SOURCE, DEFAULT_SOURCE]);
    let (apps, _) = parse_scan(SCAN, &policy);
    assert_eq!(
        apps.apps.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
        ["Example.Internal"]
    );
    // 1 and missing values leave it on.
    for value in [None, Some(1)] {
        let policy = Policy {
            enable_default_source: value,
            ..Policy::default()
        };
        assert_eq!(policy.excluded(), [STORE_SOURCE]);
    }
}

#[test]
fn policy_turns_winget_off() {
    assert!(!Policy::default().disabled());
    for policy in [
        Policy {
            enable_app_installer: Some(0),
            ..Policy::default()
        },
        Policy {
            enable_command_line: Some(0),
            ..Policy::default()
        },
    ] {
        assert!(policy.disabled(), "{policy:?}");
    }
    let on = Policy {
        enable_app_installer: Some(1),
        enable_command_line: Some(1),
        enable_default_source: Some(1),
    };
    assert!(!on.disabled());
}

#[test]
fn a_missing_module_or_a_failure_is_reported() {
    let (apps, _) = parse_scan(br#"{"missing":"module"}"#, &Policy::default());
    assert_eq!(apps, Apps::absent(NO_MODULE));
    let (apps, _) = parse_scan(
        b"{\"error\":\"source unreachable\\r\\nat line 4\"}",
        &Policy::default(),
    );
    assert_eq!(apps.state, Availability::Unusable);
    assert_eq!(apps.error, "source unreachable");
    let (apps, _) = parse_scan(b"Import-Module : not found", &Policy::default());
    assert_eq!(apps.state, Availability::Unusable);
    assert!(
        apps.error
            .starts_with("the winget scan wrote no valid JSON"),
        "{}",
        apps.error
    );
    let (apps, _) = parse_scan(b"", &Policy::default());
    assert_eq!(apps.state, Availability::Unusable);
}

#[test]
fn a_scan_is_capped_with_pending_apps_kept() {
    let mut json = String::from(r#"{"module":"1.0","apps":["#);
    for i in 0..MAX_APPS + 10 {
        let available = if i >= MAX_APPS { "2.0" } else { "" };
        json.push_str(&format!(
            r#"{{"id":"Example.App{i}","name":"","source":"winget","version":"1.0","available":"{available}"}},"#
        ));
    }
    json.pop();
    json.push_str("]}");
    let (apps, _) = parse_scan(json.as_bytes(), &Policy::default());
    assert_eq!(apps.apps.len(), MAX_APPS);
    assert_eq!(apps.apps.iter().filter(|a| a.pending()).count(), 10);
}

#[test]
fn upgrades_parse_their_results() {
    let archiver = app("Example.Archiver", "2.0");
    let parse = |json: &[u8]| parse_upgrade(&archiver, json);
    assert_eq!(
        parse(include_bytes!("fixtures/upgrade-ok.json")),
        Upgraded::Done(
            InstallResult {
                name: "Example.Archiver".into(),
                title: "Example.Archiver app".into(),
                result: Outcome::Succeeded,
                hresult: 0,
            },
            true
        )
    );
    // The extended error wins over the installer's code.
    let Upgraded::Done(in_use, false) = parse(include_bytes!("fixtures/upgrade-in-use.json"))
    else {
        panic!("not a failure");
    };
    assert_eq!(in_use.result, Outcome::Failed);
    assert_eq!(in_use.hresult as u32, 0x8A15_0101);
    assert_eq!(
        in_use.title,
        "Example.Archiver app: The application is currently running. Exit the application then try again."
    );
    let Upgraded::Done(installer, false) = parse(include_bytes!("fixtures/upgrade-installer.json"))
    else {
        panic!("not a failure");
    };
    assert_eq!(installer.hresult, 1603);
    assert_eq!(installer.title, "Example.Archiver app: InstallError");
    assert_eq!(
        parse(include_bytes!("fixtures/upgrade-none.json")),
        Upgraded::NotPending
    );
    let Upgraded::Done(error, _) = parse(include_bytes!("fixtures/upgrade-error.json")) else {
        panic!("not a failure");
    };
    assert_eq!(
        error.title,
        "Example.Archiver app: No package found matching input criteria."
    );
    let Upgraded::Done(garbage, _) = parse(b"\xEF\xBB\xBFnot json") else {
        panic!("not a failure");
    };
    assert_eq!(garbage.result, Outcome::Failed);
}

#[test]
fn selections_take_pending_apps() {
    let apps = [
        app("Example.Archiver", "2.0"),
        app("Example.Editor", ""),
        app("Example.Viewer", "3.1"),
    ];
    let (picked, not_pending, over) = select(&[], &apps);
    assert_eq!(ids(&picked), ["Example.Archiver", "Example.Viewer"]);
    assert!(not_pending.is_empty());
    assert!(over.is_empty());
    // Case does not matter; current, unknown and repeated IDs are not
    // updated.
    let (picked, not_pending, _) = select(
        &strings(&[
            "example.viewer",
            "Example.Editor",
            "Example.Missing",
            "Example.Viewer",
        ]),
        &apps,
    );
    assert_eq!(ids(&picked), ["Example.Viewer"]);
    assert_eq!(not_pending, ["Example.Editor", "Example.Missing"]);
}

#[test]
fn selections_stop_at_the_limit() {
    let apps: Vec<App> = (0..MAX_UPGRADES + 3)
        .map(|i| app(&format!("Example.App{i}"), "2.0"))
        .collect();
    let (picked, _, over) = select(&[], &apps);
    assert_eq!(picked.len(), MAX_UPGRADES);
    assert_eq!(
        ids(&over),
        ["Example.App100", "Example.App101", "Example.App102"]
    );
    let result = not_attempted(over[0]);
    assert_eq!(result.result, Outcome::NotStarted);
    assert_eq!(
        result.title,
        "Example.App100 app: not attempted, over 100 apps"
    );
    let result = timed_out(over[0], UPGRADE_TIMEOUT);
    assert!(!result.result.succeeded());
    assert_eq!(
        result.title,
        "Example.App100 app: timed out after 30 minutes"
    );
}

#[test]
fn scans_become_report_entries() {
    let (apps, _) = parse_scan(SCAN, &Policy::default());
    let (status, packages) = to_report(&apps);
    assert_eq!(status.manager, i32::from(PackageManager::Winget));
    assert_eq!(status.state, i32::from(State::Available));
    assert_eq!(status.version, "1.29.380");
    assert_eq!(packages.len(), 3);
    assert_eq!(
        packages[0],
        ThirdPartyPackage {
            manager: PackageManager::Winget.into(),
            id: "Example.Archiver".into(),
            name: "Example Archiver".into(),
            version: "23.01".into(),
            available_version: "24.08".into(),
            scope: PackageScope::Machine.into(),
            origin: "winget".into(),
        }
    );
    let (status, packages) = to_report(&Apps::absent(NO_POWERSHELL));
    assert_eq!(status.state, i32::from(State::Absent));
    assert_eq!(status.error, NO_POWERSHELL);
    assert!(packages.is_empty());
}

#[test]
fn apps_cross_the_pipe_unchanged() {
    let (apps, _) = parse_scan(SCAN, &Policy::default());
    let json = serde_json::to_string(&apps).unwrap();
    assert!(json.contains(r#""state":"available""#), "{json}");
    assert_eq!(serde_json::from_str::<Apps>(&json).unwrap(), apps);
    // Unknown fields are refused, as on the rest of the pipe.
    assert!(serde_json::from_str::<App>(
        r#"{"id":"a.b","name":"","source":"","version":"","available":"","x":1}"#
    )
    .is_err());
}

#[test]
fn scripts_take_no_value_but_from_the_environment() {
    for body in [SCAN_SCRIPT, UPGRADE_SCRIPT] {
        let script = script(body);
        assert!(script.starts_with("$ErrorActionPreference = 'Stop'"));
        assert!(script.contains(MODULE));
        // Every script answers with one JSON object, errors included.
        assert!(script.contains("@{ error = $_.Exception.Message }"));
        assert!(!script.contains('\u{2014}'));
    }
    assert!(UPGRADE_SCRIPT.contains(&format!("-Id $env:{ENV_ID}")));
    assert!(UPGRADE_SCRIPT.contains(&format!("-Source $env:{ENV_SOURCE}")));
    assert!(UPGRADE_SCRIPT.contains("-MatchOption Equals -Mode Silent"));
    for never in [
        "-Force",
        "-IncludeUnknown",
        "-AllowHashMismatch",
        "-Override",
        "-Custom",
    ] {
        assert!(!UPGRADE_SCRIPT.contains(never), "{never}");
    }
    assert!(SCAN_SCRIPT.contains(&format!("$env:{ENV_EXCLUDE}")));
}

#[test]
fn encoded_commands_are_utf16le_base64() {
    use base64::Engine;
    let encoded = encoded("Write-Output 'é'");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    let units: Vec<u16> = bytes
        .chunks(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    assert_eq!(String::from_utf16(&units).unwrap(), "Write-Output 'é'");
}

#[test]
fn paths_are_under_program_files() {
    let pf = Path::new("pf");
    let paths = Paths::under(pf);
    assert_eq!(paths.pwsh, pf.join("PowerShell").join("7").join("pwsh.exe"));
    assert_eq!(paths.modules, pf.join("PowerShell").join("Modules"));
    assert_eq!(
        paths.module(),
        pf.join("PowerShell").join("Modules").join(MODULE)
    );
}
