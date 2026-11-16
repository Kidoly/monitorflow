//! The helper's Flatpak and Snap command lines, against fakes that log
//! them. Snaps named one by one need snapd's API, tested in
//! `tests/snapd.rs`.

use monitorflow_sensors::testing::write_executable;
use tempfile::TempDir;

use super::*;

fn ids(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|n| n.to_string()).collect()
}

#[test]
fn flatpak_command_lines() {
    assert_eq!(
        flatpak_update(&[]).join(" "),
        "update --system --noninteractive -y"
    );
    assert_eq!(
        flatpak_update(&ids(&[
            "app/org.example.App/x86_64/stable",
            "runtime/org.example.Platform/x86_64/24.08"
        ]))
        .join(" "),
        "update --system --noninteractive -y -- app/org.example.App/x86_64/stable \
         runtime/org.example.Platform/x86_64/24.08"
    );
}

#[test]
fn snap_command_lines() {
    assert_eq!(snap_refresh(&[]).join(" "), "refresh");
    assert_eq!(
        snap_refresh(&ids(&["example-app", "example-tool_test"])).join(" "),
        "refresh example-app example-tool_test"
    );
}

/// A fake `flatpak` listing one app and one runtime, logging the rest
/// and exiting with `code`; `snap` and its socket are absent.
fn fakes(code: i32) -> (TempDir, Tools) {
    let dir = TempDir::new().unwrap();
    let d = dir.path().display();
    let script = format!(
        "#!/bin/sh\ncase \"$1 $3\" in\n\
         'list --app') printf 'org.example.App\\tx86_64\\tstable\\tExample\\t1\\tabc\\texample\\n' ;;\n\
         'list --runtime') printf 'org.example.Platform\\tx86_64\\t1\\t\\t\\tdef\\texample\\n' ;;\n\
         *) echo \"$*\" >> '{d}/calls'; echo \"flatpak $*\"; exit {code} ;;\nesac\n"
    );
    write_executable(&dir.path().join("flatpak"), &script, 0o755);
    let tools = Tools {
        flatpak: dir.path().join("flatpak"),
        snap: dir.path().join("snap"),
        snapd_socket: dir.path().join("snapd.socket"),
        ..Tools::default()
    };
    (dir, tools)
}

fn calls(dir: &TempDir) -> Vec<String> {
    std::fs::read_to_string(dir.path().join("calls"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn flatpak_updates_everything_or_what_is_installed() {
    let (dir, tools) = fakes(0);
    let mut outcome = Outcome::default();
    run(PackageManager::Flatpak, &[], &tools, &mut outcome).await;
    assert!(outcome.upgraded);
    assert!(outcome.error.is_none());

    let mut outcome = Outcome::default();
    let named = ids(&[
        "app/org.example.App/x86_64/stable",
        "app/org.example.Gone/x86_64/stable",
        "runtime/org.example.Platform/x86_64/1",
    ]);
    run(PackageManager::Flatpak, &named, &tools, &mut outcome).await;
    assert_eq!(
        outcome.notes,
        [(
            0,
            "flatpak: not installed: app/org.example.Gone/x86_64/stable".to_owned()
        )]
    );
    assert_eq!(
        calls(&dir),
        [
            "update --system --noninteractive -y",
            "update --system --noninteractive -y -- app/org.example.App/x86_64/stable \
             runtime/org.example.Platform/x86_64/1",
        ]
    );
    assert_eq!(outcome.exit_code(), 0);
}

#[tokio::test]
async fn nothing_installed_runs_nothing() {
    let (dir, tools) = fakes(0);
    let mut outcome = Outcome::default();
    let named = ids(&["app/org.example.Gone/x86_64/stable"]);
    run(PackageManager::Flatpak, &named, &tools, &mut outcome).await;
    assert!(!outcome.upgraded);
    assert!(outcome.error.is_none());
    assert!(calls(&dir).is_empty());

    // snap is absent here: noted, not a failure.
    let mut outcome = Outcome::default();
    run(PackageManager::Snap, &[], &tools, &mut outcome).await;
    assert_eq!(outcome.notes, [(0, "snap: not installed".to_owned())]);
    assert!(outcome.error.is_none());
    assert!(outcome.runs.is_empty());
}

#[tokio::test]
async fn failures_bad_ids_and_other_managers() {
    let (_dir, tools) = fakes(1);
    let mut outcome = Outcome::default();
    run(PackageManager::Flatpak, &[], &tools, &mut outcome).await;
    assert_eq!(outcome.exit_code(), 1);
    assert!(outcome
        .error
        .unwrap()
        .to_string()
        .contains("update --system"));

    for (manager, bad) in [
        (PackageManager::Flatpak, "org.example.App/x86_64/stable"),
        (PackageManager::Flatpak, "--user"),
        (PackageManager::Flatpak, "/tmp/x.flatpakref"),
        (PackageManager::Snap, "app/org.example.App/x86_64/stable"),
        (PackageManager::Snap, "--devmode"),
        (PackageManager::Snap, "./x.snap"),
    ] {
        let mut outcome = Outcome::default();
        run(manager, &ids(&[bad]), &tools, &mut outcome).await;
        assert!(outcome.runs.is_empty(), "{bad}");
        let error = outcome.error.unwrap().to_string();
        assert!(error.starts_with("invalid "), "{error}");
    }

    let mut outcome = Outcome::default();
    run(PackageManager::Winget, &[], &tools, &mut outcome).await;
    assert!(outcome.runs.is_empty());
    assert_eq!(
        outcome.error.unwrap().to_string(),
        "winget updates are not available on this system"
    );
}
