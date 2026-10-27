use std::time::Duration;

use monitorflow_common::mirror::serve::{self, Served};
use monitorflow_common::release::sha256;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use tempfile::TempDir;

use super::*;

const LINUX: &str = "monitorflow-agent-linux-x86_64";

struct Upstream {
    root: TempDir,
    pair: Ed25519KeyPair,
    fetcher: Fetcher,
}

impl Upstream {
    /// A mirror listener over an empty tree.
    async fn start() -> Self {
        let root = TempDir::new().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let served = Served::under(root.path());
        tokio::spawn(serve::serve(listener, served, std::future::pending()));
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        Self {
            root,
            pair: Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap(),
            fetcher: Fetcher::new(&url, Duration::from_secs(10)).unwrap(),
        }
    }

    fn key(&self) -> PublicKey {
        let mut der = vec![
            0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
        ];
        der.extend_from_slice(self.pair.public_key().as_ref());
        PublicKey::from_spki(&der).unwrap()
    }

    /// Publishes a signed release of `version` with `binary`.
    fn publish(&self, version: &str, binary: &[u8]) {
        let dir = self.root.path().join("agent").join(version);
        std::fs::create_dir_all(&dir).unwrap();
        let version_file = format!("{version}\n");
        let hex = |b: &[u8]| -> String { sha256(b).iter().map(|x| format!("{x:02x}")).collect() };
        let sums = format!(
            "{}  {LINUX}\n{}  VERSION\n",
            hex(binary),
            hex(version_file.as_bytes())
        );
        std::fs::write(dir.join(LINUX), binary).unwrap();
        std::fs::write(dir.join("VERSION"), &version_file).unwrap();
        std::fs::write(dir.join("SHA256SUMS"), &sums).unwrap();
        std::fs::write(dir.join("SHA256SUMS.sig"), self.pair.sign(sums.as_bytes())).unwrap();
    }
}

fn listed(versions: &[&str]) -> Vec<String> {
    versions.iter().map(|v| v.to_string()).collect()
}

fn held(dir: &Path) -> Vec<String> {
    cached_versions(dir)
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect()
}

#[tokio::test]
async fn copies_the_newest_releases_and_drops_older_ones() {
    let up = Upstream::start().await;
    for v in ["0.1.0", "0.2.0", "0.3.0"] {
        up.publish(v, v.as_bytes());
    }
    let relay = TempDir::new().unwrap();
    let dir = relay.path().join("agent");
    let copied = sync(
        &up.fetcher,
        &dir,
        &listed(&["0.1.0", "0.3.0", "0.2.0"]),
        &[up.key()],
    )
    .await
    .unwrap();
    assert_eq!(copied.len(), 2);
    assert_eq!(held(&dir), ["0.3.0", "0.2.0"]);
    assert_eq!(
        std::fs::read(dir.join("0.3.0").join(LINUX)).unwrap(),
        b"0.3.0"
    );

    // Nothing to copy again; a newer release replaces the oldest.
    let none = sync(&up.fetcher, &dir, &listed(&["0.3.0", "0.2.0"]), &[up.key()]).await;
    assert_eq!(none.unwrap(), []);
    up.publish("0.4.0", b"0.4.0");
    sync(
        &up.fetcher,
        &dir,
        &listed(&["0.4.0", "0.3.0", "0.2.0"]),
        &[up.key()],
    )
    .await
    .unwrap();
    assert_eq!(held(&dir), ["0.4.0", "0.3.0"]);
}

#[tokio::test]
async fn refuses_releases_that_do_not_verify() {
    let up = Upstream::start().await;
    up.publish("0.2.0", b"agent");
    let relay = TempDir::new().unwrap();
    let dir = relay.path().join("agent");
    let other = Upstream::start().await;
    let error = sync(&up.fetcher, &dir, &listed(&["0.2.0"]), &[other.key()])
        .await
        .unwrap_err();
    assert!(error.contains("does not verify"), "{error}");

    // A binary changed after signing.
    std::fs::write(up.root.path().join("agent/0.2.0").join(LINUX), b"evil").unwrap();
    let error = sync(&up.fetcher, &dir, &listed(&["0.2.0"]), &[up.key()])
        .await
        .unwrap_err();
    assert!(error.contains("checksum mismatch"), "{error}");
    assert!(held(&dir).is_empty());
    // Nothing half-copied is served.
    assert!(std::fs::read_dir(&dir).map_or(0, |d| d.count()) <= 1);
}
