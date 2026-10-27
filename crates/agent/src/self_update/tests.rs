use std::sync::Arc;

use monitorflow_common::release::{sha256, PublicKey};
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use tempfile::TempDir;

use super::*;

/// A throwaway release signing key.
pub(crate) struct Signer(Ed25519KeyPair);

impl Signer {
    pub(crate) fn new() -> Self {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        Self(Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap())
    }

    pub(crate) fn public(&self) -> PublicKey {
        let mut der = vec![
            0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
        ];
        der.extend_from_slice(self.0.public_key().as_ref());
        PublicKey::from_spki(&der).unwrap()
    }

    /// The files of a release of `version` with `binary` under `asset`.
    pub(crate) fn release(&self, version: &str, asset: &str, binary: &[u8]) -> ReleaseFiles {
        let version_file = format!("{version}\n");
        let hex = |b: &[u8]| -> String { sha256(b).iter().map(|x| format!("{x:02x}")).collect() };
        let sums = format!(
            "{}  {asset}\n{}  VERSION\n",
            hex(binary),
            hex(version_file.as_bytes())
        );
        ReleaseFiles {
            signature: self.0.sign(sums.as_bytes()).as_ref().to_vec(),
            sums,
            version_file,
            binary: binary.to_vec(),
        }
    }
}

pub(crate) struct ReleaseFiles {
    pub sums: String,
    pub signature: Vec<u8>,
    pub version_file: String,
    pub binary: Vec<u8>,
}

impl ReleaseFiles {
    /// What the agent writes on the helper's stdin.
    pub(crate) fn input(&self, command_id: &str, health_timeout_ms: u64) -> Vec<u8> {
        use base64::Engine;
        let header = Header {
            command_id: command_id.into(),
            sums: self.sums.clone(),
            signature: base64::engine::general_purpose::STANDARD.encode(&self.signature),
            version_file: self.version_file.clone(),
            allow_downgrade: false,
            health_timeout_ms,
        };
        let mut input = serde_json::to_vec(&header).unwrap();
        input.push(b'\n');
        input.extend_from_slice(&self.binary);
        input
    }
}

/// A minimal x86_64 ELF header, then `tag` so binaries differ.
pub(crate) fn elf(tag: &str) -> Vec<u8> {
    let mut bytes = b"\x7fELF\x02\x01\x01\0\0\0\0\0\0\0\0\0\x02\0\x3e\0".to_vec();
    bytes.resize(64, 0);
    bytes.extend_from_slice(tag.as_bytes());
    bytes
}

fn status(phase: Phase, installed_at: i64) -> Status {
    Status {
        command_id: "c-1".into(),
        from: "0.2.0".into(),
        to: "0.3.0".into(),
        phase,
        data_dir: "/var/lib/monitorflow/agent".into(),
        health_timeout_ms: 300_000,
        installed_at_unix_ms: installed_at,
        reason: Some("0.3.0 did not connect within 300 s".into()),
    }
}

#[test]
fn outcomes_follow_the_watchdog() {
    let now = 1_000_000_000;
    let ok = status(Phase::Succeeded, now).outcome("0.3.0", now).unwrap();
    assert_eq!((ok.exit_code, ok.error.as_deref()), (0, None));
    assert_eq!(ok.stdout, b"updated from 0.2.0 to 0.3.0\n");

    let back = status(Phase::RolledBack, now)
        .outcome("0.2.0", now)
        .unwrap();
    assert_eq!(
        back.error.as_deref(),
        Some("rolled back to 0.2.0: 0.3.0 did not connect within 300 s")
    );
    assert!(!back.rejected);

    // Watching: no answer until the deadline, then by the running version.
    let watching = status(Phase::Watching, now);
    assert_eq!(watching.outcome("0.3.0", now + 1000), None);
    let late = watching.deadline_unix_ms() + 1;
    assert_eq!(watching.outcome("0.3.0", late).unwrap().exit_code, 0);
    assert!(watching.outcome("0.2.0", late).unwrap().error.is_some());
}

#[test]
fn status_and_connected_files_round_trip() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join(STATUS_FILE);
    assert_eq!(Status::read(&path).unwrap(), None);
    let written = status(Phase::Watching, 5);
    written.write(&path).unwrap();
    assert_eq!(Status::read(&path).unwrap(), Some(written));

    assert_eq!(Connected::read(dir.path()).unwrap(), None);
    Connected::record(dir.path()).unwrap();
    let connected = Connected::read(dir.path()).unwrap().unwrap();
    assert_eq!(connected.version, monitorflow_common::VERSION);
    assert_eq!(connected.pid, std::process::id());
}

#[test]
fn state_files_are_not_read_through_links() {
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("elsewhere");
    std::fs::write(&target, b"{}").unwrap();
    let link = dir.path().join(STATUS_FILE);
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(Status::read(&link).is_err());
}

#[test]
fn health_timeouts_are_bounded() {
    assert_eq!(health_timeout(0), Ok(Duration::from_secs(300)));
    assert_eq!(health_timeout(30), Ok(Duration::from_secs(30)));
    assert!(health_timeout(29).is_err());
    assert!(health_timeout(3601).is_err());
}

#[tokio::test]
async fn the_resumer_claims_only_its_command_and_waits_for_the_outcome() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join(STATUS_FILE);
    let resumer = Arc::new(StatusResumer::new(dir.path()).with_poll(Duration::from_millis(10)));
    assert!(resumer.resume("c-1").await.is_none(), "no status file");

    let mut current = status(Phase::Watching, unix_ms());
    current.to = monitorflow_common::VERSION.into();
    current.write(&path).unwrap();
    assert!(resumer.resume("other").await.is_none());
    let future = resumer.resume("c-1").await.expect("its command");
    let waiting = tokio::spawn(future);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!waiting.is_finished(), "waits while the watchdog decides");
    current.phase = Phase::Succeeded;
    current.write(&path).unwrap();
    let output = tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(output.exit_code, 0);
}
