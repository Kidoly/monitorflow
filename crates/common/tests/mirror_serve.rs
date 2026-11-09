//! The shared mirror listener (ADR-0030, ADR-0034) over the relay's
//! layout: the fixed URLs, `/index.json`, and what it refuses.

use std::path::Path;

use axum::body::Body;
use axum::http::Request;
use monitorflow_common::mirror::{serve, swap_current, Family, Index, Served};
use reqwest::{Method, StatusCode};
use tokio::net::TcpListener;
use tower::ServiceExt;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[tokio::test]
async fn serves_the_layout_and_its_index() {
    let dir = tempfile::tempdir().unwrap();
    let served = Served::under(dir.path());
    let apt = served.root(Family::Apt).join("debian");
    write(&apt.join("s1/dists/bookworm/InRelease"), "signed release");
    write(&apt.join("s1/pool/main/a/a/a_1.0+b1~x_amd64.deb"), "deb");
    swap_current(&apt, "s1").unwrap();
    let dnf = served.root(Family::Dnf).join("rocky");
    write(&dnf.join("s1/repodata/repomd.xml"), "<repomd/>");
    write(&dnf.join(".sync-s2/repodata/repomd.xml"), "half done");
    write(&dnf.join("s1/.manifest"), "private");
    swap_current(&dnf, "s1").unwrap();
    write(&served.public_key, "-----BEGIN PGP PUBLIC KEY BLOCK-----");
    write(&dir.path().join("status.json"), "private");

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(serve::serve(listener, served, std::future::pending()));
    let client = reqwest::Client::new();
    let get = |path: &str| {
        let request = client.get(format!("{base}{path}"));
        async move {
            let response = request.send().await.unwrap();
            let status = response.status();
            (status, response.text().await.unwrap())
        }
    };

    for (path, body) in [
        (
            "/apt/debian/current/dists/bookworm/InRelease",
            "signed release",
        ),
        (
            "/apt/debian/s1/pool/main/a/a/a_1.0%2Bb1%7Ex_amd64.deb",
            "deb",
        ),
        ("/dnf/rocky/current/repodata/repomd.xml", "<repomd/>"),
        (
            "/keys/monitorflow-archive.asc",
            "-----BEGIN PGP PUBLIC KEY BLOCK-----",
        ),
    ] {
        assert_eq!(get(path).await, (StatusCode::OK, body.to_owned()), "{path}");
    }

    let (status, body) = get("/index.json").await;
    assert_eq!(status, StatusCode::OK);
    let index = Index::parse(body.as_bytes()).unwrap();
    let listed: Vec<_> = index
        .mirrors
        .iter()
        .map(|m| (m.family, m.name.as_str(), m.current.as_str()))
        .collect();
    assert_eq!(
        listed,
        [(Family::Apt, "debian", "s1"), (Family::Dnf, "rocky", "s1")]
    );
    assert_eq!(index.mirrors[0].distributions, ["bookworm"]);

    for path in [
        "/dnf/rocky/.sync-s2/repodata/repomd.xml",
        "/dnf/rocky/%2Esync-s2/repodata/repomd.xml",
        "/dnf/rocky/s1/.manifest",
        "/apt/../status.json",
        "/apt/%2E%2E/status.json",
        "/status.json",
        "/keys/other.asc",
        "/",
    ] {
        assert_eq!(get(path).await.0, StatusCode::NOT_FOUND, "{path}");
    }
    for path in ["/apt/debian/current/", "/dnf/rocky/s1/repodata", "/apt/"] {
        let (status, body) = get(path).await;
        assert!(!body.contains("repomd.xml") && !body.contains("dists"));
        assert_ne!(status, StatusCode::OK, "{path}");
    }
    let post = client
        .request(Method::POST, format!("{base}/index.json"))
        .send()
        .await
        .unwrap();
    assert_eq!(post.status(), StatusCode::METHOD_NOT_ALLOWED);
}

/// Through the router rather than a client: `reqwest` normalizes `..` and
/// `%2E%2E` away, and the listener must refuse them as sent.
async fn raw(served: &Served, method: &str, uri: &str) -> (u16, String) {
    let response = serve::router(served)
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status().as_u16();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::test]
async fn missing_files_directories_and_bad_paths_are_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let served = Served::under(dir.path());
    let apt = served.root(Family::Apt).join("debian");
    write(&apt.join("s1/dists/bookworm/InRelease"), "signed release");
    write(&dir.path().join("status.json"), "private");
    // No public key and no `dnf` or `agent` directory: a relay before its
    // first sync.

    assert_eq!(
        raw(&served, "GET", "/apt/debian/s1/dists/bookworm/InRelease").await,
        (200, "signed release".to_owned())
    );

    for uri in [
        // Missing, under roots that exist or not.
        "/apt/debian/s1/missing",
        "/apt/missing/s1/InRelease",
        "/dnf/rocky/s1/repodata/repomd.xml",
        "/agent/0.3.0/install.sh",
        "/keys/monitorflow-archive.asc",
        // A file used as a directory.
        "/apt/debian/s1/dists/bookworm/InRelease/child",
        "/apt/debian/s1/dists/bookworm/InRelease/",
        // Directories: no listing and no redirect, with or without a slash.
        "/apt/",
        "/apt/debian",
        "/apt/debian/",
        "/apt/debian/s1/dists",
        "/apt/debian/s1/dists/",
        // Out of the root, plain and encoded.
        "/apt/../status.json",
        "/apt/%2E%2E/status.json",
        "/apt/%2e%2e%2fstatus.json",
        "/apt/debian/..%2f..%2f..%2fstatus.json",
        "/apt/..%5cstatus.json",
        "/apt/%2Fetc%2Fpasswd",
    ] {
        for method in ["GET", "HEAD"] {
            let (status, body) = raw(&served, method, uri).await;
            assert_eq!(status, 404, "{method} {uri}");
            assert_eq!(body, "", "{method} {uri}");
        }
    }
}

/// The tree is written by tools that parse upstream data, next to keys
/// the listener's process can read: a link out of a served directory is
/// not followed, `current` is.
#[cfg(unix)]
#[tokio::test]
async fn links_out_of_the_served_directories_are_not_found() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("mirrors");
    let served = Served::under(&base);
    let ca = dir.path().join("ca");
    write(&ca.join("key.pem"), "secret key");
    write(&ca.join("monitorflow-archive.asc"), "secret key");
    let apt = served.root(Family::Apt).join("debian");
    write(&apt.join("s1/pool/a.deb"), "0123456789");
    write(&apt.join("s1/repomd.xml"), "<repomd/>");
    swap_current(&apt, "s1").unwrap();
    symlink("pool/a.deb", apt.join("s1/alias.deb")).unwrap();
    symlink("../../../../ca/key.pem", apt.join("s1/relative")).unwrap();
    symlink(ca.join("key.pem"), apt.join("s1/absolute")).unwrap();
    symlink(&ca, apt.join("s1/directory")).unwrap();
    symlink(&ca, apt.join("linked")).unwrap();
    // Whole served directories replaced by links.
    symlink(&ca, &served.dnf).unwrap();
    symlink(&ca, &served.agent).unwrap();
    symlink(&ca, served.public_key.parent().unwrap()).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(serve::serve(listener, served, std::future::pending()));
    let client = reqwest::Client::new();

    for path in [
        "/apt/debian/s1/pool/a.deb",
        "/apt/debian/current/pool/a.deb",
        "/apt/debian/current/alias.deb",
    ] {
        let response = client
            .get(format!("{base_url}{path}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(response.text().await.unwrap(), "0123456789", "{path}");
    }
    // Content types go by the name asked for, and downloads still resume.
    let xml = client
        .get(format!("{base_url}/apt/debian/current/repomd.xml"))
        .send()
        .await
        .unwrap();
    assert_eq!(xml.headers()["content-type"], "text/xml");
    let part = client
        .get(format!("{base_url}/apt/debian/current/pool/a.deb"))
        .header("range", "bytes=4-")
        .send()
        .await
        .unwrap();
    assert_eq!(part.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(part.text().await.unwrap(), "456789");

    for path in [
        "/apt/debian/s1/relative",
        "/apt/debian/s1/absolute",
        "/apt/debian/s1/directory/key.pem",
        "/apt/debian/current/directory/key.pem",
        "/apt/linked/key.pem",
        "/dnf/key.pem",
        "/agent/key.pem",
        "/keys/monitorflow-archive.asc",
    ] {
        for method in [Method::GET, Method::HEAD] {
            let response = client
                .request(method.clone(), format!("{base_url}{path}"))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {path}");
            assert_eq!(response.text().await.unwrap(), "", "{method} {path}");
        }
    }
}
