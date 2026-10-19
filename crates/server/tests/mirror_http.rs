//! The mirror listener (ADR-0030): the fixed URL layout, `current`
//! through its symlink, and what it refuses.

use std::path::Path;

use monitorflow_server::mirror::layout::{self, Layout};
use monitorflow_server::mirror::{http, Family};
use reqwest::{Method, StatusCode};
use tokio::net::TcpListener;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[tokio::test]
async fn serves_the_fixed_layout_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let layout = Layout::new(dir.path());
    layout.prepare().unwrap();
    let apt = layout.served(Family::Apt).join("debian");
    write(&apt.join("s1/dists/bookworm/InRelease"), "signed release");
    write(&apt.join("s1/pool/main/a/a/a_1.0+b1~x_amd64.deb"), "deb");
    layout::swap_current(&apt, "s1").unwrap();
    let dnf = layout.served(Family::Dnf).join("rocky");
    write(&dnf.join("s1/repodata/repomd.xml"), "<repomd/>");
    write(&dnf.join(".creating-s2/repodata/repomd.xml"), "half done");
    layout::swap_current(&dnf, "s1").unwrap();
    write(&layout.public_key(), "-----BEGIN PGP PUBLIC KEY BLOCK-----");
    write(&dir.path().join("dnf-work/rocky/private"), "private");
    write(&layout.gnupg().join("private-keys-v1.d/x.key"), "secret");

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(http::serve(
        listener,
        layout.clone(),
        std::future::pending(),
    ));
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
        ("/apt/debian/s1/dists/bookworm/InRelease", "signed release"),
        (
            "/apt/debian/current/dists/bookworm/InRelease",
            "signed release",
        ),
        (
            "/apt/debian/current/pool/main/a/a/a_1.0%2Bb1%7Ex_amd64.deb",
            "deb",
        ),
        ("/dnf/rocky/current/repodata/repomd.xml", "<repomd/>"),
        ("/dnf/rocky/s1/repodata/repomd.xml", "<repomd/>"),
        (
            "/keys/monitorflow-archive.asc",
            "-----BEGIN PGP PUBLIC KEY BLOCK-----",
        ),
    ] {
        assert_eq!(get(path).await, (StatusCode::OK, body.to_owned()), "{path}");
    }
    let head = client
        .head(format!("{base}/dnf/rocky/current/repodata/repomd.xml"))
        .send()
        .await
        .unwrap();
    assert_eq!(head.status(), StatusCode::OK);

    for path in [
        "/dnf/rocky/.creating-s2/repodata/repomd.xml",
        "/dnf/rocky/%2Ecreating-s2/repodata/repomd.xml",
        "/apt/../gnupg/private-keys-v1.d/x.key",
        "/apt/%2E%2E/%2E%2E/gnupg/private-keys-v1.d/x.key",
        "/gnupg/private-keys-v1.d/x.key",
        "/dnf-work/rocky/private",
        "/aptly.conf",
        "/keys/other.asc",
        "/apt/missing/current/",
        "/",
    ] {
        assert_eq!(get(path).await.0, StatusCode::NOT_FOUND, "{path}");
    }
    // No directory listings.
    for path in ["/apt/debian/current/", "/dnf/rocky/s1/repodata", "/apt/"] {
        let (status, body) = get(path).await;
        assert!(
            !body.contains("repomd.xml") && !body.contains("dists"),
            "{path}: {status} {body}"
        );
        assert_ne!(status, StatusCode::OK, "{path}");
    }
    let post = client
        .request(
            Method::POST,
            format!("{base}/apt/debian/current/dists/bookworm/InRelease"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(post.status(), StatusCode::METHOD_NOT_ALLOWED);
}
