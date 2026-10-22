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

/// Names of the staging directories of a mirror.
fn staging_dirs(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.starts_with(".sync-"))
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn failed_syncs_leave_one_staging_directory() {
    let up = Upstream::start().await;
    let packages: Vec<(String, Vec<u8>)> = (0..6)
        .map(|i| (format!("p/p_{i}_all.deb"), vec![b'0' + i; 100]))
        .collect();
    let refs = |extra: &'static str| {
        let mut refs: Vec<(&str, &[u8])> = packages
            .iter()
            .map(|(n, c)| (n.as_str(), c.as_slice()))
            .collect();
        refs.push((extra, extra.as_bytes()));
        refs
    };
    let relay = tempfile::tempdir().unwrap();
    let apt = relay.path().join("apt/debian");
    let sync = syncer(&up, relay.path());
    let fetcher = up.fetcher();

    // Every sync stops at the package the upstream lost, while current
    // moves s1 -> s2 -> s3 upstream.
    for (snapshot, extra) in [
        ("s1", "x/x_1_all.deb"),
        ("s2", "y/y_1_all.deb"),
        ("s3", "z/z_1_all.deb"),
    ] {
        up.apt("debian", snapshot, &refs(extra));
        let name = extra.rsplit('/').next().unwrap();
        let lost = up
            .snapshot_dir(Family::Apt, "debian", snapshot)
            .join(format!("pool/main/{}/{name}", &extra[..1]));
        std::fs::remove_file(&lost).unwrap();
        let error = sync
            .sync(&entry(&fetcher, "debian").await)
            .await
            .unwrap_err();
        assert!(matches!(error, SyncError::Missing(_)), "{error}");
        assert_eq!(staging_dirs(&apt), [format!(".sync-{snapshot}")]);
        std::fs::write(&lost, extra).unwrap();
    }
    // A leftover of an older relay with several staging directories.
    std::fs::create_dir_all(apt.join(".sync-s0/pool")).unwrap();

    // The last one completes from the adopted staging directory, and
    // keeps nothing of the earlier snapshots it did not list.
    let Outcome::Synced(stats) = sync.sync(&entry(&fetcher, "debian").await).await.unwrap() else {
        panic!("expected a copy");
    };
    assert!(stats.kept >= 1, "{stats:?}");
    assert_eq!(stats.kept + stats.downloaded, 3 + 1 + 7, "{stats:?}");
    assert!(staging_dirs(&apt).is_empty());
    assert_eq!(read_current(&apt).as_deref(), Some("s3"));
    let s3 = apt.join("s3/pool/main");
    assert!(s3.join("z/z_1_all.deb").is_file());
    assert!(!s3.join("x").exists() && !s3.join("y").exists());
}

#[tokio::test]
async fn a_snapshot_over_the_size_bound_fails_before_fetching() {
    let up = Upstream::start().await;
    up.apt("debian", "s1", &[("a/a_1_all.deb", &[b'a'; 100])]);
    let relay = tempfile::tempdir().unwrap();
    let sync = syncer(&up, relay.path()).with_max_snapshot_bytes(2000);
    let fetcher = up.fetcher();
    sync.sync(&entry(&fetcher, "debian").await).await.unwrap();

    // s2 lists 3000 bytes of packages: nothing of them is fetched.
    up.apt(
        "debian",
        "s2",
        &[
            ("a/a_1_all.deb", &[b'a'; 100]),
            ("b/b_1_all.deb", &[b'b'; 3000]),
        ],
    );
    let error = sync
        .sync(&entry(&fetcher, "debian").await)
        .await
        .unwrap_err();
    assert!(
        matches!(error, SyncError::TooLarge { limit: 2000 }),
        "{error}"
    );
    let apt = relay.path().join("apt/debian");
    assert_eq!(read_current(&apt).as_deref(), Some("s1"));
    assert!(!apt.join("s2").exists());
    assert!(!apt.join(".sync-s2/pool").exists());
}

#[tokio::test]
async fn manifests_hold_sha256_whatever_the_metadata_uses() {
    let up = Upstream::start().await;
    let packages: [(&str, &[u8]); 2] = [("a/a-1.rpm", b"rpm-a"), ("b/b-1.rpm", b"rpm-b")];
    up.dnf_with("sha512", "rocky", "s1", &packages);
    let relay = tempfile::tempdir().unwrap();
    let sync = syncer(&up, relay.path());
    let fetcher = up.fetcher();
    sync.sync(&entry(&fetcher, "rocky").await).await.unwrap();

    let dnf = relay.path().join("dnf/rocky");
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dnf.join("s1").join(MANIFEST)).unwrap()).unwrap();
    let files = manifest["files"].as_array().unwrap();
    assert_eq!(files.len(), 5);
    for file in files {
        let path = file["path"].as_str().unwrap();
        let sha256 = mirror_fixture::sha256(&std::fs::read(dnf.join("s1").join(path)).unwrap());
        assert_eq!(file["checksum"], format!("sha256:{sha256}"), "{path}");
        let signed = path.starts_with("repodata/repomd.xml");
        assert_eq!(file.get("listed").is_none(), signed, "{path}");
    }

    // The repository moves to SHA-256: the packages are linked, not
    // fetched; the metadata changed.
    up.dnf_with("sha256", "rocky", "s2", &packages);
    let Outcome::Synced(stats) = sync.sync(&entry(&fetcher, "rocky").await).await.unwrap() else {
        panic!("expected a copy");
    };
    assert_eq!((stats.downloaded, stats.linked), (3, 2), "{stats:?}");

    // A manifest from before this change, with the SHA-512 of each file
    // only, still lets the files be linked, after a local hash: the
    // packages and the unchanged primary metadata.
    let path = dnf.join("s2").join(MANIFEST);
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for file in manifest["files"].as_array_mut().unwrap() {
        let content = std::fs::read(dnf.join("s2").join(file["path"].as_str().unwrap())).unwrap();
        file["checksum"] = format!("sha512:{}", mirror_fixture::digest("sha512", &content)).into();
    }
    std::fs::write(&path, manifest.to_string()).unwrap();
    std::fs::remove_dir_all(dnf.join("s1")).unwrap();
    up.dnf_with("sha256", "rocky", "s3", &packages);
    let Outcome::Synced(stats) = sync.sync(&entry(&fetcher, "rocky").await).await.unwrap() else {
        panic!("expected a copy");
    };
    assert_eq!((stats.downloaded, stats.linked), (2, 3), "{stats:?}");
    let a = "Packages/a/a-1.rpm";
    assert_eq!(ino(&dnf.join("s2").join(a)), ino(&dnf.join("s3").join(a)));
}
