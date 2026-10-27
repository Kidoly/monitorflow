use std::sync::Mutex;

use tempfile::TempDir;

use super::*;
use crate::self_update::tests::Signer;

const ASSET: &str = "monitorflow-agent-windows-x86_64.exe";

/// A minimal x86-64 PE image, then `tag`.
fn pe(tag: &str) -> Vec<u8> {
    let mut bytes = vec![0u8; 0x100];
    bytes[..2].copy_from_slice(b"MZ");
    bytes[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
    bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
    bytes[0x84..0x86].copy_from_slice(&0x8664u16.to_le_bytes());
    bytes.extend_from_slice(tag.as_bytes());
    bytes
}

/// Counts restarts; may write the connected file as a new agent would.
#[derive(Default)]
struct FakeAgent {
    restarts: Mutex<u32>,
    connect_as: Option<(PathBuf, String)>,
}

impl AgentService for FakeAgent {
    fn restart(&self) -> io::Result<()> {
        *self.restarts.lock().unwrap() += 1;
        if let Some((data_dir, version)) = &self.connect_as {
            let path = Connected::path(data_dir);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let connected = Connected {
                version: version.clone(),
                pid: 1,
                at_unix_ms: unix_ms(),
            };
            fs::write(path, serde_json::to_vec(&connected).unwrap()).unwrap();
        }
        Ok(())
    }
}

struct Host {
    dir: TempDir,
    paths: Paths,
    signer: Signer,
}

impl Host {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let bin = dir.path().join("Program Files/MonitorFlow");
        fs::create_dir_all(&bin).unwrap();
        let paths = Paths {
            exe: bin.join("monitorflow-agent.exe"),
            state_dir: dir.path().join("update/self-update"),
        };
        fs::write(&paths.exe, pe("0.2.0")).unwrap();
        Self {
            dir,
            paths,
            signer: Signer::new(),
        }
    }

    fn data_dir(&self) -> PathBuf {
        self.dir.path().join("agent")
    }

    /// Stages a release as the agent does.
    fn stage(&self, version: &str, binary: &[u8]) -> Request {
        let files = self.signer.release(version, ASSET, binary);
        let request = Request {
            command_id: "c-1".into(),
            data_dir: self.data_dir(),
            allow_downgrade: false,
            health_timeout_ms: 200,
        };
        let staged = request.staged();
        fs::create_dir_all(&staged).unwrap();
        fs::write(staged.join(SUMS), &files.sums).unwrap();
        fs::write(staged.join(SIGNATURE), &files.signature).unwrap();
        fs::write(staged.join(VERSION_FILE), &files.version_file).unwrap();
        fs::write(staged.join(ASSET), &files.binary).unwrap();
        request
    }

    fn install(&self, request: &Request) -> Result<Swapped, SwapError> {
        install(
            request,
            &self.paths,
            &[self.signer.public()],
            &"0.2.0".parse().unwrap(),
            Platform::WindowsX86_64,
        )
    }

    fn exe(&self) -> Vec<u8> {
        fs::read(&self.paths.exe).unwrap()
    }

    fn status(&self) -> Status {
        Status::read(&self.paths.status()).unwrap().unwrap()
    }
}

#[test]
fn swaps_the_executable_and_keeps_the_new_one_when_it_connects() {
    let host = Host::new();
    let request = host.stage("0.3.0", &pe("0.3.0"));
    let swapped = host.install(&request).unwrap();
    assert_eq!(
        swapped,
        Swapped::Restarting {
            from: "0.2.0".parse().unwrap(),
            to: "0.3.0".parse().unwrap()
        }
    );
    assert_eq!(host.exe(), pe("0.3.0"));
    assert_eq!(fs::read(host.paths.previous()).unwrap(), pe("0.2.0"));
    assert!(!request.staged().exists(), "staged files are removed");
    assert_eq!(host.status().phase, Phase::Watching);
    // A second update waits for this one.
    let again = host.stage("0.4.0", &pe("0.4.0"));
    assert!(matches!(
        host.install(&again),
        Err(SwapError::InProgress(_))
    ));

    let agent = FakeAgent {
        connect_as: Some((host.data_dir(), "0.3.0".into())),
        ..FakeAgent::default()
    };
    let watched = watch(&host.paths, &agent, Duration::from_millis(10)).unwrap();
    assert_eq!(watched, Watched::Succeeded);
    assert_eq!(host.status().phase, Phase::Succeeded);
    assert_eq!(host.exe(), pe("0.3.0"));
    assert_eq!(*agent.restarts.lock().unwrap(), 1);
}

#[test]
fn renames_the_old_executable_back_when_the_new_one_does_not_connect() {
    let host = Host::new();
    host.install(&host.stage("0.3.0", &pe("0.3.0"))).unwrap();
    let agent = FakeAgent::default();
    let watched = watch(&host.paths, &agent, Duration::from_millis(10)).unwrap();
    assert_eq!(watched, Watched::RolledBack);
    assert_eq!(host.exe(), pe("0.2.0"));
    let status = host.status();
    assert_eq!(status.phase, Phase::RolledBack);
    assert_eq!(
        status.reason.as_deref(),
        Some("0.3.0 did not connect within 0 s")
    );
    assert_eq!(*agent.restarts.lock().unwrap(), 2);
    // The next update works, replacing what the rollback left.
    host.install(&host.stage("0.3.1", &pe("0.3.1"))).unwrap();
    assert_eq!(host.exe(), pe("0.3.1"));
}

#[test]
fn refuses_what_does_not_verify_and_changes_nothing() {
    let host = Host::new();
    let mut elf = b"\x7fELF\x02\x01\x01\0\0\0\0\0\0\0\0\0\x02\0\x3e\0".to_vec();
    elf.resize(64, 0);
    for (name, request) in [
        ("linux binary", host.stage("0.3.0", &elf)),
        ("downgrade", host.stage("0.1.0", &pe("0.1.0"))),
    ] {
        assert!(host.install(&request).is_err(), "{name}");
        assert_eq!(host.exe(), pe("0.2.0"), "{name}");
    }
    // Changed after staging: the signature or the checksum fails.
    let request = host.stage("0.3.0", &pe("0.3.0"));
    fs::write(request.staged().join(ASSET), pe("evil")).unwrap();
    assert!(matches!(
        host.install(&request),
        Err(SwapError::Release(ReleaseError::Mismatch(_)))
    ));
    let request = host.stage("0.3.0", &pe("0.3.0"));
    fs::write(request.staged().join(SUMS), "garbage").unwrap();
    assert!(matches!(
        host.install(&request),
        Err(SwapError::Release(ReleaseError::BadSignature))
    ));
    assert_eq!(host.exe(), pe("0.2.0"));
    assert!(!host.paths.previous().exists());
    // The running version is a no-op; a relative data directory is refused.
    let same = host.stage("0.2.0", &pe("0.2.0"));
    assert_eq!(
        host.install(&same).unwrap(),
        Swapped::Current("0.2.0".parse().unwrap())
    );
    let mut relative = host.stage("0.3.0", &pe("0.3.0"));
    relative.data_dir = "agent".into();
    assert!(matches!(host.install(&relative), Err(SwapError::Input(_))));
}
