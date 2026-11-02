//! What a relay refuses from its upstream (ADR-0083): metadata that
//! expands past the limit, an index that does not match its listing, a
//! snapshot without its signatures, and responses larger than expected.
//! None of them moves `current`.

mod mirror_fixture;

use std::path::Path;
use std::time::{Duration, Instant};

use mirror_fixture::{compress, entry, sha256, write, Upstream};
use monitorflow_common::mirror::{read_current, Family};
use monitorflow_relay::mirror::compress::{Compression, MAX_DECOMPRESSED};
use monitorflow_relay::mirror::fetch::{Expect, FetchError, Fetcher};
use monitorflow_relay::mirror::meta::{Algorithm, Checksum, Entry, MetaError};
use monitorflow_relay::mirror::sync::{Outcome, SyncError, Syncer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn syncer(upstream: &Upstream, root: &Path) -> Syncer {
    Syncer::new(upstream.fetcher(), root.to_owned(), 3)
}

/// Replaces the `Packages.gz` of suite `bookworm` of `mirror` with `raw`,
/// and lists it alone in `Release` and `InRelease` with its checksum.
fn replace_list(up: &Upstream, mirror: &str, raw: &[u8]) {
    let dists = up
        .snapshot_dir(Family::Apt, mirror, "s1")
        .join("dists/bookworm");
    let name = "main/binary-amd64/Packages.gz";
    write(&dists.join(name), raw);
    let release = format!("SHA256:\n {} {} {name}\n", sha256(raw), raw.len());
    write(&dists.join("Release"), release.as_bytes());
    write(&dists.join("InRelease"), release.as_bytes());
}

#[tokio::test]
async fn a_decompression_bomb_fails_the_sync() {
    let up = Upstream::start().await;
    up.apt_lists(
        "debian",
        "s1",
        &[("a/a_1_amd64.deb", b"aaa")],
        &[(Compression::Gzip, true)],
    );
    // Concatenated gzip members of 1 MiB each, a few kilobytes on the
    // wire, one more than the limit once decompressed.
    let line = format!("Description: {}\n", "x".repeat(1010));
    let member = compress(Compression::Gzip, line.repeat(1024).as_bytes());
    let bomb = member.repeat((MAX_DECOMPRESSED >> 20) as usize + 1);
    assert!(bomb.len() < 4 << 20, "{}", bomb.len());
    replace_list(&up, "debian", &bomb);

    let relay = tempfile::tempdir().unwrap();
    let error = syncer(&up, relay.path())
        .sync(&entry(&up.fetcher(), "debian").await)
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            SyncError::Meta(MetaError::TooLarge { ref file, limit })
                if file.ends_with("Packages.gz") && limit == MAX_DECOMPRESSED
        ),
        "{error}"
    );
    assert!(error.to_string().contains("512 MiB"), "{error}");
    assert_eq!(read_current(&relay.path().join("apt/debian")), None);
}

#[tokio::test]
async fn a_tampered_index_is_never_read() {
    let up = Upstream::start().await;
    up.apt_lists(
        "debian",
        "s1",
        &[("a/a_1_amd64.deb", b"aaa")],
        &[(Compression::Gzip, true)],
    );
    let list = up
        .snapshot_dir(Family::Apt, "debian", "s1")
        .join("dists/bookworm/main/binary-amd64/Packages.gz");
    // Same size, other bytes: a list naming a file the Release does not
    // vouch for.
    let mut tampered = std::fs::read(&list).unwrap();
    let last = tampered.len() - 1;
    tampered[last] ^= 1;
    std::fs::write(&list, tampered).unwrap();

    let relay = tempfile::tempdir().unwrap();
    let error = syncer(&up, relay.path())
        .sync(&entry(&up.fetcher(), "debian").await)
        .await
        .unwrap_err();
    assert!(
        matches!(error, SyncError::Fetch(FetchError::Mismatch { ref url, .. }) if url.ends_with("Packages.gz")),
        "{error}"
    );
    let apt = relay.path().join("apt/debian");
    assert_eq!(read_current(&apt), None);
    assert!(!apt
        .join(".sync-s1/dists/bookworm/main/binary-amd64/Packages.gz")
        .exists());
}

#[tokio::test]
async fn a_snapshot_without_its_signatures_is_refused() {
    let up = Upstream::start().await;
    let relay = tempfile::tempdir().unwrap();
    let sync = syncer(&up, relay.path());
    let fetcher = up.fetcher();
    let packages: &[(&str, &[u8])] = &[("a/a-1.rpm", b"rpm-a")];

    for (family, mirror, signature) in [
        (Family::Dnf, "rocky", "repodata/repomd.xml.asc"),
        (Family::Apt, "debian", "dists/bookworm/InRelease"),
        (Family::Apt, "ubuntu", "dists/bookworm/Release.gpg"),
    ] {
        match family {
            Family::Dnf => up.dnf(mirror, "s1", packages),
            Family::Apt => up.apt(mirror, "s1", packages),
        }
        std::fs::remove_file(up.snapshot_dir(family, mirror, "s1").join(signature)).unwrap();
        let error = sync.sync(&entry(&fetcher, mirror).await).await.unwrap_err();
        assert!(
            matches!(error, SyncError::Unsigned(ref p) if p.ends_with(signature)),
            "{error}"
        );
        assert!(error.to_string().contains("not signed"), "{error}");
        let dir = relay.path().join(family.as_str()).join(mirror);
        assert_eq!(read_current(&dir), None, "{mirror}");
    }
}

/// The relay cannot check signatures (ADR-0034): it copies a bad one
/// unchanged, and the agents' apt and dnf refuse it.
#[tokio::test]
async fn a_bad_signature_is_copied_unchanged_for_agents_to_refuse() {
    let up = Upstream::start().await;
    up.dnf("rocky", "s1", &[("a/a-1.rpm", b"rpm-a")]);
    let asc = "repodata/repomd.xml.asc";
    let snapshot = up.snapshot_dir(Family::Dnf, "rocky", "s1");
    std::fs::write(snapshot.join(asc), b"not a signature").unwrap();

    let relay = tempfile::tempdir().unwrap();
    let outcome = syncer(&up, relay.path())
        .sync(&entry(&up.fetcher(), "rocky").await)
        .await
        .unwrap();
    assert!(matches!(outcome, Outcome::Synced(_)));
    let copied = relay.path().join("dnf/rocky/current").join(asc);
    assert_eq!(std::fs::read(copied).unwrap(), b"not a signature");
}

/// A listener that answers every request with `head` and then `body`,
/// and keeps the connection open unless `close`.
async fn raw_upstream(head: &'static str, body: Vec<u8>, close: bool) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let body = body.clone();
            tokio::spawn(async move {
                let mut request = [0; 4096];
                let _ = stream.read(&mut request).await;
                stream.write_all(head.as_bytes()).await.unwrap();
                let _ = stream.write_all(&body).await;
                if !close {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                }
            });
        }
    });
    base
}

#[tokio::test]
async fn an_announced_size_over_the_limit_is_refused_before_the_body() {
    // Announces a gigabyte and sends nothing: waiting for the body would
    // run into the 30 s timeout.
    let base = raw_upstream(
        "HTTP/1.1 200 OK\r\nContent-Length: 1073741824\r\n\r\n",
        Vec::new(),
        false,
    )
    .await;
    let fetcher = Fetcher::new(&base, Duration::from_secs(30)).unwrap();
    let started = Instant::now();

    let error = fetcher.bytes("index.json", 1 << 20).await.unwrap_err();
    assert!(
        matches!(error, FetchError::TooLarge { limit, .. } if limit == 1 << 20),
        "{error}"
    );
    assert!(error.to_string().contains("larger than the 1 MiB expected"));

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("a.rpm");
    let listed = Entry {
        path: "a.rpm".into(),
        size: 5,
        checksum: Checksum::new(Algorithm::Sha256, &sha256(b"rpm-a")).unwrap(),
    };
    let error = fetcher
        .file("a.rpm", &dest, Expect::Entry(&listed))
        .await
        .unwrap_err();
    assert!(
        matches!(error, FetchError::TooLarge { limit: 5, .. }),
        "{error}"
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(!dest.exists());
}

#[tokio::test]
async fn a_body_without_a_length_is_cut_off_at_the_limit() {
    let base = raw_upstream(
        "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n",
        vec![b'x'; 64 * 1024],
        true,
    )
    .await;
    let fetcher = Fetcher::new(&base, Duration::from_secs(30)).unwrap();
    let error = fetcher.bytes("index.json", 1000).await.unwrap_err();
    assert!(
        matches!(error, FetchError::TooLarge { limit: 1000, .. }),
        "{error}"
    );
    let dir = tempfile::tempdir().unwrap();
    let error = fetcher
        .file("x", &dir.path().join("x"), Expect::AtMost(1000))
        .await
        .unwrap_err();
    assert!(
        matches!(error, FetchError::TooLarge { limit: 1000, .. }),
        "{error}"
    );
}
