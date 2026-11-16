//! On-demand mirrors in cascade (ADR-0142): a relay copies an on-demand
//! snapshot without its packages, then fetches each package through the
//! server on its agents' first request, the server fetching it from the
//! distribution in turn. Each keeps it, so the next request stays local.
//! A relay that ignores `on_demand`, as older ones do, still gets every
//! package through the server.

mod mirror_fixture;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::{StatusCode, Uri};
use mirror_fixture::{sha256, Upstream};
use monitorflow_common::mirror::demand::{OnDemand, Source};
use monitorflow_common::mirror::origin::{Origin, ORIGIN};
use monitorflow_common::mirror::packages::{PackageList, PACKAGES};
use monitorflow_common::mirror::repo::apt;
use monitorflow_common::mirror::repo::fetch::Fetcher;
use monitorflow_common::mirror::store::Store;
use monitorflow_common::mirror::{serve, Family, Index, Served};
use monitorflow_relay::mirror::cache;
use monitorflow_relay::mirror::sync::{Outcome, Syncer};
use tokio::net::TcpListener;

/// The distribution: `/debian/pool/main/<name>` holds `<name>`'s content.
#[derive(Clone)]
struct Distribution {
    hits: Arc<AtomicUsize>,
}

async fn distribution(State(d): State<Distribution>, uri: Uri) -> (StatusCode, Vec<u8>) {
    d.hits.fetch_add(1, Ordering::SeqCst);
    match uri.path() {
        "/debian/pool/main/a.deb" => (StatusCode::OK, b"package a".to_vec()),
        "/debian/pool/main/b.deb" => (StatusCode::OK, b"package b".to_vec()),
        _ => (StatusCode::NOT_FOUND, Vec::new()),
    }
}

async fn listen(served: Served, ondemand: OnDemand) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(serve::serve_with(
        listener,
        served,
        Some(ondemand),
        std::future::pending(),
    ));
    base
}

async fn get(base: &str, path: &str) -> (StatusCode, Vec<u8>) {
    let response = reqwest::get(format!("{base}{path}")).await.unwrap();
    let status = response.status();
    (status, response.bytes().await.unwrap().to_vec())
}

fn stored(root: &Path, body: &[u8]) -> bool {
    let checksum = format!("sha256:{}", sha256(body)).parse().unwrap();
    Store::under(root).package_path(&checksum).is_file()
}

/// A server's tree with an on-demand apt snapshot of `a.deb` and `b.deb`,
/// whose packages are at the distribution `dist`, and its listener.
async fn server(dist: &str) -> (tempfile::TempDir, String) {
    // The fixture writes a full snapshot; its packages then leave it.
    let upstream = Upstream::start().await;
    upstream.apt(
        "debian",
        "s1",
        &[("a.deb", b"package a"), ("b.deb", b"package b")],
    );
    let snapshot = upstream.snapshot_dir(Family::Apt, "debian", "s1");
    let packages = snapshot.join("dists/bookworm/main/binary-amd64/Packages");
    let list = apt::read_package_list("Packages", &packages).unwrap();
    PackageList::new(list)
        .unwrap()
        .write(&snapshot.join(PACKAGES))
        .unwrap();
    Origin {
        upstream: format!("{dist}/debian"),
        archives: vec![],
    }
    .write(&snapshot.join(ORIGIN))
    .unwrap();
    std::fs::remove_dir_all(snapshot.join("pool")).unwrap();

    let dir = upstream.dir;
    let store = Store::under(dir.path());
    store.prepare().unwrap();
    let client = Fetcher::new("http://unused", Duration::from_secs(10)).unwrap();
    let base = listen(
        Served::under(dir.path()),
        OnDemand::new(store, Source::Origin(client)),
    )
    .await;
    (dir, base)
}

#[tokio::test]
async fn a_relay_copies_indexes_only_and_fetches_packages_through_the_server() {
    let hits = Arc::new(AtomicUsize::new(0));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dist = format!("http://{}", listener.local_addr().unwrap());
    let app = axum::Router::new()
        .fallback(distribution)
        .with_state(Distribution { hits: hits.clone() });
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (server_dir, server) = server(&dist).await;

    let to_server = Fetcher::new(&server, Duration::from_secs(10)).unwrap();
    let index = Index::parse(
        &to_server
            .bytes("index.json", 1 << 20)
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let entry = index.mirrors[0].clone();
    assert!(entry.on_demand);

    // The relay's copy: the signed indexes and its own package list, no
    // package.
    let relay_dir = tempfile::tempdir().unwrap();
    let relay = relay_dir.path();
    monitorflow_relay::mirror::prepare(relay).unwrap();
    let syncer = Syncer::new(to_server.clone(), relay.to_owned(), 4);
    assert!(matches!(
        syncer.sync(&entry).await.unwrap(),
        Outcome::Synced(_)
    ));
    let copy = relay.join("apt/debian/s1");
    assert!(copy.join("dists/bookworm/InRelease").is_file());
    assert!(!copy.join("pool").exists());
    let list = PackageList::read(&copy.join(PACKAGES)).unwrap().unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(hits.load(Ordering::SeqCst), 0);

    // An agent behind the relay installs `a`: relay, then server, then
    // the distribution, each keeping it.
    let (ondemand, _grew) = cache::ondemand(relay, to_server.clone());
    Store::under(relay).prepare().unwrap();
    let relay_base = listen(Served::under(relay), ondemand).await;
    let path = "/apt/debian/current/pool/main/a.deb";
    assert_eq!(
        get(&relay_base, path).await,
        (StatusCode::OK, b"package a".to_vec())
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert!(stored(relay, b"package a") && stored(server_dir.path(), b"package a"));
    assert_eq!(get(&relay_base, path).await.0, StatusCode::OK);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    // Not in the list: nobody is asked.
    assert_eq!(
        get(&relay_base, "/apt/debian/current/pool/main/x.deb")
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    // Eviction on the relay uses the same rules: `a` is in the current
    // snapshot and was used just now, so a tiny cap keeps it.
    let outcome = cache::evict_once(relay, 1).unwrap();
    assert!(outcome.over_cap);
    assert!(stored(relay, b"package a"));

    // An older relay ignores `on_demand` and copies every package, which
    // the server fetches as for any request.
    let old_dir = tempfile::tempdir().unwrap();
    monitorflow_relay::mirror::prepare(old_dir.path()).unwrap();
    let old = Syncer::new(to_server, old_dir.path().to_owned(), 4);
    let mut whole = entry.clone();
    whole.on_demand = false;
    assert!(matches!(
        old.sync(&whole).await.unwrap(),
        Outcome::Synced(_)
    ));
    let pool = old_dir.path().join("apt/debian/s1/pool/main");
    assert_eq!(std::fs::read(pool.join("a.deb")).unwrap(), b"package a");
    assert_eq!(std::fs::read(pool.join("b.deb")).unwrap(), b"package b");
    // `a` came from the server's store; only `b` from the distribution.
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}
