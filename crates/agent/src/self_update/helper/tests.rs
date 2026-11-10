use std::io::Cursor;
use std::sync::Mutex;

use monitorflow_common::release::PublicKey;
use tempfile::TempDir;

use super::*;
use crate::helper_trust::tests::{Ca, Host as Trusted};
use crate::self_update::tests::{elf, update_command, ReleaseFiles, Signer};

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
    /// The central server's CA, and the helper's anchor pinned to it.
    ca: Ca,
    trusted: Trusted,
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
        let ca = Ca::new();
        let trusted = Trusted::pinned(&ca);
        Self {
            _dir: dir,
            paths,
            signer,
            keys,
            ca,
            trusted,
        }
    }

    /// The stdin of a signed update to `version`: its release, and the
    /// command `id` asking for it.
    fn update(&self, id: &str, version: &str, health_timeout_seconds: u32) -> Vec<u8> {
        let command = update_command(id, version, false, health_timeout_seconds);
        self.release(version).input(&self.ca.sign(command))
    }

    /// `files` with a signed command `id` asking for 0.3.0.
    fn with(&self, id: &str, files: &ReleaseFiles) -> Vec<u8> {
        files.input(&self.ca.sign(update_command(id, "0.3.0", false, 0)))
    }

    fn install(&self, input: Vec<u8>, services: &FakeServices) -> Result<Installed, HelperError> {
        install(
            &mut Cursor::new(input),
            &self.paths,
            &self.trusted.place,
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
        .install(host.update("c-1", "0.3.0", 60), &services)
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
    let again = host.install(host.update("c-2", "0.4.0", 0), &services);
    assert!(
        matches!(again, Err(HelperError::InProgress(_))),
        "{again:?}"
    );
}

#[test]
fn refuses_what_does_not_verify_and_changes_nothing() {
    let host = Host::new();
    let services = FakeServices::default();

    // Every case has its own signed command, so none is refused as a
    // replay of the one before.
    let mut foreign = Signer::new().release("0.3.0", ASSET, &elf("0.3.0"));
    let bad_signature = host.with("c-1", &foreign);
    // A binary cut short.
    foreign = host.release("0.3.0");
    foreign.binary.truncate(40);
    let truncated = host.with("c-2", &foreign);
    // Signed, but for another machine.
    let mut arm = elf("0.3.0");
    arm[18] = 183;
    let wrong_arch = host.with("c-3", &host.signer.release("0.3.0", ASSET, &arm));
    // Signed for aarch64 only.
    let other_asset =
        host.signer
            .release("0.3.0", "monitorflow-agent-linux-aarch64", &elf("0.3.0"));
    let other_asset = host.with("c-4", &other_asset);
    let downgrade = host.update("c-5", "0.1.0", 0);
    let request = |id| HelperRequest::of(&host.ca.sign(update_command(id, "0.3.0", false, 0)));
    let garbage = [request("c-6").framed(), b"not json\n".to_vec()].concat();
    let no_header_line = [request("c-7").framed(), b"{}".to_vec()].concat();

    for (name, input, error) in [
        ("bad signature", bad_signature, "does not verify"),
        ("truncated", truncated, "checksum mismatch"),
        ("wrong arch", wrong_arch, "x86_64"),
        ("other asset", other_asset, "monitorflow-agent-linux-x86_64"),
        ("downgrade", downgrade, "older"),
        ("garbage", garbage, "invalid input"),
        ("no header line", no_header_line, "no header line"),
    ] {
        let result = host.install(input, &services).map_err(|e| e.to_string());
        let message = result.as_ref().unwrap_err();
        assert!(message.contains(error), "{name}: {message}");
        host.assert_untouched();
    }
    assert!(services.calls.lock().unwrap().is_empty());
    assert!(!host.paths.status().exists());
}

#[test]
fn the_running_version_is_a_no_op_and_downgrades_need_permission() {
    let host = Host::new();
    let services = FakeServices::default();
    let same = host.install(host.update("c-1", "0.2.0", 0), &services);
    assert_eq!(same.unwrap(), Installed::Current("0.2.0".parse().unwrap()));
    host.assert_untouched();

    // The permission is the signed command's: the caller setting it in a
    // command the server signed without it breaks the signature.
    let old = host.release("0.1.0");
    let mut forged = host.ca.sign(update_command("c-2", "0.1.0", false, 0));
    forged.payload = update_command("c-2", "0.1.0", true, 0).payload;
    let result = host.install(old.input(&forged), &services);
    assert!(matches!(result, Err(HelperError::Trust(_))), "{result:?}");
    let refused = host.install(host.update("c-3", "0.1.0", 0), &services);
    assert!(
        matches!(refused, Err(HelperError::Release(_))),
        "{refused:?}"
    );
    host.assert_untouched();

    let allowed = host.ca.sign(update_command("c-4", "0.1.0", true, 0));
    host.install(old.input(&allowed), &services).unwrap();
    assert_eq!(host.installed(), elf("0.1.0"));
}

#[test]
fn only_the_signed_command_installs_and_only_the_version_it_names() {
    let host = Host::new();
    let services = FakeServices::default();
    let release = host.release("0.3.0");

    // What the helper read before this decision: a header line alone.
    let header = b"{\"command_id\":\"c-1\",\"allow_downgrade\":true}\n".to_vec();
    let unsigned = release.input(&update_command("c-1", "0.3.0", false, 0));
    let foreign = release.input(&Ca::new().sign(update_command("c-1", "0.3.0", false, 0)));
    let mut other = update_command("c-1", "0.3.0", false, 0);
    other.agent_id = "web-02".into();
    let other = release.input(&host.ca.sign(other));
    let mut expired = update_command("c-1", "0.3.0", false, 0);
    expired.expires_at_unix_ms = unix_ms() - 1;
    let expired = release.input(&host.ca.sign(expired));
    for (name, input) in [
        ("old input", header),
        ("unsigned", unsigned),
        ("another CA", foreign),
        ("another machine", other),
        ("expired", expired),
    ] {
        let result = host.install(input, &services);
        assert!(
            matches!(result, Err(HelperError::Trust(_))),
            "{name}: {result:?}"
        );
        host.assert_untouched();
    }

    // A command for 0.3.0 does not install another signed release.
    let newer = host.release("0.4.0");
    let result = host.install(host.with("c-2", &newer), &services);
    assert!(
        result
            .as_ref()
            .is_err_and(|e| e.to_string().contains("the command asks for 0.3.0")),
        "{result:?}"
    );
    host.assert_untouched();

    // The command runs once.
    host.install(host.update("c-3", "0.3.0", 0), &services)
        .unwrap();
    let again = host.install(host.update("c-3", "0.3.0", 0), &services);
    assert!(
        again
            .as_ref()
            .is_err_and(|e| e.to_string().contains("already run")),
        "{again:?}"
    );
}

#[test]
fn a_watchdog_that_cannot_start_puts_the_old_binary_back() {
    let host = Host::new();
    let services = FakeServices {
        fail_watch: true,
        ..FakeServices::default()
    };
    let result = host.install(host.update("c-1", "0.3.0", 0), &services);
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
    host.install(host.update("c-1", "0.3.0", 30), &services)
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
    host.install(host.update("c-1", "0.3.0", 30), &services)
        .unwrap();
    // The shortest timeout a command may ask for is 30 s: cut it here.
    let mut status = host.status();
    status.health_timeout_ms = 200;
    status.write(&host.paths.status()).unwrap();
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
