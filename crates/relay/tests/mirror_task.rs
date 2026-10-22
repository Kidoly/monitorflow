//! The relay's mirror sync loop (ADR-0034): the key and the index,
//! narrowing to configured mirrors, retention, an upstream that is down,
//! and a chained relay syncing from its parent's listener.

mod mirror_fixture;

use std::path::Path;
use std::time::Duration;

use mirror_fixture::Upstream;
use monitorflow_common::mirror::{read_current, Family};
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
