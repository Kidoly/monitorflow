use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::panic::{self, AssertUnwindSafe};

use monitorflow_common::proto::v1::configure_sources::{DnfRepository, SigningKey};

use super::*;

type Hook = Box<dyn FnMut(&Path) -> io::Result<()>>;
type Change = Box<dyn Fn(&mut ConfigureSources)>;

thread_local! {
    /// Called before each change to the host; an error fails the change.
    static HOOK: RefCell<Option<Hook>> = RefCell::new(None);
}

pub(super) fn before_change(root: &Path) -> io::Result<()> {
    HOOK.with(|hook| match hook.borrow_mut().as_mut() {
        Some(hook) => hook(root),
        None => Ok(()),
    })
}

fn set_hook(hook: impl FnMut(&Path) -> io::Result<()> + 'static) {
    HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}

fn clear_hook() {
    HOOK.with(|slot| *slot.borrow_mut() = None);
}

struct Check {
    result: Result<(), String>,
    calls: Cell<usize>,
}

impl Check {
    fn ok() -> Self {
        Self {
            result: Ok(()),
            calls: Cell::new(0),
        }
    }

    fn failing() -> Self {
        Self {
            result: Err("E: mirror unreachable".into()),
            calls: Cell::new(0),
        }
    }
}

impl PostCheck for Check {
    fn check(&self, _: Family) -> Result<(), String> {
        self.calls.set(self.calls.get() + 1);
        self.result.clone()
    }
}

fn write(root: &Path, path: &str, content: &str, mode: u32) {
    let disk = root.join(path.trim_start_matches('/'));
    fs::create_dir_all(disk.parent().unwrap()).unwrap();
    fs::write(&disk, content).unwrap();
    fs::set_permissions(&disk, fs::Permissions::from_mode(mode)).unwrap();
}

fn read(root: &Path, path: &str) -> String {
    fs::read_to_string(root.join(path.trim_start_matches('/'))).unwrap()
}

fn exists(root: &Path, path: &str) -> bool {
    root.join(path.trim_start_matches('/')).exists()
}

/// Every file and directory under `etc`, with its content and mode.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, (Option<Vec<u8>>, u32)> {
    fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<PathBuf, (Option<Vec<u8>>, u32)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            let rel = path.strip_prefix(root).unwrap().to_owned();
            let mode = meta.mode() & 0o7777;
            if meta.is_dir() {
                out.insert(rel, (None, mode));
                walk(&path, root, out);
            } else {
                out.insert(rel, (Some(fs::read(&path).unwrap()), mode));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(&root.join("etc"), root, &mut out);
    out
}

fn debian() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    write(
        r,
        "/etc/apt/sources.list",
        "deb http://deb.debian.org/debian bookworm main\n",
        0o644,
    );
    write(
        r,
        "/etc/apt/sources.list.d/docker.list",
        "deb https://download.docker.com/linux/debian bookworm stable\n",
        0o640,
    );
    write(
        r,
        "/etc/apt/sources.list.d/vendor.sources",
        "Types: deb\nURIs: https://vendor.example\nSuites: stable\nComponents: main\n",
        0o600,
    );
    write(
        r,
        "/etc/apt/sources.list.d/notes.txt",
        "not read by apt\n",
        0o644,
    );
    root
}

fn signing_key(name: &str, seed: u8) -> SigningKey {
    let (armored, fingerprint) = key::tests::test_key(seed);
    SigningKey {
        name: name.into(),
        armored,
        fingerprint,
    }
}

fn apt_spec(mode: ProtoMode, snapshot: &str) -> ConfigureSources {
    let mut spec = ConfigureSources {
        mirror_base_url: "https://mirror.example.net/".into(),
        apt: vec![AptRepository {
            mirror: "debian-bookworm".into(),
            snapshot: snapshot.into(),
            suite: "bookworm".into(),
            components: vec!["main".into(), "contrib".into()],
            architectures: vec!["amd64".into()],
        }],
        keys: vec![signing_key("archive", 1)],
        ..Default::default()
    };
    spec.set_family(ProtoFamily::Apt);
    spec.set_mode(mode);
    spec
}

fn backups(root: &Path) -> usize {
    fs::read_dir(root.join(backup::STATE_DIR))
        .unwrap()
        .filter(|e| e.as_ref().unwrap().file_type().unwrap().is_dir())
        .count()
}

#[test]
fn apt_replace_writes_the_mirror_and_sets_the_others_aside() {
    let root = debian();
    let r = root.path();
    let check = Check::ok();
    let outcome = apply(&apt_spec(ProtoMode::Replace, "current"), r, &check).unwrap();
    assert_eq!(check.calls.get(), 1);
    let key = key::tests::test_key(1).0;
    assert_eq!(read(r, "/etc/apt/keyrings/monitorflow-archive.asc"), key);
    assert_eq!(
        read(r, apt::SOURCES),
        format!(
            "{HEADER}\nTypes: deb\nURIs: https://mirror.example.net/apt/debian-bookworm/current/\n\
             Suites: bookworm\nComponents: main contrib\nArchitectures: amd64\n\
             Signed-By: /etc/apt/keyrings/monitorflow-archive.asc\n"
        )
    );
    assert!(!exists(r, "/etc/apt/sources.list"));
    assert!(!exists(r, "/etc/apt/sources.list.d/docker.list"));
    assert!(!exists(r, "/etc/apt/sources.list.d/vendor.sources"));
    assert!(exists(r, "/etc/apt/sources.list.d/notes.txt"));
    // The originals are in the backup, not deleted.
    let dir = outcome.backup.unwrap();
    assert_eq!(
        fs::read_to_string(dir.join("files/etc/apt/sources.list.d/docker.list")).unwrap(),
        "deb https://download.docker.com/linux/debian bookworm stable\n"
    );
    let expected: Vec<PathBuf> = [
        "/etc/apt/keyrings",
        "/etc/apt/keyrings/monitorflow-archive.asc",
        apt::SOURCES,
        "/etc/apt/sources.list",
        "/etc/apt/sources.list.d/docker.list",
        "/etc/apt/sources.list.d/vendor.sources",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    assert_eq!(outcome.changed, expected);
}

#[test]
fn apt_add_keeps_the_other_sources() {
    let root = debian();
    let r = root.path();
    apply(&apt_spec(ProtoMode::Add, "current"), r, &Check::ok()).unwrap();
    assert!(exists(r, apt::SOURCES));
    assert!(exists(r, "/etc/apt/sources.list"));
    assert!(exists(r, "/etc/apt/sources.list.d/docker.list"));
}

#[test]
fn revert_restores_debian_byte_for_byte() {
    let root = debian();
    let r = root.path();
    let before = snapshot(r);
    apply(&apt_spec(ProtoMode::Replace, "current"), r, &Check::ok()).unwrap();
    assert_ne!(snapshot(r), before);
    let outcome = revert(r).unwrap();
    assert!(!outcome.changed.is_empty());
    assert_eq!(snapshot(r), before);
    // Nothing left to revert.
    assert_eq!(revert(r).unwrap(), Outcome::default());
}

#[test]
fn applying_twice_changes_nothing() {
    let root = debian();
    let r = root.path();
    let check = Check::ok();
    let spec = apt_spec(ProtoMode::Replace, "current");
    apply(&spec, r, &check).unwrap();
    let after_first = snapshot(r);
    let again = apply(&spec, r, &check).unwrap();
    assert!(again.changed.is_empty());
    assert_eq!(snapshot(r), after_first);
    assert_eq!(check.calls.get(), 1);
    assert_eq!(backups(r), 1);
}

#[test]
fn a_new_snapshot_keeps_the_first_backup() {
    let root = debian();
    let r = root.path();
    let before = snapshot(r);
    apply(&apt_spec(ProtoMode::Replace, "current"), r, &Check::ok()).unwrap();
    let outcome = apply(&apt_spec(ProtoMode::Replace, "2026-10-19"), r, &Check::ok()).unwrap();
    assert_eq!(outcome.changed, vec![PathBuf::from(apt::SOURCES)]);
    assert!(read(r, apt::SOURCES).contains("/apt/debian-bookworm/2026-10-19/"));
    assert_eq!(backups(r), 1);
    revert(r).unwrap();
    assert_eq!(snapshot(r), before);
}

#[test]
fn keys_no_longer_given_are_removed_and_new_sources_set_aside() {
    let root = debian();
    let r = root.path();
    let before_first = snapshot(r);
    let mut spec = apt_spec(ProtoMode::Replace, "current");
    spec.keys.push(signing_key("old", 3));
    apply(&spec, r, &Check::ok()).unwrap();
    assert!(exists(r, "/etc/apt/keyrings/monitorflow-old.asc"));
    // An administrator adds a source after the first apply.
    write(
        r,
        "/etc/apt/sources.list.d/new.list",
        "deb http://new.example stable main\n",
        0o644,
    );
    spec.keys.pop();
    let outcome = apply(&spec, r, &Check::ok()).unwrap();
    assert!(!exists(r, "/etc/apt/keyrings/monitorflow-old.asc"));
    assert!(!exists(r, "/etc/apt/sources.list.d/new.list"));
    assert!(!read(r, apt::SOURCES).contains("monitorflow-old"));
    assert_eq!(outcome.changed.len(), 3);
    revert(r).unwrap();
    assert!(exists(r, "/etc/apt/sources.list.d/new.list"));
    fs::remove_file(r.join("etc/apt/sources.list.d/new.list")).unwrap();
    assert_eq!(snapshot(r), before_first);
}

#[test]
fn a_source_changed_after_being_set_aside_is_not_touched() {
    let root = debian();
    let r = root.path();
    let spec = apt_spec(ProtoMode::Replace, "current");
    apply(&spec, r, &Check::ok()).unwrap();
    write(
        r,
        "/etc/apt/sources.list",
        "deb http://other.example stable main\n",
        0o644,
    );
    let error = apply(&spec, r, &Check::ok()).unwrap_err();
    assert!(matches!(error, Error::Unsupported { .. }), "{error}");
    assert!(exists(r, "/etc/apt/sources.list"));
}

#[test]
fn switching_mode_starts_again_from_the_originals() {
    let root = debian();
    let r = root.path();
    apply(&apt_spec(ProtoMode::Replace, "current"), r, &Check::ok()).unwrap();
    let outcome = apply(&apt_spec(ProtoMode::Add, "current"), r, &Check::ok()).unwrap();
    assert!(outcome
        .changed
        .contains(&PathBuf::from("/etc/apt/sources.list")));
    assert!(exists(r, "/etc/apt/sources.list"));
    assert!(exists(r, apt::SOURCES));
    assert_eq!(backups(r), 2);
}

#[test]
fn an_existing_file_at_a_monitorflow_path_is_backed_up() {
    let root = debian();
    let r = root.path();
    write(
        r,
        "/etc/apt/keyrings/monitorflow-archive.asc",
        "hand-installed\n",
        0o600,
    );
    let before = snapshot(r);
    apply(&apt_spec(ProtoMode::Add, "current"), r, &Check::ok()).unwrap();
    revert(r).unwrap();
    assert_eq!(snapshot(r), before);
}

#[test]
fn a_wrong_fingerprint_is_refused_before_any_change() {
    let root = debian();
    let r = root.path();
    let before = snapshot(r);
    let mut spec = apt_spec(ProtoMode::Replace, "current");
    spec.keys[0].fingerprint = key::tests::test_key(9).1;
    let error = apply(&spec, r, &Check::ok()).unwrap_err();
    assert!(
        matches!(&error, Error::Key { name, .. } if name == "archive"),
        "{error}"
    );
    assert_eq!(snapshot(r), before);
    assert!(!r.join(backup::STATE_DIR).exists());
}

#[test]
fn a_failed_post_check_reverts() {
    let root = debian();
    let r = root.path();
    let before = snapshot(r);
    let error = apply(
        &apt_spec(ProtoMode::Replace, "current"),
        r,
        &Check::failing(),
    )
    .unwrap_err();
    assert!(matches!(&error, Error::PostCheck(reason) if reason.contains("unreachable")));
    assert_eq!(snapshot(r), before);
    assert_eq!(revert(r).unwrap(), Outcome::default());
}

#[test]
fn a_failure_midway_rolls_back() {
    let root = debian();
    let r = root.path();
    let before = snapshot(r);
    let mut calls = 0;
    set_hook(move |_| {
        calls += 1;
        match calls {
            4 => Err(io::Error::other("disk full")),
            _ => Ok(()),
        }
    });
    let result = apply(&apt_spec(ProtoMode::Replace, "current"), r, &Check::ok());
    clear_hook();
    assert!(matches!(result, Err(Error::Io { .. })), "{result:?}");
    assert_eq!(snapshot(r), before);
}

#[test]
fn a_crash_midway_leaves_a_manifest_that_reverts() {
    let root = debian();
    let r = root.path();
    let before = snapshot(r);
    let mut calls = 0;
    set_hook(move |root| {
        calls += 1;
        // The whole manifest is on disk before the first change.
        let state = root.join(backup::STATE_DIR);
        let name = fs::read_to_string(state.join("active")).unwrap();
        let manifest = fs::read_to_string(state.join(name.trim()).join("manifest.toml")).unwrap();
        for path in [
            "sources.list\"",
            "docker.list",
            "vendor.sources",
            "monitorflow.sources",
        ] {
            assert!(manifest.contains(path), "{path} missing from {manifest}");
        }
        if calls == 4 {
            panic!("simulated crash");
        }
        Ok(())
    });
    let crashed = panic::catch_unwind(AssertUnwindSafe(|| {
        apply(&apt_spec(ProtoMode::Replace, "current"), r, &Check::ok())
    }));
    clear_hook();
    assert!(crashed.is_err());
    assert_ne!(snapshot(r), before);
    revert(r).unwrap();
    assert_eq!(snapshot(r), before);
}

#[test]
fn invalid_specs_are_refused() {
    let root = debian();
    let r = root.path();
    let cases: Vec<(&str, Change)> = vec![
        (
            "family",
            Box::new(|s| s.set_family(ProtoFamily::Unspecified)),
        ),
        ("mode", Box::new(|s| s.set_mode(ProtoMode::Unspecified))),
        (
            "URL",
            Box::new(|s| s.mirror_base_url = "ftp://mirror".into()),
        ),
        (
            "URL",
            Box::new(|s| s.mirror_base_url = "https://m\nx".into()),
        ),
        ("suite", Box::new(|s| s.apt[0].suite = "../etc".into())),
        (
            "component",
            Box::new(|s| s.apt[0].components = vec!["main\nX: y".into()]),
        ),
        ("components", Box::new(|s| s.apt[0].components.clear())),
        (
            "family only",
            Box::new(|s| s.dnf = vec![DnfRepository::default()]),
        ),
        ("signing key", Box::new(|s| s.keys.clear())),
        (
            "repeated",
            Box::new(|s| s.keys.push(signing_key("archive", 1))),
        ),
    ];
    for (expected, change) in cases {
        let mut spec = apt_spec(ProtoMode::Replace, "current");
        change(&mut spec);
        let error = apply(&spec, r, &Check::ok()).unwrap_err().to_string();
        assert!(error.contains(expected), "{expected}: {error}");
    }
}

#[test]
fn a_symlinked_source_is_refused() {
    let root = debian();
    let r = root.path();
    std::os::unix::fs::symlink("/dev/null", r.join("etc/apt/sources.list.d/link.list")).unwrap();
    let error = apply(&apt_spec(ProtoMode::Replace, "current"), r, &Check::ok()).unwrap_err();
    assert!(error.to_string().contains("not a regular file"), "{error}");
}
