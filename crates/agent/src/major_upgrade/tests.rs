use std::cell::Cell;
use std::fs;
use std::io;
use std::path::Path;

use monitorflow_common::major_upgrade::{self, Phase, FAILED, OK, WARNING};

use super::checks::{self, Probe};
use super::helper::{self, Exec, Paths};
use super::sources::{self, rewrite_classic, rewrite_deb822};
use super::state::{self, State};

fn debian() -> &'static major_upgrade::Upgrade {
    major_upgrade::next("debian 12").unwrap()
}

const CLASSIC: &str = "\
# See /usr/share/doc/apt/README.Debian.
deb http://deb.debian.org/debian bookworm main contrib non-free non-free-firmware
deb-src http://deb.debian.org/debian bookworm main

deb http://security.debian.org/debian-security bookworm-security main non-free
deb [arch=amd64 signed-by=/usr/share/keyrings/x.gpg] http://ftp.fr.debian.org/debian/ bookworm-updates main # updates
#deb http://deb.debian.org/debian bookworm-backports main
deb http://deb.debian.org/debian bookworm-backports main
deb http://deb.debian.org/debian stable main
deb [arch=amd64 signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/debian bookworm stable
deb http://apt.example.com/repo bookworm main
deb cdrom:[Debian GNU/Linux 12.5.0 _Bookworm_]/ bookworm main
";

#[test]
fn classic_entries_of_the_release_move_and_others_are_switched_off() {
    let out = rewrite_classic(CLASSIC, debian());
    assert_eq!(
        out.text,
        "\
# See /usr/share/doc/apt/README.Debian.
deb http://deb.debian.org/debian trixie main contrib non-free non-free-firmware
deb-src http://deb.debian.org/debian trixie main

deb http://security.debian.org/debian-security trixie-security main non-free non-free-firmware
deb [arch=amd64 signed-by=/usr/share/keyrings/x.gpg] http://ftp.fr.debian.org/debian/ trixie-updates main # updates
#deb http://deb.debian.org/debian bookworm-backports main
deb http://deb.debian.org/debian trixie-backports main
deb http://deb.debian.org/debian stable main
# Disabled by MonitorFlow for the upgrade to Debian 13 (trixie):
# deb [arch=amd64 signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/debian bookworm stable
# Disabled by MonitorFlow for the upgrade to Debian 13 (trixie):
# deb http://apt.example.com/repo bookworm main
# Disabled by MonitorFlow for the upgrade to Debian 13 (trixie):
# deb cdrom:[Debian GNU/Linux 12.5.0 _Bookworm_]/ bookworm main
"
    );
    assert_eq!(out.release_entries, 5);
    assert_eq!(out.third_party.len(), 3);
    assert!(out.third_party[0].contains("download.docker.com"));
    // Applying it again changes nothing more.
    let again = rewrite_classic(&out.text, debian());
    assert_eq!(again.text, out.text);
    assert_eq!(again.release_entries, 0);
}

#[test]
fn a_local_mirror_or_cache_of_debian_is_the_release() {
    let text = "deb http://cache.lan:3142/deb.debian.org/debian bookworm main\n\
                deb https://mirror.example.org/debian-security bookworm-security main\n";
    let out = rewrite_classic(text, debian());
    assert_eq!(out.release_entries, 2);
    assert!(out.third_party.is_empty());
    assert!(out.text.contains("/debian trixie main"));
    assert!(out.text.contains("trixie-security"));
}

const DEB822: &str = "\
Types: deb deb-src
URIs: https://deb.debian.org/debian
Suites: bookworm bookworm-updates
Components: main non-free
Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg

# Security
Types: deb
URIs: https://security.debian.org/debian-security
Suites: bookworm-security
Components: main
Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg

Types: deb
URIs: https://packages.example.com/apt
Suites: bookworm
Components: main
Enabled: yes
Signed-By:
 -----BEGIN PGP PUBLIC KEY BLOCK-----
 .
 xxx
 -----END PGP PUBLIC KEY BLOCK-----

Types: deb
URIs: https://other.example.com/apt
Suites: bookworm
Components: main
Enabled: no
";

#[test]
fn deb822_stanzas_move_or_are_switched_off() {
    let out = rewrite_deb822(DEB822, debian(), false);
    assert_eq!(
        out.text,
        "\
Types: deb deb-src
URIs: https://deb.debian.org/debian
Suites: trixie trixie-updates
Components: main non-free non-free-firmware
Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg

# Security
Types: deb
URIs: https://security.debian.org/debian-security
Suites: trixie-security
Components: main
Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg

# Disabled by MonitorFlow for the upgrade to Debian 13 (trixie):
Types: deb
URIs: https://packages.example.com/apt
Suites: bookworm
Components: main
Signed-By:
 -----BEGIN PGP PUBLIC KEY BLOCK-----
 .
 xxx
 -----END PGP PUBLIC KEY BLOCK-----
Enabled: no

Types: deb
URIs: https://other.example.com/apt
Suites: bookworm
Components: main
Enabled: no
"
    );
    assert_eq!(out.release_entries, 2);
    assert_eq!(
        out.third_party,
        vec!["https://packages.example.com/apt bookworm".to_owned()]
    );
    let again = rewrite_deb822(&out.text, debian(), false);
    assert_eq!(again.text, out.text);
}

#[test]
fn the_monitorflow_mirror_file_moves_to_the_new_mirrors() {
    let text = "\
# Managed by MonitorFlow (ADR-0032).

Types: deb
URIs: http://central:7480/apt/debian-12/current/
Suites: bookworm
Components: main contrib
Signed-By: /etc/apt/keyrings/monitorflow-mirror.asc

Types: deb
URIs: http://central:7480/apt/debian-12-security/current/
Suites: bookworm-security
Components: main
Signed-By: /etc/apt/keyrings/monitorflow-mirror.asc
";
    let out = rewrite_deb822(text, debian(), true);
    assert!(out.pinned.is_empty());
    assert!(out
        .text
        .contains("URIs: http://central:7480/apt/debian-13/current/\nSuites: trixie\n"));
    assert!(out.text.contains(
        "URIs: http://central:7480/apt/debian-13-security/current/\nSuites: trixie-security\n"
    ));
    let pinned = text.replace("debian-12/current", "debian-12/20261101");
    assert_eq!(
        rewrite_deb822(&pinned, debian(), true).pinned,
        vec!["http://central:7480/apt/debian-12/20261101/ (20261101)".to_owned()]
    );
}

fn host(root: &Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let disk = root.join(path.trim_start_matches('/'));
        fs::create_dir_all(disk.parent().unwrap()).unwrap();
        fs::write(disk, text).unwrap();
    }
}

#[test]
fn a_plan_reads_both_formats_and_refuses_without_a_debian_source() {
    let root = tempfile::tempdir().unwrap();
    host(
        root.path(),
        &[
            (
                "/etc/apt/sources.list",
                "deb http://apt.example.com/x bookworm main\n",
            ),
            ("/etc/apt/sources.list.d/debian.sources", DEB822),
            (
                "/etc/apt/sources.list.d/notes.txt",
                "deb http://x bookworm main\n",
            ),
        ],
    );
    let plan = sources::plan(root.path(), debian());
    assert_eq!(plan.refusal, None);
    assert_eq!(plan.release_entries, 2);
    assert_eq!(plan.files.len(), 2);
    assert_eq!(plan.third_party.len(), 2);
    assert!(plan.third_party[0].starts_with("/etc/apt/sources.list: "));

    let empty = tempfile::tempdir().unwrap();
    host(empty.path(), &[("/etc/apt/sources.list", "# nothing\n")]);
    let plan = sources::plan(empty.path(), debian());
    assert!(plan
        .refusal
        .unwrap()
        .starts_with("no enabled source of Debian 12"));
}

struct Fake {
    held: Vec<String>,
    audit: String,
    free: u64,
    reboot: bool,
}

impl Probe for Fake {
    fn held(&self) -> Result<Vec<String>, String> {
        Ok(self.held.clone())
    }
    fn audit(&self) -> Result<String, String> {
        Ok(self.audit.clone())
    }
    fn free(&self, _path: &str) -> Result<(u64, u64), String> {
        Ok((self.free, 1))
    }
    fn reboot_pending(&self) -> bool {
        self.reboot
    }
}

fn healthy() -> Fake {
    Fake {
        held: vec![],
        audit: String::new(),
        free: 100 << 30,
        reboot: false,
    }
}

#[test]
fn checks_pass_on_a_healthy_host_and_third_party_sources_only_warn() {
    let root = tempfile::tempdir().unwrap();
    host(root.path(), &[("/etc/apt/sources.list", CLASSIC)]);
    let plan = sources::plan(root.path(), debian());
    let checks = checks::evaluate(&healthy(), 0, &plan);
    let names: Vec<&str> = checks.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, major_upgrade::CHECKS);
    let states: Vec<&str> = checks.iter().map(|c| c.state.as_str()).collect();
    assert_eq!(states, [OK, OK, OK, OK, OK, OK, WARNING]);
    assert_eq!(checks[6].items.len(), 3);
    assert_eq!(checks::first_failure(&checks), None);
}

#[test]
fn each_problem_fails_its_check() {
    let root = tempfile::tempdir().unwrap();
    host(root.path(), &[("/etc/apt/sources.list", CLASSIC)]);
    let plan = sources::plan(root.path(), debian());
    let probe = Fake {
        held: vec!["linux-image-amd64".into()],
        audit: "The following packages are only half configured:\n libc6\n".into(),
        free: 1 << 30,
        reboot: true,
    };
    let checks = checks::evaluate(&probe, 3, &plan);
    let failed: Vec<&str> = checks
        .iter()
        .filter(|c| c.state == FAILED)
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(
        failed,
        [
            "up_to_date",
            "held_packages",
            "broken_packages",
            "free_space",
            "reboot_pending"
        ]
    );
    assert_eq!(checks[1].items, ["linux-image-amd64"]);
    // Every path is on one filesystem here: their needs add up.
    assert_eq!(
        checks[3].detail,
        "/ and /var and /boot has 1.0 GiB free, needs 6.3 GiB"
    );
    assert_eq!(
        checks::first_failure(&checks).unwrap(),
        "up_to_date: 3 updates pending in the current release; install them first"
    );
}

/// Records the programs run; fails the one named in `fail`.
struct Steps {
    ran: Vec<String>,
    fail: Option<(&'static str, &'static str)>,
}

impl Exec for Steps {
    fn run(
        &mut self,
        program: &str,
        args: &[&str],
        _env: &[(&str, &str)],
    ) -> io::Result<(i32, String)> {
        let line = format!("{program} {}", args.join(" "));
        self.ran.push(line.clone());
        match self.fail {
            Some((needle, output)) if line.contains(needle) => Ok((100, output.into())),
            _ => Ok((0, String::new())),
        }
    }
    fn note(&mut self, _line: &str) {}
}

fn prepared(sources_list: &str) -> (tempfile::TempDir, Paths) {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths {
        root: dir.path().join("root"),
        state_dir: dir.path().join("state"),
    };
    host(&paths.root, &[("/etc/apt/sources.list", sources_list)]);
    state::prepare(&paths.state_dir).unwrap();
    let s = State::new("c1", "debian 12", "debian 13", "boot", 1);
    state::write(&paths.state_dir, &s).unwrap();
    (dir, paths)
}

#[test]
fn the_steps_run_in_order_and_end_with_a_reboot() {
    let (_dir, paths) = prepared(CLASSIC);
    let mut steps = Steps {
        ran: vec![],
        fail: None,
    };
    let clock = Cell::new(10);
    let state = helper::run(&paths, &mut steps, || {
        clock.set(clock.get() + 1);
        clock.get()
    })
    .unwrap();
    assert_eq!(state.phase, Phase::Rebooting, "{}", state.error);
    assert_eq!(state.disabled_sources.len(), 3);
    let apt = |l: &String| {
        l.split(' ')
            .filter(|w| !w.starts_with('-') && !w.starts_with("Dpkg"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let ran: Vec<String> = steps.ran.iter().map(apt).collect();
    assert_eq!(
        ran,
        [
            "/usr/bin/apt-get update",
            "/usr/bin/apt-get upgrade",
            "/usr/bin/apt-get full-upgrade",
            "/usr/bin/systemctl reboot",
        ]
    );
    assert!(steps.ran[1].contains("--without-new-pkgs"));
    assert!(steps.ran[1].contains("Dpkg::Options::=--force-confold"));
    let written = fs::read_to_string(paths.root.join("etc/apt/sources.list")).unwrap();
    assert!(written.contains("deb http://deb.debian.org/debian trixie main"));
    assert_eq!(state::read(&paths.state_dir).unwrap().unwrap(), state);
    // It runs once.
    assert!(helper::run(&paths, &mut steps, || 0).is_err());
}

#[test]
fn a_failed_refresh_puts_the_sources_back() {
    let (_dir, paths) = prepared(CLASSIC);
    let mut steps = Steps {
        ran: vec![],
        fail: Some((
            "update",
            "E: The repository 'x trixie Release' does not have a Release file.",
        )),
    };
    let state = helper::run(&paths, &mut steps, || 5).unwrap();
    assert_eq!(state.phase, Phase::Failed);
    assert!(state.sources_restored);
    assert!(
        state.error.contains("does not have a Release file"),
        "{}",
        state.error
    );
    assert!(state
        .error
        .ends_with("the sources were put back as they were"));
    let back = fs::read_to_string(paths.root.join("etc/apt/sources.list")).unwrap();
    assert_eq!(back, CLASSIC);
    assert!(!steps.ran.iter().any(|l| l.contains("upgrade")));
}

#[test]
fn a_failed_package_step_leaves_the_rest_to_the_snapshot() {
    let (_dir, paths) = prepared(CLASSIC);
    let mut steps = Steps {
        ran: vec![],
        fail: Some((
            "full-upgrade",
            "E: Sub-process /usr/bin/dpkg returned an error code (1)",
        )),
    };
    let state = helper::run(&paths, &mut steps, || 5).unwrap();
    assert_eq!(state.phase, Phase::Failed);
    assert!(!state.sources_restored);
    assert_eq!(
        state.error,
        "`apt-get full-upgrade` exited with 100: E: Sub-process /usr/bin/dpkg returned an error code (1); only the snapshot goes back now"
    );
    assert!(!steps.ran.iter().any(|l| l.contains("reboot")));
}

#[test]
fn sources_without_the_release_fail_before_changing_anything() {
    let text = "deb http://apt.example.com/x bookworm main\n";
    let (_dir, paths) = prepared(text);
    let mut steps = Steps {
        ran: vec![],
        fail: None,
    };
    let state = helper::run(&paths, &mut steps, || 5).unwrap();
    assert_eq!(state.phase, Phase::Failed);
    assert!(steps.ran.is_empty());
    assert_eq!(
        fs::read_to_string(paths.root.join("etc/apt/sources.list")).unwrap(),
        text
    );
}

#[test]
fn a_payload_must_name_this_release_and_the_next() {
    use monitorflow_common::proto::v1::MajorUpgrade;
    use prost::Message;
    let payload = |from: &str, to: &str| {
        MajorUpgrade {
            from: from.into(),
            to: to.into(),
        }
        .encode_to_vec()
    };
    assert!(helper::requested(&payload("debian 12", "debian 13"), "debian 12").is_ok());
    assert_eq!(
        helper::requested(&payload("debian 12", "debian 13"), "debian 13").unwrap_err(),
        "this server runs debian 13, not debian 12"
    );
    assert!(helper::requested(&payload("debian 11", "debian 13"), "debian 11").is_err());
    assert!(helper::requested(b"\xff", "debian 12").is_err());
}

#[test]
fn consent_is_read_from_the_file_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent.toml");
    fs::write(&path, "upstream_url = \"https://relay:7443\"\n").unwrap();
    assert_eq!(helper::consent(&path), Ok(false));
    fs::write(
        &path,
        "upstream_url = \"https://relay:7443\"\n[packages]\nmajor_upgrades = true\n",
    )
    .unwrap();
    assert_eq!(helper::consent(&path), Ok(true));
}

#[test]
fn the_consent_is_set_keeping_the_rest_of_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent.toml");
    let text = "# Written by install.sh.\nupstream_url = \"https://relay:7443\"\n\n[packages]\nreport_interval = 600\n";
    fs::write(&path, text).unwrap();
    assert_eq!(helper::set_consent(&path, true), Ok(true));
    let edited = fs::read_to_string(&path).unwrap();
    assert!(edited.starts_with(text), "{edited}");
    assert_eq!(helper::consent(&path), Ok(true));
    assert_eq!(helper::set_consent(&path, true), Ok(false));
    assert_eq!(helper::set_consent(&path, false), Ok(true));
    assert_eq!(helper::consent(&path), Ok(false));
}
