//! Ubuntu through `do-release-upgrade` and the RHEL family through leapp
//! (ADR-0182): the same checks, state and unit as Debian, their own steps.

use std::fs;
use std::io;
use std::path::PathBuf;

use monitorflow_common::major_upgrade::{self, Method, Phase, Upgrade, FAILED, OK, WARNING};

use super::{healthy, host, Fake};
use crate::major_upgrade::checks::{self, parse_offered, parse_prompt, parse_versionlock, Probe};
use crate::major_upgrade::helper::{self, inhibitors, Exec, Paths};
use crate::major_upgrade::sources;
use crate::major_upgrade::state::{self, State};

fn ubuntu() -> &'static Upgrade {
    major_upgrade::next("ubuntu 22.04").unwrap()
}

fn rocky() -> &'static Upgrade {
    major_upgrade::next("rocky 8.10").unwrap()
}

/// Ubuntu 22.04's sources as installed, a PPA and a vendor's deb822 file.
const JAMMY: &str = "\
deb http://archive.ubuntu.com/ubuntu/ jammy main restricted
deb http://archive.ubuntu.com/ubuntu/ jammy-updates main restricted universe multiverse
deb http://security.ubuntu.com/ubuntu/ jammy-security main restricted
# deb http://archive.ubuntu.com/ubuntu/ jammy-backports main
deb https://ppa.launchpadcontent.net/ondrej/php/ubuntu/ jammy main
";
const VENDOR: &str =
    "Types: deb\nURIs: https://packages.example.invalid/apt\nSuites: jammy\nComponents: main\n";

const LEAPP_JSON: &str = include_str!("../fixtures/leapp-report.json");
const LEAPP_TXT: &str = include_str!("../fixtures/leapp-report.txt");

/// A healthy host that offers `offered` with `Prompt=lts`, and has
/// leapp's packages but `missing`.
struct Other {
    base: Fake,
    offered: Result<Option<String>, String>,
    prompt: Option<String>,
    missing: Vec<String>,
}

impl Probe for Other {
    fn held(&self) -> Result<Vec<String>, String> {
        self.base.held()
    }
    fn audit(&self) -> Result<String, String> {
        self.base.audit()
    }
    fn free(&self, path: &str) -> Result<(u64, u64), String> {
        self.base.free(path)
    }
    fn reboot_pending(&self) -> bool {
        self.base.reboot_pending()
    }
    fn offered(&self) -> Result<Option<String>, String> {
        self.offered.clone()
    }
    fn prompt(&self) -> Option<String> {
        self.prompt.clone()
    }
    fn missing(&self, packages: &[String]) -> Vec<String> {
        packages
            .iter()
            .filter(|p| self.missing.contains(p))
            .cloned()
            .collect()
    }
}

fn other() -> Other {
    Other {
        base: healthy(),
        offered: Ok(Some("24.04.1".into())),
        prompt: Some("lts".into()),
        missing: vec![],
    }
}

#[test]
fn ubuntu_sources_are_read_and_never_rewritten_here() {
    let root = tempfile::tempdir().unwrap();
    host(
        root.path(),
        &[
            ("/etc/apt/sources.list", JAMMY),
            ("/etc/apt/sources.list.d/vendor.sources", VENDOR),
        ],
    );
    let plan = sources::plan(root.path(), ubuntu());
    assert_eq!(plan.refusal, None);
    assert_eq!(plan.release_entries, 3);
    assert_eq!(plan.third_party.len(), 2, "{:?}", plan.third_party);
    assert!(plan.third_party[0].contains("ppa.launchpadcontent.net"));
    // Ubuntu has no non-free-firmware to add.
    let rewritten = sources::rewrite_classic(JAMMY, ubuntu()).text;
    assert!(rewritten.contains("noble-updates main restricted universe multiverse\n"));

    // On the MonitorFlow mirrors, do-release-upgrade would switch them off.
    host(
        root.path(),
        &[(
            "/etc/apt/sources.list.d/monitorflow.sources",
            "Types: deb\nURIs: https://central.example.invalid/apt/ubuntu-22.04/current/\nSuites: jammy\nComponents: main\n",
        )],
    );
    let mirrored = sources::plan(root.path(), ubuntu());
    assert_eq!(mirrored.refusal, None);
    assert!(mirrored.mirrored);
    assert!(sources::blocker(&mirrored, Method::ReleaseUpgrade)
        .unwrap()
        .contains("MonitorFlow mirrors, which do-release-upgrade does not use"));
    // The campaign puts the server's own sources back first: a warning.
    let checks = checks::evaluate(&other(), ubuntu(), 0, &mirrored);
    let check = checks.iter().find(|c| c.name == "sources").unwrap();
    assert_eq!(check.state, WARNING);
    assert!(
        check
            .detail
            .contains("the campaign puts its own sources back before the upgrade"),
        "{}",
        check.detail
    );
    assert_eq!(checks::first_failure(&checks), None);
}

#[test]
fn ubuntu_on_the_mirrors_is_refused_before_do_release_upgrade_runs() {
    let (_dir, paths) = prepared(
        ubuntu(),
        &[(
            "/etc/apt/sources.list.d/monitorflow.sources",
            "Types: deb\nURIs: https://central.example.invalid/apt/ubuntu-22.04/current/\nSuites: jammy\nComponents: main\n",
        )],
    );
    let mut tool = Tool {
        root: paths.root.clone(),
        ran: vec![],
        on: vec![],
    };
    let state = helper::run(&paths, &mut tool, || 5).unwrap();
    assert_eq!(state.phase, Phase::Failed);
    assert!(
        state
            .error
            .contains("a campaign puts its own sources back first"),
        "{}",
        state.error
    );
    assert!(tool.ran.is_empty());
}

#[test]
fn ubuntu_checks_need_the_next_lts_offered() {
    let root = tempfile::tempdir().unwrap();
    host(root.path(), &[("/etc/apt/sources.list", JAMMY)]);
    let plan = sources::plan(root.path(), ubuntu());
    let checks = checks::evaluate(&other(), ubuntu(), 0, &plan);
    let names: Vec<&str> = checks.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, major_upgrade::checks(Method::ReleaseUpgrade));
    let states: Vec<&str> = checks.iter().map(|c| c.state.as_str()).collect();
    assert_eq!(states, [OK, OK, OK, OK, OK, OK, OK, WARNING]);
    assert_eq!(checks[6].detail, "do-release-upgrade offers 24.04.1");

    let release_offered = |probe: Other| {
        let checks = checks::evaluate(&probe, ubuntu(), 0, &plan);
        let check = checks
            .into_iter()
            .find(|c| c.name == "release_offered")
            .unwrap();
        assert_eq!(check.state, FAILED);
        check.detail
    };
    assert_eq!(
        release_offered(Other {
            offered: Ok(None),
            ..other()
        }),
        "Ubuntu 24.04 (noble) does not offer the upgrade yet: Ubuntu offers it after its first point release"
    );
    assert_eq!(
        release_offered(Other {
            offered: Ok(Some("23.10".into())),
            ..other()
        }),
        "do-release-upgrade offers 23.10, not 24.04"
    );
    assert!(release_offered(Other {
        prompt: Some("normal".into()),
        ..other()
    })
    .starts_with("Prompt is normal in /etc/update-manager/release-upgrades; set it to lts"));
    assert_eq!(
        release_offered(Other {
            offered: Err("no network".into()),
            ..other()
        }),
        "no network"
    );
}

#[test]
fn do_release_upgrade_output_and_settings_are_read() {
    let text = "Checking for a new Ubuntu release\nNew release '24.04.5 LTS' available.\nRun 'do-release-upgrade' to upgrade to it.\n";
    assert_eq!(parse_offered(text).as_deref(), Some("24.04.5"));
    assert_eq!(
        parse_offered("Checking for a new Ubuntu release\nNo new release found.\n"),
        None
    );
    let settings = "# Default behavior\n[DEFAULT]\n#Prompt=normal\nPrompt=lts\n";
    assert_eq!(parse_prompt(settings).as_deref(), Some("lts"));
    assert_eq!(parse_prompt("[DEFAULT]\n"), None);
    assert_eq!(
        parse_versionlock("# locks\n\nkernel-0:4.18.0-553.el8.*\n"),
        ["kernel-0:4.18.0-553.el8.*"]
    );
}

#[test]
fn leapp_checks_need_leapp_and_the_elevate_data() {
    let root = tempfile::tempdir().unwrap();
    host(
        root.path(),
        &[(
            "/etc/yum.repos.d/rocky.repo",
            "[baseos]\nmirrorlist=https://mirrors.rockylinux.org/mirrorlist?repo=BaseOS-8\n",
        )],
    );
    let plan = sources::plan(root.path(), rocky());
    let checks = checks::evaluate(&other(), rocky(), 0, &plan);
    let names: Vec<&str> = checks.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, major_upgrade::checks(Method::Leapp));
    assert!(checks.iter().all(|c| c.state == OK), "{checks:?}");
    assert_eq!(
        checks[5].detail,
        "leapp takes the repositories of Rocky Linux 9 from its own data"
    );

    let probe = Other {
        missing: vec!["leapp-data-rocky".into()],
        ..other()
    };
    let checks = checks::evaluate(&probe, rocky(), 0, &plan);
    let leapp = &checks[6];
    assert_eq!(
        (leapp.name.as_str(), leapp.state.as_str()),
        ("leapp", FAILED)
    );
    assert_eq!(leapp.items, ["leapp-data-rocky"]);
    assert!(
        leapp
            .detail
            .contains("elevate-release-latest-el8.noarch.rpm"),
        "{}",
        leapp.detail
    );
    assert_eq!(
        checks::leapp_packages(major_upgrade::next("rhel 9.4").unwrap()),
        ["leapp-upgrade-el9toel10"],
        "RHEL takes no ELevate data"
    );
}

/// A program to react to: the words its line contains, its exit code and
/// the files it writes.
type Reaction = (&'static str, i32, Vec<(&'static str, String)>);

/// Records the programs run; `on` writes files under the root when a
/// program runs, as the real one would, and gives its exit code.
struct Tool {
    root: PathBuf,
    ran: Vec<String>,
    on: Vec<Reaction>,
}

impl Exec for Tool {
    fn run(
        &mut self,
        program: &str,
        args: &[&str],
        _env: &[(&str, &str)],
    ) -> io::Result<(i32, String)> {
        let line = format!("{program} {}", args.join(" "));
        self.ran.push(line.clone());
        for (needle, code, files) in &self.on {
            if line.contains(needle) {
                for (path, text) in files {
                    let disk = self.root.join(path);
                    fs::create_dir_all(disk.parent().unwrap()).unwrap();
                    fs::write(disk, text).unwrap();
                }
                return Ok((*code, "last line\n".into()));
            }
        }
        Ok((0, String::new()))
    }
    fn note(&mut self, _line: &str) {}
}

fn prepared(upgrade: &Upgrade, files: &[(&str, &str)]) -> (tempfile::TempDir, Paths) {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths {
        root: dir.path().join("root"),
        state_dir: dir.path().join("state"),
    };
    host(&paths.root, files);
    state::prepare(&paths.state_dir).unwrap();
    let s = State::new(
        "c1",
        &upgrade.from_release(),
        &upgrade.to_release(),
        "boot",
        1,
    );
    state::write(&paths.state_dir, &s).unwrap();
    (dir, paths)
}

const NOBLE: &str = "ID=ubuntu\nVERSION_ID=\"24.04\"\n";

#[test]
fn ubuntu_runs_do_release_upgrade_then_reboots() {
    let (_dir, paths) = prepared(ubuntu(), &[("/etc/apt/sources.list", JAMMY)]);
    let mut tool = Tool {
        root: paths.root.clone(),
        ran: vec![],
        on: vec![(
            "do-release-upgrade",
            0,
            vec![("etc/os-release", NOBLE.into())],
        )],
    };
    let state = helper::run(&paths, &mut tool, || 5).unwrap();
    assert_eq!(state.phase, Phase::Rebooting, "{}", state.error);
    assert_eq!(
        tool.ran,
        [
            "/usr/bin/do-release-upgrade -f DistUpgradeViewNonInteractive",
            "/usr/bin/systemctl reboot"
        ]
    );
    assert_eq!(state.disabled_sources.len(), 1, "the PPA");
    // MonitorFlow left the sources to do-release-upgrade.
    let sources = fs::read_to_string(paths.root.join("etc/apt/sources.list")).unwrap();
    assert_eq!(sources, JAMMY);
}

#[test]
fn ubuntu_still_on_the_old_release_fails_without_a_reboot() {
    let jammy = "ID=ubuntu\nVERSION_ID=\"22.04\"\n";
    let (_dir, paths) = prepared(
        ubuntu(),
        &[("/etc/apt/sources.list", JAMMY), ("/etc/os-release", jammy)],
    );
    let mut tool = Tool {
        root: paths.root.clone(),
        ran: vec![],
        on: vec![],
    };
    let state = helper::run(&paths, &mut tool, || 5).unwrap();
    assert_eq!(state.phase, Phase::Failed);
    assert!(
        state.error.starts_with(
            "do-release-upgrade finished but the server still runs ubuntu 22.04, not ubuntu 24.04"
        ),
        "{}",
        state.error
    );
    assert!(!tool.ran.iter().any(|l| l.contains("reboot")));

    let (_dir, paths) = prepared(ubuntu(), &[("/etc/apt/sources.list", JAMMY)]);
    let mut tool = Tool {
        root: paths.root.clone(),
        ran: vec![],
        on: vec![("do-release-upgrade", 1, vec![])],
    };
    let state = helper::run(&paths, &mut tool, || 5).unwrap();
    assert!(state.error.starts_with(
        "`do-release-upgrade` exited with 1: last line; its logs are in /var/log/dist-upgrade/"
    ));
}

const ROCKY_REPO: &str =
    "[baseos]\nmirrorlist=https://mirrors.rockylinux.org/mirrorlist?repo=BaseOS-8\n";
const CLEAN: &str = "{\"entries\": [{\"title\": \"Packages not signed\", \"flags\": [], \"groups\": [\"sanity\"]}]}";

#[test]
fn an_inhibitor_stops_leapp_and_its_report_is_kept_as_it_is() {
    let (_dir, paths) = prepared(rocky(), &[("/etc/yum.repos.d/rocky.repo", ROCKY_REPO)]);
    let mut tool = Tool {
        root: paths.root.clone(),
        ran: vec![],
        on: vec![(
            "preupgrade",
            1,
            vec![
                ("var/log/leapp/leapp-report.json", LEAPP_JSON.into()),
                ("var/log/leapp/leapp-report.txt", LEAPP_TXT.into()),
            ],
        )],
    };
    let state = helper::run(&paths, &mut tool, || 5).unwrap();
    assert_eq!(state.phase, Phase::Failed);
    assert_eq!(
        state.error,
        "`leapp preupgrade` found 1 inhibitors: File \"/etc/default/grub\" does not exist!; nothing was changed, see leapp's report"
    );
    assert_eq!(state.report, LEAPP_TXT);
    assert_eq!(state.to_progress().report, LEAPP_TXT);
    assert_eq!(tool.ran, ["/usr/bin/leapp preupgrade"]);
}

#[test]
fn leapp_upgrades_then_reboots_into_its_environment() {
    let (_dir, paths) = prepared(rocky(), &[("/etc/yum.repos.d/rocky.repo", ROCKY_REPO)]);
    let report = vec![
        ("var/log/leapp/leapp-report.json", CLEAN.to_owned()),
        (
            "var/log/leapp/leapp-report.txt",
            "Risk Factor: info\n".to_owned(),
        ),
    ];
    let mut tool = Tool {
        root: paths.root.clone(),
        ran: vec![],
        on: vec![("preupgrade", 0, report.clone()), ("upgrade", 0, report)],
    };
    let state = helper::run(&paths, &mut tool, || 5).unwrap();
    assert_eq!(state.phase, Phase::Rebooting, "{}", state.error);
    assert_eq!(
        tool.ran,
        [
            "/usr/bin/leapp preupgrade",
            "/usr/bin/leapp upgrade",
            "/usr/bin/systemctl reboot"
        ]
    );
    assert_eq!(state.report, "Risk Factor: info\n");

    // A failed `leapp upgrade` leaves the old release running.
    let (_dir, paths) = prepared(rocky(), &[("/etc/yum.repos.d/rocky.repo", ROCKY_REPO)]);
    let mut tool = Tool {
        root: paths.root.clone(),
        ran: vec![],
        on: vec![("preupgrade", 0, vec![]), ("upgrade", 1, vec![])],
    };
    let state = helper::run(&paths, &mut tool, || 5).unwrap();
    assert_eq!(
        state.error,
        "`leapp upgrade` exited with 1: last line; the server still runs Rocky Linux 8, see leapp's report"
    );
}

#[test]
fn a_leapp_server_on_the_mirrors_is_refused_before_leapp_runs() {
    let (_dir, paths) = prepared(
        rocky(),
        &[(
            "/etc/yum.repos.d/monitorflow-baseos.repo",
            "[monitorflow-baseos]\nbaseurl=https://central.example.invalid/dnf/rocky-8/current/\n",
        )],
    );
    let mut tool = Tool {
        root: paths.root.clone(),
        ran: vec![],
        on: vec![],
    };
    let state = helper::run(&paths, &mut tool, || 5).unwrap();
    assert_eq!(state.phase, Phase::Failed);
    assert!(tool.ran.is_empty());
}

/// A state of `leapp_preupgrade` for `upgrade`, as the helper writes it.
fn preupgrade(upgrade: &Upgrade, files: &[(&str, &str)]) -> (tempfile::TempDir, Paths) {
    let (dir, paths) = prepared(upgrade, files);
    let mut s = state::read(&paths.state_dir).unwrap().unwrap();
    s.preupgrade_only = true;
    state::write(&paths.state_dir, &s).unwrap();
    (dir, paths)
}

#[test]
fn leapp_preupgrade_alone_keeps_the_report_and_never_upgrades() {
    let report = vec![
        ("var/log/leapp/leapp-report.json", CLEAN.to_owned()),
        (
            "var/log/leapp/leapp-report.txt",
            "Risk Factor: info\n".to_owned(),
        ),
    ];
    // On the mirrors too: it changes nothing, so it does not care.
    let (_dir, paths) = preupgrade(
        rocky(),
        &[(
            "/etc/yum.repos.d/monitorflow-baseos.repo",
            "[monitorflow-baseos]\nbaseurl=https://central.example.invalid/dnf/rocky-8/current/\n",
        )],
    );
    let mut tool = Tool {
        root: paths.root.clone(),
        ran: vec![],
        on: vec![("preupgrade", 0, report)],
    };
    let state = helper::run(&paths, &mut tool, || 5).unwrap();
    assert_eq!(state.phase, Phase::Done, "{}", state.error);
    assert_eq!(tool.ran, ["/usr/bin/leapp preupgrade"]);
    assert_eq!(state.report, "Risk Factor: info\n");
    assert!(state.to_progress().preupgrade_only);
    // Its final state stays done after a reboot, on the old release.
    assert_eq!(
        state.effective("other-boot", "rocky 8.10").phase,
        Phase::Done
    );

    // An inhibitor fails it, with the report.
    let (_dir, paths) = preupgrade(rocky(), &[("/etc/yum.repos.d/rocky.repo", ROCKY_REPO)]);
    let mut tool = Tool {
        root: paths.root.clone(),
        ran: vec![],
        on: vec![(
            "preupgrade",
            1,
            vec![
                ("var/log/leapp/leapp-report.json", LEAPP_JSON.into()),
                ("var/log/leapp/leapp-report.txt", LEAPP_TXT.into()),
            ],
        )],
    };
    let state = helper::run(&paths, &mut tool, || 5).unwrap();
    assert_eq!(state.phase, Phase::Failed);
    assert!(state.error.contains("1 inhibitors"), "{}", state.error);
    assert_eq!(state.report, LEAPP_TXT);
    assert_eq!(tool.ran, ["/usr/bin/leapp preupgrade"]);

    // Never outside leapp.
    let (_dir, paths) = preupgrade(ubuntu(), &[("/etc/apt/sources.list", JAMMY)]);
    let mut tool = Tool {
        root: paths.root.clone(),
        ran: vec![],
        on: vec![],
    };
    let state = helper::run(&paths, &mut tool, || 5).unwrap();
    assert_eq!(state.phase, Phase::Failed);
    assert!(tool.ran.is_empty());
}

#[test]
fn leapp_preupgrade_needs_leapp_and_no_upgrade_running() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let missing = Other {
        missing: vec!["leapp-data-rocky".into()],
        ..other()
    };
    let error = helper::start_preupgrade("c2", rocky(), &missing, &state_dir, 5).unwrap_err();
    assert!(
        error.starts_with("leapp: install leapp and the ELevate data first"),
        "{error}"
    );
    let error = helper::start_preupgrade("c2", ubuntu(), &other(), &state_dir, 5).unwrap_err();
    assert_eq!(
        error,
        "leapp preupgrade is for the RHEL family, not Ubuntu 22.04 (jammy)"
    );

    let mut running = State::new("c1", "rocky 8", "rocky 9", &state::boot_id(), 1);
    running.phase = Phase::Upgrading;
    state::write(&state_dir, &running).unwrap();
    let error = helper::start_preupgrade("c2", rocky(), &other(), &state_dir, 5).unwrap_err();
    assert_eq!(
        error,
        "an upgrade to rocky 9 is already running (upgrading)"
    );
}

#[test]
fn inhibitors_are_flagged_or_grouped() {
    assert_eq!(
        inhibitors(LEAPP_JSON),
        ["File \"/etc/default/grub\" does not exist!"]
    );
    let grouped =
        "{\"entries\": [{\"title\": \"Old kernel\", \"groups\": [\"inhibitor\", \"kernel\"]}]}";
    assert_eq!(inhibitors(grouped), ["Old kernel"]);
    assert!(inhibitors(CLEAN).is_empty());
    assert!(inhibitors("not json").is_empty());
}

#[test]
fn payloads_of_the_other_distributions() {
    use monitorflow_common::proto::v1::MajorUpgrade;
    use prost::Message;
    let payload = |from: &str, to: &str| {
        MajorUpgrade {
            from: from.into(),
            to: to.into(),
        }
        .encode_to_vec()
    };
    assert!(helper::requested(&payload("ubuntu 22.04", "ubuntu 24.04"), "ubuntu 22.04").is_ok());
    assert!(helper::requested(&payload("ubuntu 22.04", "ubuntu 26.04"), "ubuntu 22.04").is_err());
    assert!(helper::requested(&payload("almalinux 9", "almalinux 10"), "almalinux 9.5").is_ok());
    assert!(helper::requested(&payload("rocky 9", "rocky 10"), "rocky 9.4").is_err());
}

#[test]
fn a_leapp_reboot_into_the_target_is_done() {
    let mut s = State::new("c1", "rocky 8", "rocky 9", "boot-a", 1);
    s.phase = Phase::Rebooting;
    assert_eq!(s.effective("boot-b", "rocky 9.6").phase, Phase::Done);
    s.phase = Phase::Preupgrade;
    assert_eq!(
        s.effective("boot-b", "rocky 8.10").error,
        "interrupted by a restart while checking with leapp"
    );
}
