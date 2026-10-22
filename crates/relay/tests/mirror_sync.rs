//! Copying snapshots from a fake upstream (ADR-0034): a full first copy,
//! incremental copies through hardlinks, a checksum mismatch that leaves
//! `current` alone, and resuming an interrupted copy.

mod mirror_fixture;

use std::os::unix::fs::MetadataExt;
use std::path::Path;

use mirror_fixture::{entry, Upstream};
use monitorflow_common::mirror::{read_current, Family};
use monitorflow_relay::mirror::fetch::FetchError;
use monitorflow_relay::mirror::sync::{Outcome, Stats, SyncError, Syncer, MANIFEST};

fn syncer(upstream: &Upstream, root: &Path) -> Syncer {
    Syncer::new(upstream.fetcher(), root.to_owned(), 3)
}

fn ino(path: &Path) -> u64 {
    std::fs::metadata(path).unwrap().ino()
}

#[tokio::test]
async fn copies_snapshots_and_then_only_what_changed() {
    let up = Upstream::start().await;
    up.apt(
        "debian",
        "s1",
        &[("a/a_1_amd64.deb", b"aaa"), ("b/b_1+x~y_all.deb", b"bbbb")],
    );
    up.dnf(
        "rocky",
        "s1",
        &[("a/a-1.rpm", b"rpm-a"), ("b/b-1.rpm", b"rpm-b")],
    );
    let relay = tempfile::tempdir().unwrap();
    let sync = syncer(&up, relay.path());
    let fetcher = up.fetcher();

    // apt: Release, InRelease, Release.gpg, Packages and two packages; the
    // listed Packages.gz is absent upstream and skipped.
    let debian = entry(&fetcher, "debian").await;
    let Outcome::Synced(stats) = sync.sync(&debian).await.unwrap() else {
        panic!("expected a copy");
    };
    assert_eq!((stats.downloaded, stats.linked, stats.kept), (6, 0, 0));
    let apt = relay.path().join("apt/debian");
    assert_eq!(read_current(&apt).as_deref(), Some("s1"));
    assert_eq!(
        std::fs::read(apt.join("current/pool/main/b/b_1+x~y_all.deb")).unwrap(),
        b"bbbb"
    );
    for file in ["InRelease", "Release.gpg", "main/binary-amd64/Packages"] {
        let path = format!("current/dists/bookworm/{file}");
        assert_eq!(
            std::fs::read(apt.join(&path)).unwrap(),
            std::fs::read(
                up.snapshot_dir(Family::Apt, "debian", "s1")
                    .join(&path[8..])
            )
            .unwrap(),
            "{file}"
        );
    }
    assert!(apt.join("s1").join(MANIFEST).is_file());
    assert!(!apt.join(".sync-s1").exists());
    assert_eq!(sync.sync(&debian).await.unwrap(), Outcome::Unchanged);

    // dnf: repomd.xml, its signature, primary and two packages.
    let rocky = entry(&fetcher, "rocky").await;
    let Outcome::Synced(stats) = sync.sync(&rocky).await.unwrap() else {
        panic!("expected a copy");
    };
    assert_eq!(stats.downloaded, 5);
    let dnf = relay.path().join("dnf/rocky");
    assert_eq!(
        std::fs::read(dnf.join("current/Packages/b/b-1.rpm")).unwrap(),
        b"rpm-b"
    );
    assert!(dnf.join("current/repodata/repomd.xml.asc").is_file());

    // A new snapshot changes one package and adds one: the unchanged one
    // is linked, not fetched.
    up.apt(
        "debian",
        "s2",
        &[
            ("a/a_1_amd64.deb", b"aaa"),
            ("b/b_1+x~y_all.deb", b"BBBB"),
            ("c/c_1_all.deb", b"c"),
        ],
    );
    let debian = entry(&fetcher, "debian").await;
    let Outcome::Synced(stats) = sync.sync(&debian).await.unwrap() else {
        panic!("expected a copy");
    };
    // Release files and Packages changed, b changed, c is new.
    assert_eq!(
        stats,
        Stats {
            downloaded: 6,
            bytes: stats.bytes,
            linked: 1,
            kept: 0
        }
    );
    assert_eq!(read_current(&apt).as_deref(), Some("s2"));
    let a = "pool/main/a/a_1_amd64.deb";
    assert_eq!(ino(&apt.join("s1").join(a)), ino(&apt.join("s2").join(a)));
    assert_eq!(
        std::fs::read(apt.join("current/pool/main/b/b_1+x~y_all.deb")).unwrap(),
        b"BBBB"
    );
    assert_eq!(
        std::fs::read(apt.join("s1/pool/main/b/b_1+x~y_all.deb")).unwrap(),
        b"bbbb"
    );

    // Pointing current back at a snapshot the relay holds copies nothing.
    monitorflow_common::mirror::swap_current(&up.served.root(Family::Apt).join("debian"), "s1")
        .unwrap();
    let debian = entry(&fetcher, "debian").await;
    assert_eq!(sync.sync(&debian).await.unwrap(), Outcome::Swapped);
    assert_eq!(read_current(&apt).as_deref(), Some("s1"));
}

#[tokio::test]
async fn a_checksum_mismatch_leaves_current_alone() {
    let up = Upstream::start().await;
    up.dnf("rocky", "s1", &[("a/a-1.rpm", b"rpm-a")]);
    let relay = tempfile::tempdir().unwrap();
    let sync = syncer(&up, relay.path());
    let fetcher = up.fetcher();
    sync.sync(&entry(&fetcher, "rocky").await).await.unwrap();

    // The package changes after the metadata was written: same size,
    // other bytes.
    up.dnf("rocky", "s2", &[("a/a-2.rpm", b"rpm-2")]);
    let rpm = up
        .snapshot_dir(Family::Dnf, "rocky", "s2")
        .join("Packages/a/a-2.rpm");
    std::fs::write(&rpm, b"evil!").unwrap();
    let error = sync
        .sync(&entry(&fetcher, "rocky").await)
        .await
        .unwrap_err();
    assert!(
        matches!(error, SyncError::Fetch(FetchError::Mismatch { .. })),
        "{error}"
    );
    let dnf = relay.path().join("dnf/rocky");
    assert_eq!(read_current(&dnf).as_deref(), Some("s1"));
    assert!(!dnf.join("s2").exists());
    assert!(!dnf.join(".sync-s2/Packages/a/a-2.rpm").exists());
    assert!(!dnf.join(".sync-s2/Packages/a/.a-2.rpm.part").exists());

    // A larger file is cut off at the listed size.
    std::fs::write(&rpm, b"rpm-2 and more").unwrap();
    let error = sync
        .sync(&entry(&fetcher, "rocky").await)
        .await
        .unwrap_err();
    assert!(
        matches!(error, SyncError::Fetch(FetchError::TooLarge { .. })),
        "{error}"
    );
    assert_eq!(read_current(&dnf).as_deref(), Some("s1"));
}

#[tokio::test]
async fn an_interrupted_copy_resumes() {
    let up = Upstream::start().await;
    let packages: Vec<(String, Vec<u8>)> = (0..8)
        .map(|i| (format!("p/p_{i}_all.deb"), vec![b'0' + i; 100]))
        .collect();
    let refs: Vec<(&str, &[u8])> = packages
        .iter()
        .map(|(n, c)| (n.as_str(), c.as_slice()))
        .collect();
    up.apt("debian", "s1", &refs);
    // The upstream lost one package: the copy stops there.
    let lost = up
        .snapshot_dir(Family::Apt, "debian", "s1")
        .join("pool/main/p/p_7_all.deb");
    let kept = std::fs::read(&lost).unwrap();
    std::fs::remove_file(&lost).unwrap();
    let relay = tempfile::tempdir().unwrap();
    let sync = syncer(&up, relay.path());
    let fetcher = up.fetcher();
    let debian = entry(&fetcher, "debian").await;
    let error = sync.sync(&debian).await.unwrap_err();
    assert!(
        matches!(error, SyncError::Missing(ref p) if p.ends_with("p_7_all.deb")),
        "{error}"
    );
    let apt = relay.path().join("apt/debian");
    assert_eq!(read_current(&apt), None);
    let staging = apt.join(".sync-s1");
    assert!(staging.is_dir());

    // Debris of a download cut short, and a corrupted staged file.
    std::fs::write(staging.join("pool/main/p/.p_0_all.deb.part"), b"half").unwrap();
    let corrupt = staging.join("pool/main/p/p_1_all.deb");
    if corrupt.exists() {
        std::fs::remove_file(&corrupt).unwrap();
        std::fs::write(&corrupt, vec![b'x'; 100]).unwrap();
    }

    std::fs::write(&lost, kept).unwrap();
    let Outcome::Synced(stats) = sync.sync(&debian).await.unwrap() else {
        panic!("expected a copy");
    };
    // Every staged file that still matched was kept, the rest fetched
    // again: three roots, Packages and the corrupt and lost packages.
    assert!(stats.kept >= 1, "{stats:?}");
    assert_eq!(stats.kept + stats.downloaded, 3 + 1 + 8, "{stats:?}");
    assert_eq!(read_current(&apt).as_deref(), Some("s1"));
    assert_eq!(
        std::fs::read(apt.join("s1/pool/main/p/p_1_all.deb")).unwrap(),
        vec![b'1'; 100]
    );
    assert!(!apt.join("s1/pool/main/p/.p_0_all.deb.part").exists());
    assert!(!staging.exists());
}
