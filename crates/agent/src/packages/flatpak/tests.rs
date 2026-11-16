//! Flatpak's output on each supported distribution, from the fixtures in
//! `../fixtures/flatpak`, captured in containers with the agent's exact
//! command lines. The formats are the same from Flatpak 1.12 to 1.16.

use monitorflow_sensors::testing::write_executable;
use tempfile::TempDir;

use super::*;

const FIXTURES: [(&str, &str); 6] = [
    ("debian12", include_str!("../fixtures/flatpak/debian12.txt")),
    ("debian13", include_str!("../fixtures/flatpak/debian13.txt")),
    (
        "ubuntu2204",
        include_str!("../fixtures/flatpak/ubuntu2204.txt"),
    ),
    (
        "ubuntu2404",
        include_str!("../fixtures/flatpak/ubuntu2404.txt"),
    ),
    (
        "rockylinux9",
        include_str!("../fixtures/flatpak/rockylinux9.txt"),
    ),
    (
        "almalinux10",
        include_str!("../fixtures/flatpak/almalinux10.txt"),
    ),
];

/// The output of each command in a fixture, by the command's start.
fn section(fixture: &str, command: &str) -> String {
    let start = fixture
        .find(&format!("## {command}"))
        .unwrap_or_else(|| panic!("no `{command}` section"));
    let rest = &fixture[start..];
    let body = &rest[rest.find('\n').unwrap() + 1..];
    let end = body.find("\n## ").map_or(body.len(), |i| i + 1);
    body[..end].to_owned()
}

#[test]
fn every_distribution_lists_and_offers_the_same_way() {
    for (distro, fixture) in FIXTURES {
        let apps = parse_list(Kind::App, &section(fixture, "flatpak list --system --app"));
        let runtimes = parse_list(
            Kind::Runtime,
            &section(fixture, "flatpak list --system --runtime"),
        );
        let offers = parse_updates(&section(fixture, "flatpak remote-ls"));
        assert_eq!(apps.len(), 1, "{distro}");
        assert_eq!(runtimes.len(), 1, "{distro}");
        assert_eq!(offers.len(), 2, "{distro}");

        let app = &apps[0];
        assert_eq!(app.id, "app/org.example.App/x86_64/stable", "{distro}");
        assert_eq!(app.name, "Example", "{distro}");
        assert_eq!(app.version, "1.0.0", "{distro}");
        assert_eq!(app.origin, "example", "{distro}");
        assert_eq!(app.scope, PackageScope::Machine as i32);
        assert_eq!(app.manager, PackageManager::Flatpak as i32);

        // No declared version: the commit stands for it.
        let runtime = &runtimes[0];
        assert_eq!(
            runtime.id, "runtime/org.example.Platform/x86_64/1",
            "{distro}"
        );
        assert_eq!(runtime.version.len(), 12, "{distro}");

        for package in apps.iter().chain(&runtimes) {
            let offer = &offers[&key(&package.id)];
            assert!(offer.version.is_empty(), "{distro}");
            assert_eq!(offer.against(&package.version), offer.commit, "{distro}");
            assert_ne!(offer.commit, package.version, "{distro}");
        }
    }
}

#[test]
fn offered_versions_and_rebuilds() {
    let offer = |version: &str| Offer {
        version: version.into(),
        commit: "0123456789ab".into(),
    };
    assert_eq!(offer("2.0.0").against("1.0.0"), "2.0.0");
    // Same version, new commit: a rebuild, told apart by its commit.
    assert_eq!(offer("1.0.0").against("1.0.0"), "0123456789ab");
    assert_eq!(offer("").against("1.0.0"), "0123456789ab");
    let updates = parse_updates("org.example.App\tx86_64\tstable\t2.0.0\t0123456789ab\nshort\n");
    assert_eq!(updates.len(), 1);
}

#[test]
fn refs_outside_the_grammar_are_left_out() {
    let text = "org.example.App\tx86_64\tstable\tExample\t1\tabc\tremote\n\
        -y\tx86_64\tstable\t\t1\tabc\tremote\n\
        org.example.App\tx86_64\t--user\t\t1\tabc\tremote\n\
        org.example\tx86_64\tstable\t\t1\tabc\tremote\n\
        truncated\tline\n";
    let ids: Vec<_> = parse_list(Kind::App, text)
        .into_iter()
        .map(|p| p.id)
        .collect();
    assert_eq!(ids, ["app/org.example.App/x86_64/stable"]);
}

/// A fake `flatpak` answering from `fixture`, or failing `remote-ls`.
fn fake(fixture: &str, remote_fails: bool) -> (TempDir, Tools) {
    let dir = TempDir::new().unwrap();
    for (name, command) in [
        ("app", "flatpak list --system --app"),
        ("runtime", "flatpak list --system --runtime"),
        ("updates", "flatpak remote-ls"),
    ] {
        std::fs::write(dir.path().join(name), section(fixture, command)).unwrap();
    }
    let d = dir.path().display();
    let remote = if remote_fails {
        "echo 'error: Unable to load summary from remote example' >&2; exit 1".to_owned()
    } else {
        format!("cat '{d}/updates'")
    };
    let script = format!(
        "#!/bin/sh\necho \"$*\" >> '{d}/calls'\ncase \"$1 $3\" in\n\
         '--version ') echo 'Flatpak 1.14.10' ;;\n\
         'list --app') cat '{d}/app' ;;\n\
         'list --runtime') cat '{d}/runtime' ;;\n\
         remote-ls*) {remote} ;;\n\
         *) exit 2 ;;\nesac\n"
    );
    write_executable(&dir.path().join("flatpak"), &script, 0o755);
    let tools = Tools {
        flatpak: dir.path().join("flatpak"),
        ..Tools::default()
    };
    (dir, tools)
}

#[tokio::test]
async fn inventory_runs_three_reads() {
    let (dir, tools) = fake(FIXTURES[0].1, false);
    let flatpak = Flatpak::new(tools);
    assert!(flatpak.present());
    assert_eq!(flatpak.version().await.as_deref(), Some("1.14.10"));
    let packages = flatpak.inventory().await.unwrap();
    let pending: Vec<_> = packages
        .iter()
        .map(|p| (p.id.as_str(), p.available_version.len()))
        .collect();
    assert_eq!(
        pending,
        [
            ("app/org.example.App/x86_64/stable", 12),
            ("runtime/org.example.Platform/x86_64/1", 12)
        ]
    );
    let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
    assert_eq!(
        calls.lines().skip(1).collect::<Vec<_>>(),
        [
            format!("list --system --app {LIST_COLUMNS}"),
            format!("list --system --runtime {LIST_COLUMNS}"),
            format!("remote-ls --updates --system {UPDATE_COLUMNS}"),
        ]
    );
}

#[tokio::test]
async fn an_unreachable_remote_fails_the_inventory() {
    let (_dir, tools) = fake(FIXTURES[0].1, true);
    let error = Flatpak::new(tools).inventory().await.unwrap_err();
    assert!(
        error.to_string().contains("Unable to load summary"),
        "{error}"
    );
}
