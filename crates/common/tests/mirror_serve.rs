//! The shared mirror listener (ADR-0030, ADR-0034) over the relay's
//! layout: the fixed URLs, `/index.json`, and what it refuses.

use std::path::Path;

use monitorflow_common::mirror::{serve, swap_current, Family, Index, Served};
use reqwest::{Method, StatusCode};
use tokio::net::TcpListener;

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
