//! A fake mirror upstream for the relay's sync tests: small apt and dnf
//! snapshots with real metadata, served by the shared mirror listener.

#![allow(dead_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use monitorflow_common::mirror::{serve, swap_current, Family, Index, IndexMirror, Served};
use monitorflow_relay::mirror::fetch::Fetcher;
use tokio::net::TcpListener;

pub fn sha256(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A mirror listener over a temporary tree.
pub struct Upstream {
    pub dir: tempfile::TempDir,
    pub served: Served,
    pub base: String,
}

impl Upstream {
    pub async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        Self::serve(dir).await
    }

    /// Serves an existing tree with the relay's layout.
    pub async fn serve(dir: tempfile::TempDir) -> Self {
        let served = Served::under(dir.path());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(serve::serve(
            listener,
            served.clone(),
            std::future::pending(),
        ));
        Self { dir, served, base }
    }

    pub fn fetcher(&self) -> Fetcher {
        Fetcher::new(&self.base, Duration::from_secs(10)).unwrap()
    }

    pub fn snapshot_dir(&self, family: Family, mirror: &str, snapshot: &str) -> PathBuf {
        self.served.root(family).join(mirror).join(snapshot)
    }

    /// An apt snapshot of suite `bookworm`, component `main`, with
    /// `packages` (path under `pool/`, content), published as `current`.
    /// `Release` also lists a `Packages.gz` that does not exist, as a
    /// compression the publisher did not generate.
    pub fn apt(&self, mirror: &str, snapshot: &str, packages: &[(&str, &[u8])]) {
        let root = self.snapshot_dir(Family::Apt, mirror, snapshot);
        let mut index = String::new();
        for (name, content) in packages {
            let path = format!("pool/main/{name}");
            write(&root.join(&path), content);
            index.push_str(&format!(
                "Package: {name}\nFilename: {path}\nSize: {}\nSHA256: {}\n\n",
                content.len(),
                sha256(content)
            ));
        }
        let dists = root.join("dists/bookworm");
        write(&dists.join("main/binary-amd64/Packages"), index.as_bytes());
        let release = format!(
            "Origin: MonitorFlow\nSuite: bookworm\nSHA256:\n {} {} main/binary-amd64/Packages\n {} 99 main/binary-amd64/Packages.gz\n",
            sha256(index.as_bytes()),
            index.len(),
            sha256(b"absent"),
        );
        write(&dists.join("Release"), release.as_bytes());
        write(&dists.join("Release.gpg"), b"signature");
        write(
            &dists.join("InRelease"),
            format!("signed\n{release}").as_bytes(),
        );
        swap_current(root.parent().unwrap(), snapshot).unwrap();
    }

    /// A dnf snapshot with `packages` (path under `Packages/`, content),
    /// gzip primary metadata, published as `current`.
    pub fn dnf(&self, mirror: &str, snapshot: &str, packages: &[(&str, &[u8])]) {
        let root = self.snapshot_dir(Family::Dnf, mirror, snapshot);
        let mut primary = String::from(
            r#"<?xml version="1.0"?><metadata xmlns="http://linux.duke.edu/metadata/common">"#,
        );
        for (name, content) in packages {
            let path = format!("Packages/{name}");
            write(&root.join(&path), content);
            primary.push_str(&format!(
                r#"<package type="rpm"><checksum type="sha256" pkgid="YES">{}</checksum><size package="{}"/><location href="{path}"/></package>"#,
                sha256(content),
                content.len()
            ));
        }
        primary.push_str("</metadata>");
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), Default::default());
        gz.write_all(primary.as_bytes()).unwrap();
        let gz = gz.finish().unwrap();
        let primary_path = format!("repodata/{}-primary.xml.gz", sha256(&gz));
        write(&root.join(&primary_path), &gz);
        let repomd = format!(
            r#"<?xml version="1.0"?><repomd><data type="primary"><checksum type="sha256">{}</checksum><location href="{primary_path}"/><size>{}</size></data></repomd>"#,
            sha256(&gz),
            gz.len()
        );
        write(&root.join("repodata/repomd.xml"), repomd.as_bytes());
        write(&root.join("repodata/repomd.xml.asc"), b"signature");
        swap_current(root.parent().unwrap(), snapshot).unwrap();
    }

    pub fn key(&self, text: &str) {
        write(&self.served.public_key, text.as_bytes());
    }
}

/// The index `base` publishes.
pub async fn index(fetcher: &Fetcher) -> Vec<IndexMirror> {
    let bytes = fetcher.bytes("index.json", 1 << 20).await.unwrap().unwrap();
    Index::parse(&bytes).unwrap().mirrors
}

/// The entry of `name` in the index of `fetcher`.
pub async fn entry(fetcher: &Fetcher, name: &str) -> IndexMirror {
    index(fetcher)
        .await
        .into_iter()
        .find(|m| m.name == name)
        .unwrap()
}
