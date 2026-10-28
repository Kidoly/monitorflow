use std::io::Cursor;
use std::sync::Mutex;

use monitorflow_common::release::PublicKey;
use tempfile::TempDir;

use super::*;
use crate::self_update::tests::{elf, ReleaseFiles, Signer};

const ASSET: &str = "monitorflow-agent-linux-x86_64";

/// Records calls; the restart may write the connected file, as a working
/// new agent would.
#[derive(Default)]
struct FakeServices {
    calls: Mutex<Vec<String>>,
    fail_watch: bool,
    connect_as: Option<(PathBuf, String)>,
}

impl Services for FakeServices {
    fn start_watch(&self, program: &Path) -> io::Result<()> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("watch {}", program.display()));
        if self.fail_watch {
            return Err(io::Error::other("no systemd"));
        }
        Ok(())
    }

    fn restart_agent(&self) -> io::Result<()> {
        self.calls.lock().unwrap().push("restart".into());
        if let Some((data_dir, version)) = &self.connect_as {
            let connected = Connected {
                version: version.clone(),
                pid: 1,
                at_unix_ms: unix_ms(),
            };
            let path = Connected::path(data_dir);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, serde_json::to_vec(&connected).unwrap()).unwrap();
        }
        Ok(())
    }
}

struct Host {
    _dir: TempDir,
    paths: Paths,
    signer: Signer,
    keys: Vec<PublicKey>,
}

impl Host {
    /// An installed 0.2.0 binary.
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let bin_dir = dir.path().join("usr/bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let paths = Paths {
            product: crate::self_update::AGENT,
            bin: bin_dir.join("monitorflow-agent"),
            state_dir: dir.path().join("state"),
            data_dir: dir.path().join("data"),
        };
        fs::write(&paths.bin, elf("0.2.0")).unwrap();
        let signer = Signer::new();
        let keys = vec![signer.public()];
        Self {
            _dir: dir,
            paths,
            signer,
            keys,
        }
    }

    fn install(&self, input: Vec<u8>, services: &FakeServices) -> Result<Installed, HelperError> {
        install(
            &mut Cursor::new(input),
            &self.paths,
            &self.keys,
            &"0.2.0".parse().unwrap(),
            Platform::LinuxX86_64,
            services,
        )
    }

    fn release(&self, version: &str) -> ReleaseFiles {
        self.signer.release(version, ASSET, &elf(version))
    }

    fn installed(&self) -> Vec<u8> {
        fs::read(&self.paths.bin).unwrap()
    }

    fn status(&self) -> Status {
        Status::read(&self.paths.status()).unwrap().unwrap()
    }

    fn assert_untouched(&self) {
        assert_eq!(self.installed(), elf("0.2.0"));
        assert!(!self.paths.staging().exists(), "no staging file left");
    }
}

#[test]
fn installs_keeps_the_previous_binary_and_starts_the_watchdog() {
    let host = Host::new();
    let services = FakeServices::default();
    let outcome = host
        .install(host.release("0.3.0").input("c-1", 60_000), &services)
        .unwrap();
    assert_eq!(
        outcome,
        Installed::Restarting {
            from: "0.2.0".parse().unwrap(),
            to: "0.3.0".parse().unwrap()
        }
    );
    assert_eq!(host.installed(), elf("0.3.0"));
    assert_eq!(fs::read(host.paths.previous()).unwrap(), elf("0.2.0"));
    let mode = fs::metadata(&host.paths.bin).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o755);
    let status = host.status();
    assert_eq!(
        (status.phase, status.command_id.as_str(), status.to.as_str()),
        (Phase::Watching, "c-1", "0.3.0")
    );
    assert_eq!(status.health_timeout_ms, 60_000);
    assert_eq!(
        *services.calls.lock().unwrap(),
        [format!("watch {}", host.paths.previous().display())]
    );
    // A second update waits for the first one's outcome.
    let again = host.install(host.release("0.4.0").input("c-2", 0), &services);
    assert!(
        matches!(again, Err(HelperError::InProgress(_))),
        "{again:?}"
    );
}

#[test]
fn refuses_what_does_not_verify_and_changes_nothing() {
    let host = Host::new();
    let services = FakeServices::default();

    let mut foreign = Signer::new().release("0.3.0", ASSET, &elf("0.3.0"));
    let bad_signature = foreign.input("c-1", 0);
    // A binary cut short.
    foreign = host.release("0.3.0");
    foreign.binary.truncate(40);
    let truncated = foreign.input("c-1", 0);
    // Signed, but for another machine.
    let mut arm = elf("0.3.0");
    arm[18] = 183;
    let wrong_arch = host.signer.release("0.3.0", ASSET, &arm).input("c-1", 0);
    // Signed for aarch64 only.
    let other_asset = host
        .signer
        .release("0.3.0", "monitorflow-agent-linux-aarch64", &elf("0.3.0"))
        .input("c-1", 0);
    let downgrade = host.release("0.1.0").input("c-1", 0);
    let garbage = b"not json\n".to_vec();

    for (name, input) in [
        ("bad signature", bad_signature),
        ("truncated", truncated),
        ("wrong arch", wrong_arch),
        ("other asset", other_asset),
        ("downgrade", downgrade),
        ("garbage", garbage),
        ("no header line", b"{}".to_vec()),
    ] {
        let result = host.install(input, &services);
        assert!(result.is_err(), "{name}: {result:?}");
        host.assert_untouched();
    }
    assert!(services.calls.lock().unwrap().is_empty());
    assert!(!host.paths.status().exists());
}

#[test]
fn the_running_version_is_a_no_op_and_downgrades_need_permission() {
    let host = Host::new();
    let services = FakeServices::default();
    let same = host.install(host.release("0.2.0").input("c-1", 0), &services);
    assert_eq!(same.unwrap(), Installed::Current("0.2.0".parse().unwrap()));
    host.assert_untouched();

    let mut input = host.release("0.1.0").input("c-1", 0);
    let newline = input.iter().position(|b| *b == b'\n').unwrap();
    let mut header: Header = serde_json::from_slice(&input[..newline]).unwrap();
    header.allow_downgrade = true;
    let mut allowed = serde_json::to_vec(&header).unwrap();
    allowed.extend(input.drain(newline..));
    host.install(allowed, &services).unwrap();
    assert_eq!(host.installed(), elf("0.1.0"));
}

#[test]
fn a_watchdog_that_cannot_start_puts_the_old_binary_back() {
    let host = Host::new();
    let services = FakeServices {
        fail_watch: true,
        ..FakeServices::default()
    };
    let result = host.install(host.release("0.3.0").input("c-1", 0), &services);
    assert!(matches!(result, Err(HelperError::Launch(_))), "{result:?}");
    assert_eq!(host.installed(), elf("0.2.0"));
    assert_eq!(host.status().phase, Phase::Failed);
}

#[test]
fn the_watchdog_keeps_a_new_agent_that_connects() {
    let host = Host::new();
    let services = FakeServices {
        connect_as: Some((host.paths.data_dir.clone(), "0.3.0".into())),
        ..FakeServices::default()
    };
    host.install(host.release("0.3.0").input("c-1", 5_000), &services)
        .unwrap();
    let watched = watch(&host.paths, &services, Duration::from_millis(10)).unwrap();
    assert_eq!(watched, Watched::Succeeded);
    assert_eq!(host.installed(), elf("0.3.0"));
    assert_eq!(host.status().phase, Phase::Succeeded);
    // Nothing more to watch.
    assert_eq!(
        watch(&host.paths, &services, Duration::from_millis(10)).unwrap(),
        Watched::Nothing
    );
}

#[test]
fn the_watchdog_rolls_back_an_agent_that_does_not_connect() {
    let host = Host::new();
    // The old version connecting does not count, nor an old record.
    let services = FakeServices {
        connect_as: Some((host.paths.data_dir.clone(), "0.2.0".into())),
        ..FakeServices::default()
    };
    host.install(host.release("0.3.0").input("c-1", 200), &services)
        .unwrap();
    let watched = watch(&host.paths, &services, Duration::from_millis(10)).unwrap();
    assert_eq!(watched, Watched::RolledBack);
    assert_eq!(host.installed(), elf("0.2.0"));
    let status = host.status();
    assert_eq!(status.phase, Phase::RolledBack);
    assert_eq!(
        status.reason.as_deref(),
        Some("0.3.0 did not connect within 0 s")
    );
    let calls = services.calls.lock().unwrap();
    assert_eq!(calls.iter().filter(|c| *c == "restart").count(), 2);
}
