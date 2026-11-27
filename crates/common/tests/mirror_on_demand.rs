//! Fetching the packages of on-demand snapshots on their first request
//! (ADR-0142), against a fake upstream that counts its requests: kept and
//! served from the store, refused when they do not match, one download
//! for concurrent requests, nothing fetched outside the package list, and
//! the historical archive when the upstream lost a package.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::{StatusCode, Uri};
use monitorflow_common::mirror::demand::{OnDemand, Source};
use monitorflow_common::mirror::origin::{Origin, ORIGIN};
use monitorflow_common::mirror::packages::{PackageList, PACKAGES};
use monitorflow_common::mirror::repo::fetch::Fetcher;
use monitorflow_common::mirror::repo::meta::{Algorithm, Entry, Package};
use monitorflow_common::mirror::store::Store;
use monitorflow_common::mirror::{serve, swap_current, Family, Served};
use tokio::net::TcpListener;

/// Files of the fake upstream, by URL path.
type Files = Vec<(String, Vec<u8>)>;

/// The fake upstream's files, by URL path, and how many GETs each got.
#[derive(Clone, Default)]
struct Upstream {
    files: Arc<std::sync::Mutex<Files>>,
    hits: Arc<AtomicUsize>,
}

impl Upstream {
    fn put(&self, path: &str, body: &[u8]) {
        self.files.lock().unwrap().push((path.into(), body.into()));
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

async fn upstream_file(State(up): State<Upstream>, uri: Uri) -> (StatusCode, Vec<u8>) {
    up.hits.fetch_add(1, Ordering::SeqCst);
    // Slow enough for concurrent requests to meet.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let found = up
        .files
        .lock()
        .unwrap()
        .iter()
        .find(|(p, _)| p == uri.path())
        .map(|(_, b)| b.clone());
    match found {
        Some(body) => (StatusCode::OK, body),
        None => (StatusCode::NOT_FOUND, Vec::new()),
    }
}

fn package(path: &str, body: &[u8]) -> Package {
    let mut hasher = Algorithm::Sha256.hasher();
    hasher.update(body);
    Package {
        entry: Entry {
            path: path.into(),
            size: body.len() as u64,
            checksum: hasher.finish(),
        },
        name: path.rsplit('/').next().unwrap().into(),
        version: "1".into(),
        arch: "amd64".into(),
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    up: Upstream,
    up_base: String,
    base: String,
    store: Store,
    snapshot: std::path::PathBuf,
}

/// A served on-demand apt snapshot listing `a.deb` (right content upstream),
/// `bad.deb` (upstream sends other bytes) and `old.deb` (only in the
/// archive), and a listener over it.
async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let up = Upstream::default();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let up_base = format!("http://{}", listener.local_addr().unwrap());
    let app = axum::Router::new()
        .fallback(upstream_file)
        .with_state(up.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let served = Served::under(dir.path());
    let mirror = served.root(Family::Apt).join("debian");
    let snapshot = mirror.join("s1");
    std::fs::create_dir_all(snapshot.join("dists/bookworm")).unwrap();
    std::fs::write(snapshot.join("dists/bookworm/InRelease"), "signed").unwrap();
    swap_current(&mirror, "s1").unwrap();
    PackageList::new(vec![
        package("pool/main/a/a.deb", b"deb a"),
        package("pool/main/b/bad.deb", b"deb b"),
        package("pool/main/o/old.deb", b"deb old"),
    ])
    .unwrap()
    .write(&snapshot.join(PACKAGES))
    .unwrap();
    Origin {
        upstream: format!("{up_base}/debian"),
        archives: vec![format!("{up_base}/gone"), format!("{up_base}/archive/x/")],
    }
    .write(&snapshot.join(ORIGIN))
    .unwrap();
    up.put("/debian/pool/main/a/a.deb", b"deb a");
    up.put("/debian/pool/main/b/bad.deb", b"deb B");
    up.put("/archive/x/pool/main/o/old.deb", b"deb old");

    let store = Store::under(dir.path());
    store.prepare().unwrap();
    let client = Fetcher::with_agent("http://unused", Duration::from_secs(5), "test").unwrap();
    let ondemand = OnDemand::new(store.clone(), Source::Origin(client));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(serve::serve_with(
        listener,
        served,
        Some(ondemand),
        std::future::pending(),
    ));
    Fixture {
        _dir: dir,
        up,
        up_base,
        base,
        store,
        snapshot,
    }
}

async fn get(base: &str, path: &str) -> (StatusCode, Vec<u8>) {
    let response = reqwest::get(format!("{base}{path}")).await.unwrap();
    let status = response.status();
    (status, response.bytes().await.unwrap().to_vec())
}

fn stored(store: &Store, body: &[u8]) -> bool {
    let mut hasher = Algorithm::Sha256.hasher();
    hasher.update(body);
    store.package_path(&hasher.finish()).is_file()
}

#[tokio::test]
async fn a_package_is_fetched_once_kept_and_served_from_the_store() {
    let fx = fixture().await;
    let path = "/apt/debian/current/pool/main/a/a.deb";
    assert_eq!(
        get(&fx.base, path).await,
        (StatusCode::OK, b"deb a".to_vec())
    );
    assert_eq!(fx.up.hits(), 1);
    assert!(stored(&fx.store, b"deb a"));
    // By its snapshot's name too, and without the upstream.
    fx.up.files.lock().unwrap().clear();
    assert_eq!(
        get(&fx.base, "/apt/debian/s1/pool/main/a/a.deb").await,
        (StatusCode::OK, b"deb a".to_vec())
    );
    assert_eq!(fx.up.hits(), 1);
    // The snapshot's own files are served as before.
    assert_eq!(
        get(&fx.base, "/apt/debian/s1/dists/bookworm/InRelease")
            .await
            .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn concurrent_requests_share_one_download() {
    let fx = fixture().await;
    let requests: Vec<_> = (0..8)
        .map(|_| {
            let base = fx.base.clone();
            tokio::spawn(async move { get(&base, "/apt/debian/current/pool/main/a/a.deb").await })
        })
        .collect();
    for request in requests {
        assert_eq!(request.await.unwrap(), (StatusCode::OK, b"deb a".to_vec()));
    }
    assert_eq!(fx.up.hits(), 1);
}

#[tokio::test]
async fn a_package_that_does_not_match_its_index_is_neither_served_nor_kept() {
    let fx = fixture().await;
    let (status, body) = get(&fx.base, "/apt/debian/current/pool/main/b/bad.deb").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(body.is_empty());
    assert!(!stored(&fx.store, b"deb b") && !stored(&fx.store, b"deb B"));
    // Nothing is left in the temporary directory either.
    let temp = fx.store.root().join(".tmp");
    assert_eq!(std::fs::read_dir(temp).unwrap().count(), 0);
}

#[tokio::test]
async fn only_what_the_package_list_names_is_fetched() {
    let fx = fixture().await;
    fx.up.put("/debian/pool/main/x/unlisted.deb", b"x");
    for path in [
        "/apt/debian/current/pool/main/x/unlisted.deb",
        "/apt/debian/s2/pool/main/a/a.deb",
        "/apt/other/current/pool/main/a/a.deb",
        "/dnf/debian/current/pool/main/a/a.deb",
        "/apt/debian/current/.packages",
        "/apt/debian/current/.origin",
    ] {
        assert_eq!(get(&fx.base, path).await.0, StatusCode::NOT_FOUND, "{path}");
    }
    assert_eq!(fx.up.hits(), 0);
}

#[tokio::test]
async fn a_package_gone_upstream_comes_from_the_archive() {
    let fx = fixture().await;
    let (status, body) = get(&fx.base, "/apt/debian/current/pool/main/o/old.deb").await;
    assert_eq!((status, body), (StatusCode::OK, b"deb old".to_vec()));
    // The upstream, the first archive, then the second.
    assert_eq!(fx.up.hits(), 3);
    // Nowhere at all: 404.
    std::fs::remove_file(fx.snapshot.join(ORIGIN)).unwrap();
    Origin {
        upstream: format!("{}/debian", fx.up_base),
        archives: vec![],
    }
    .write(&fx.snapshot.join(ORIGIN))
    .unwrap();
    fx.store
        .remove_package(&package("x", b"deb old").entry.checksum)
        .unwrap();
    assert_eq!(
        get(&fx.base, "/apt/debian/current/pool/main/o/old.deb")
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

fn sha256(body: &[u8]) -> String {
    let mut hasher = Algorithm::Sha256.hasher();
    hasher.update(body);
    hasher.finish().hex().to_owned()
}

/// Serves `served` with `ondemand` on a new listener; returns its URL.
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

#[tokio::test]
async fn msi_files_are_served_by_digest_and_relays_fetch_them_checked() {
    let fx = fixture().await;
    let body = b"an MSI file".as_slice();
    let digest = sha256(body);
    let path = format!("/software/{digest}.msi");

    // The server serves uploaded files and fetches none.
    let server_dir = tempfile::tempdir().unwrap();
    let served = Served::under(server_dir.path());
    std::fs::create_dir_all(&served.software).unwrap();
    std::fs::write(served.software.join(format!("{digest}.msi")), body).unwrap();
    let client = Fetcher::with_agent("http://unused", Duration::from_secs(5), "test").unwrap();
    let server = listen(
        served,
        OnDemand::new(Store::under(server_dir.path()), Source::Origin(client)),
    )
    .await;
    assert_eq!(get(&server, &path).await, (StatusCode::OK, body.to_vec()));
    let other = sha256(b"other");
    assert_eq!(
        get(&server, &format!("/software/{other}.msi")).await.0,
        StatusCode::NOT_FOUND
    );
    for bad in [
        "/software/x.msi",
        "/software/../keys/x",
        &format!("/software/{}.msi", digest.to_uppercase()),
    ] {
        assert_eq!(get(&server, bad).await.0, StatusCode::NOT_FOUND, "{bad}");
    }

    // A relay fetches from its upstream once, keeps the file and serves it.
    fx.up.put(&path, body);
    let liar = sha256(b"what the name says");
    fx.up
        .put(&format!("/software/{liar}.msi"), b"something else");
    let relay_dir = tempfile::tempdir().unwrap();
    let store = Store::under(relay_dir.path());
    store.prepare().unwrap();
    let upstream = Fetcher::new(&fx.up_base, Duration::from_secs(5)).unwrap();
    let relay = listen(
        Served::under(relay_dir.path()),
        OnDemand::new(store.clone(), Source::Listener(upstream)),
    )
    .await;
    let before = fx.up.hits();
    assert_eq!(get(&relay, &path).await, (StatusCode::OK, body.to_vec()));
    assert_eq!(get(&relay, &path).await, (StatusCode::OK, body.to_vec()));
    assert_eq!(fx.up.hits(), before + 1);
    assert!(stored(&store, body));
    // A file whose digest is not its name is neither served nor kept.
    assert_eq!(
        get(&relay, &format!("/software/{liar}.msi")).await.0,
        StatusCode::BAD_GATEWAY
    );
    assert!(!stored(&store, b"something else"));
    assert_eq!(
        get(&relay, &format!("/software/{other}.msi")).await.0,
        StatusCode::NOT_FOUND
    );
}
