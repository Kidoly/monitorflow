//! The relay's mirror sync loop (ADR-0034): the key and the index,
//! narrowing to configured mirrors, retention, an upstream that is down,
//! a chained relay syncing from its parent's listener, and mirrors deleted
//! upstream.

mod mirror_fixture;

use std::path::Path;
use std::time::Duration;

use mirror_fixture::Upstream;
use monitorflow_common::mirror::{
    read_current, read_pinned, read_removed, write_pinned, write_removed, Family,
};
use monitorflow_relay::mirror::sync::Outcome;
use monitorflow_relay::mirror::task::{MirrorSettings, MirrorTask};

fn settings(upstream: &str, root: &Path) -> MirrorSettings {
    MirrorSettings {
        upstream_url: upstream.to_owned(),
        root: root.to_owned(),
        interval: Duration::from_secs(900),
        mirrors: Vec::new(),
        keep_snapshots: 2,
        max_downloads: 4,
        request_timeout: Duration::from_secs(10),
        max_snapshot_bytes: 1 << 30,
    }
}

/// Files under `dir`, relative, with their content; dot files skipped.
fn tree(dir: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let path = entry.path();
            let kind = std::fs::symlink_metadata(&path).unwrap().file_type();
            if kind.is_dir() {
                walk(base, &path, out);
            } else if kind.is_file() {
                let rel = path
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.push((rel, std::fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

#[tokio::test]
async fn cycles_copy_the_key_narrow_and_prune() {
    let up = Upstream::start().await;
    up.key("key v1");
    up.apt("debian", "s1", &[("a/a_1_all.deb", b"a1")]);
    up.dnf("rocky", "s1", &[("a/a-1.rpm", b"r1")]);
    let relay = tempfile::tempdir().unwrap();
    let mut only_debian = settings(&up.base, relay.path());
    only_debian.mirrors = vec!["debian".into()];
    let mut task = MirrorTask::new(only_debian).unwrap();

    let report = task.cycle().await;
    assert!(report.index_error.is_none());
    assert_eq!(report.mirrors.len(), 1);
    assert!(matches!(report.mirrors[0].1, Ok(Outcome::Synced(_))));
    assert!(!relay.path().join("dnf/rocky").exists());
    let key = relay.path().join("keys/monitorflow-archive.asc");
    assert_eq!(std::fs::read_to_string(&key).unwrap(), "key v1");

    // Three more snapshots with two kept: current and the one before.
    up.key("key v2");
    for s in ["s2", "s3", "s4"] {
        up.apt(
            "debian",
            s,
            &[
                ("a/a_1_all.deb", b"a1"),
                (&format!("b/b_{s}_all.deb"), s.as_bytes()),
            ],
        );
        let report = task.cycle().await;
        assert!(report.mirrors[0].1.is_ok(), "{s}: {:?}", report.mirrors);
    }
    let apt = relay.path().join("apt/debian");
    assert_eq!(read_current(&apt).as_deref(), Some("s4"));
    assert!(apt.join("s3").is_dir() && apt.join("s4").is_dir());
    assert!(!apt.join("s1").exists() && !apt.join("s2").exists());
    assert_eq!(std::fs::read_to_string(&key).unwrap(), "key v2");
    let report = task.cycle().await;
    assert_eq!(report.mirrors[0].1, Ok(Outcome::Unchanged));
}

#[tokio::test]
async fn an_unreachable_upstream_is_reported_not_fatal() {
    let relay = tempfile::tempdir().unwrap();
    // A port nothing listens on.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let mut task = MirrorTask::new(settings(&base, relay.path())).unwrap();
    let report = task.cycle().await;
    assert!(report.index_error.is_some());
    assert!(report.mirrors.is_empty());

    // An upstream that publishes nothing yet.
    let up = Upstream::start().await;
    let mut task = MirrorTask::new(settings(&up.base, relay.path())).unwrap();
    let report = task.cycle().await;
    assert!(report.index_error.is_none(), "{:?}", report.index_error);
    assert!(report.mirrors.is_empty());
}

#[tokio::test]
async fn a_chained_relay_syncs_from_its_parent() {
    let up = Upstream::start().await;
    up.key("key");
    up.apt("debian", "s1", &[("a/a_1+b~c_all.deb", b"a1")]);
    up.dnf("rocky", "s1", &[("a/a-1.rpm", b"r1")]);

    // The parent relay copies from the server and serves its copy with
    // the shared listener, as `main` does.
    let parent_dir = tempfile::tempdir().unwrap();
    let mut parent = MirrorTask::new(settings(&up.base, parent_dir.path())).unwrap();
    let report = parent.cycle().await;
    assert!(
        report.mirrors.iter().all(|(_, r)| r.is_ok()),
        "{:?}",
        report.mirrors
    );
    let parent_listener = Upstream::serve(parent_dir).await;

    let child_dir = tempfile::tempdir().unwrap();
    let mut child = MirrorTask::new(settings(&parent_listener.base, child_dir.path())).unwrap();
    let report = child.cycle().await;
    assert_eq!(report.mirrors.len(), 2);
    assert!(
        report
            .mirrors
            .iter()
            .all(|(_, r)| matches!(r, Ok(Outcome::Synced(_)))),
        "{:?}",
        report.mirrors
    );

    // The child holds exactly what the server publishes.
    for (family, name) in [(Family::Apt, "debian"), (Family::Dnf, "rocky")] {
        let server = up.snapshot_dir(family, name, "s1");
        let child = child_dir
            .path()
            .join(family.as_str())
            .join(name)
            .join("current");
        assert_eq!(tree(&child), tree(&server), "{family}/{name}");
    }
    assert_eq!(
        std::fs::read_to_string(child_dir.path().join("keys/monitorflow-archive.asc")).unwrap(),
        "key"
    );

    // A new snapshot on the server reaches the child once the parent has it.
    up.dnf("rocky", "s2", &[("a/a-1.rpm", b"r1"), ("b/b-1.rpm", b"r2")]);
    assert_eq!(child.cycle().await.mirrors[1].1, Ok(Outcome::Unchanged));
    parent.cycle().await;
    child.cycle().await;
    let rocky = child_dir.path().join("dnf/rocky");
    assert_eq!(read_current(&rocky).as_deref(), Some("s2"));
    assert_eq!(
        std::fs::read(rocky.join("current/Packages/b/b-1.rpm")).unwrap(),
        b"r2"
    );
}

#[tokio::test]
async fn pinned_snapshots_are_copied_kept_and_passed_down() {
    let up = Upstream::start().await;
    for s in ["s1", "s2", "s3"] {
        up.apt(
            "debian",
            s,
            &[
                ("a/a_1_all.deb", b"a1"),
                (&format!("b/b_{s}_all.deb"), s.as_bytes()),
            ],
        );
    }
    // A campaign pins s1, which the relay never saw as current.
    let upstream_dir = up.served.root(Family::Apt).join("debian");
    write_pinned(&upstream_dir, &["s1".into()]).unwrap();

    let parent_dir = tempfile::tempdir().unwrap();
    let mut keep_one = settings(&up.base, parent_dir.path());
    keep_one.keep_snapshots = 1;
    let mut parent = MirrorTask::new(keep_one.clone()).unwrap();
    let report = parent.cycle().await;
    assert!(matches!(report.mirrors[0].1, Ok(Outcome::Synced(_))));
    assert_eq!(report.mirrors[0].0.pinned[0].name, "s1");
    let apt = parent_dir.path().join("apt/debian");
    assert_eq!(read_current(&apt).as_deref(), Some("s3"));
    assert_eq!(
        tree(&apt.join("s1")),
        tree(&up.snapshot_dir(Family::Apt, "debian", "s1"))
    );
    assert_eq!(read_pinned(&apt), ["s1"]);

    // A new current: the pinned snapshot stays beside it, over the count.
    up.apt("debian", "s4", &[("a/a_1_all.deb", b"a1")]);
    parent.cycle().await;
    assert_eq!(read_current(&apt).as_deref(), Some("s4"));
    assert!(apt.join("s1").is_dir() && !apt.join("s3").exists());

    // A chained relay gets it from its parent's index.
    let parent_listener = Upstream::serve(parent_dir).await;
    let child_dir = tempfile::tempdir().unwrap();
    let mut child_settings = keep_one.clone();
    child_settings.upstream_url = parent_listener.base.clone();
    child_settings.root = child_dir.path().to_owned();
    let mut child = MirrorTask::new(child_settings).unwrap();
    child.cycle().await;
    let child_apt = child_dir.path().join("apt/debian");
    assert_eq!(read_current(&child_apt).as_deref(), Some("s4"));
    assert_eq!(
        tree(&child_apt.join("s1")),
        tree(&up.snapshot_dir(Family::Apt, "debian", "s1"))
    );

    // Unpinned upstream, it goes at the next cycle.
    write_pinned(&upstream_dir, &[]).unwrap();
    let mut parent = MirrorTask::new(keep_one).unwrap();
    parent.cycle().await;
    let apt = parent_listener.served.root(Family::Apt).join("debian");
    assert!(!apt.join("s1").exists());
    assert!(read_pinned(&apt).is_empty());
}

/// Deletes `name` upstream as the server does: the name goes in `.removed`,
/// then the tree goes.
fn delete_upstream(up: &Upstream, family: Family, name: &str) {
    let root = up.served.root(family);
    let mut names = read_removed(root);
    names.push(name.to_owned());
    write_removed(root, &names).unwrap();
    std::fs::remove_dir_all(root.join(name)).unwrap();
}

#[tokio::test]
async fn a_mirror_deleted_upstream_goes_down_the_chain() {
    let up = Upstream::start().await;
    up.apt("debian", "s1", &[("a/a_1_all.deb", b"a1")]);
    up.dnf("rocky", "s1", &[("a/a-1.rpm", b"r1")]);
    up.dnf("alma", "s1", &[("a/a-1.rpm", b"r2")]);

    let parent_dir = tempfile::tempdir().unwrap();
    let parent_root = parent_dir.path().to_owned();
    let mut parent = MirrorTask::new(settings(&up.base, &parent_root)).unwrap();
    parent.cycle().await;
    let parent_listener = Upstream::serve(parent_dir).await;
    let child_dir = tempfile::tempdir().unwrap();
    let mut child = MirrorTask::new(settings(&parent_listener.base, child_dir.path())).unwrap();
    child.cycle().await;
    assert!(child_dir.path().join("dnf/rocky/current").exists());

    delete_upstream(&up, Family::Dnf, "rocky");
    delete_upstream(&up, Family::Apt, "debian");

    let report = parent.cycle().await;
    assert_eq!(
        report.removed,
        [
            (Family::Apt, "debian".to_owned()),
            (Family::Dnf, "rocky".to_owned())
        ]
    );
    assert!(!parent_root.join("dnf/rocky").exists());
    assert!(!parent_root.join("apt/debian").exists());
    assert_eq!(report.mirrors.len(), 1, "alma stays");

    // The parent passes the news on: the child has it from its listener.
    let report = child.cycle().await;
    assert_eq!(report.removed.len(), 2);
    assert!(!child_dir.path().join("dnf/rocky").exists());
    assert!(!child_dir.path().join("apt/debian").exists());
    assert_eq!(
        read_current(&child_dir.path().join("dnf/alma")).as_deref(),
        Some("s1")
    );

    // Nothing brings it back, and nothing is removed twice.
    assert!(child.cycle().await.removed.is_empty());
    assert!(!child_dir.path().join("dnf/rocky").exists());
}

#[tokio::test]
async fn a_mirror_merely_missing_upstream_stays() {
    // What a server restored from a backup looks like: its index lists
    // nothing, and agents behind the relay still need the copy.
    let up = Upstream::start().await;
    up.dnf("rocky", "s1", &[("a/a-1.rpm", b"r1")]);
    let dir = tempfile::tempdir().unwrap();
    let mut task = MirrorTask::new(settings(&up.base, dir.path())).unwrap();
    task.cycle().await;

    std::fs::remove_dir_all(up.served.root(Family::Dnf).join("rocky")).unwrap();
    let report = task.cycle().await;
    assert!(report.index_error.is_none() && report.mirrors.is_empty());
    assert!(report.removed.is_empty());
    assert!(dir
        .path()
        .join("dnf/rocky/current/Packages/a/a-1.rpm")
        .exists());
}

#[tokio::test]
async fn a_deletion_reaches_a_relay_outside_its_narrowed_list_and_its_stale_staging() {
    let up = Upstream::start().await;
    up.dnf("rocky", "s1", &[("a/a-1.rpm", b"r1")]);
    up.dnf("alma", "s1", &[("a/a-1.rpm", b"r2")]);
    let dir = tempfile::tempdir().unwrap();
    let mut wide = MirrorTask::new(settings(&up.base, dir.path())).unwrap();
    wide.cycle().await;

    let narrowed = MirrorSettings {
        mirrors: vec!["alma".to_owned()],
        ..settings(&up.base, dir.path())
    };
    let mut task = MirrorTask::new(narrowed).unwrap();
    // A sync cut short by the deletion, as files vanish under it upstream.
    std::fs::create_dir_all(dir.path().join("dnf/rocky/.sync-s2/repodata")).unwrap();
    // A crash in the middle of an earlier removal.
    std::fs::create_dir_all(dir.path().join("dnf/.removing-old/s1")).unwrap();

    delete_upstream(&up, Family::Dnf, "rocky");
    let report = task.cycle().await;
    assert_eq!(report.removed, [(Family::Dnf, "rocky".to_owned())]);
    assert!(!dir.path().join("dnf/rocky").exists());
    assert!(!dir.path().join("dnf/.removing-old").exists());
    assert_eq!(read_removed(&dir.path().join("dnf")), ["rocky"]);
}

#[tokio::test]
async fn a_mirror_added_again_comes_back() {
    let up = Upstream::start().await;
    up.dnf("rocky", "s1", &[("a/a-1.rpm", b"r1")]);
    let dir = tempfile::tempdir().unwrap();
    let mut task = MirrorTask::new(settings(&up.base, dir.path())).unwrap();
    task.cycle().await;
    delete_upstream(&up, Family::Dnf, "rocky");
    assert_eq!(task.cycle().await.removed.len(), 1);

    // The server overrides `.removed` by publishing the name.
    up.dnf("rocky", "s2", &[("a/a-1.rpm", b"r3")]);
    let report = task.cycle().await;
    assert!(report.removed.is_empty());
    assert_eq!(report.mirrors.len(), 1);
    assert!(read_removed(&dir.path().join("dnf")).is_empty());
    assert_eq!(
        std::fs::read(dir.path().join("dnf/rocky/current/Packages/a/a-1.rpm")).unwrap(),
        b"r3"
    );
}
