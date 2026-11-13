//! The updater's flow (ADR-0056) against a fake Docker host and real
//! signed release directories, served from disk or over HTTP.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use monitorflow_common::release::{sha256, PublicKey, Version};
use monitorflow_common::server_update::backup::{
    file_name, BackupKind, BackupState, Manifest, BACKUP_REQUEST_FILE, KEEP,
};
use monitorflow_common::server_update::retention::RetentionRequest;
use monitorflow_common::server_update::{
    ImageRef, Request, Status, UpdateRun, UpdateState, REQUEST_FILE, STATUS_FILE,
};
use monitorflow_updater::compose_file;
use monitorflow_updater::env_file::{EnvFile, Pins};
use monitorflow_updater::host::{Host, HostError};
use monitorflow_updater::source::{self, Source};
use monitorflow_updater::update::{Reporter, Updater, COMMAND_LINE};
use monitorflow_updater::{restore, take_backup_request, take_request, INBOX};
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use tempfile::TempDir;

const BROKEN: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn digest(version: &str) -> String {
    let hex: String = sha256(version.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    hex
}

fn image(name: &str, version: &str) -> String {
    format!(
        "registry.test/monitorflow-{name}@sha256:{}",
        digest(version)
    )
}

fn v(text: &str) -> Version {
    text.parse().unwrap()
}

struct Signer(Ed25519KeyPair);

impl Signer {
    fn new() -> Self {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        Self(Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap())
    }

    fn public(&self) -> PublicKey {
        let mut der = vec![
            0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
        ];
        der.extend_from_slice(self.0.public_key().as_ref());
        PublicKey::from_spki(&der).unwrap()
    }

    /// A release directory of `version`; `server` overrides its server
    /// image.
    fn release(&self, version: &str, server: Option<String>) -> TempDir {
        let dir = TempDir::new().unwrap();
        let images = format!(
            r#"{{"version": "{version}", "images": {{"server": "{}", "web": "{}", "updater": "{}", "mirror": "{}"}}}}"#,
            server.unwrap_or(image("server", version)),
            image("web", version),
            image("updater", version),
            image("mirror", version)
        );
        let files: [(&str, Vec<u8>); 4] = [
            ("VERSION", format!("{version}\n").into_bytes()),
            ("images.json", images.into_bytes()),
            ("monitorflow-agent-linux-x86_64", b"agent".to_vec()),
            ("install.sh", b"#!/bin/sh\n".to_vec()),
        ];
        let mut sums = String::new();
        for (name, bytes) in &files {
            std::fs::write(dir.path().join(name), bytes).unwrap();
            let hex: String = sha256(bytes).iter().map(|b| format!("{b:02x}")).collect();
            sums.push_str(&format!("{hex}  {name}\n"));
        }
        std::fs::write(dir.path().join("SHA256SUMS"), &sums).unwrap();
        std::fs::write(
            dir.path().join("SHA256SUMS.sig"),
            self.0.sign(sums.as_bytes()).as_ref(),
        )
        .unwrap();
        dir
    }
}

/// A Docker host in memory: the server's version follows the image pinned
/// in `.env` when it is recreated.
struct FakeHost {
    env: PathBuf,
    state: Mutex<HostState>,
}

#[derive(Default)]
struct HostState {
    version: String,
    migration: i64,
    pulled: Vec<String>,
    imported: Vec<PathBuf>,
    replaced: bool,
    /// The new server applies a migration when it starts.
    migrates: bool,
    /// The new server starts but reports this version.
    reports: Option<String>,
    /// Recreation never returns, as when the updater is killed.
    hangs: bool,
    /// `pg_dump` fails.
    dump_fails: bool,
    /// The migration step of the new image fails.
    migrate_fails: bool,
    /// The next recreation of the stores fails.
    stores_fail: bool,
    /// The next check of the stores fails.
    check_fails: bool,
    /// What a restore loaded, and what it stopped and started.
    restored: Option<Vec<u8>>,
    calls: Vec<&'static str>,
}

impl FakeHost {
    fn new(env: &Path, version: &str) -> Self {
        Self {
            env: env.to_owned(),
            state: Mutex::new(HostState {
                version: version.into(),
                migration: 20261031090000,
                ..HostState::default()
            }),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, HostState> {
        self.state.lock().unwrap()
    }
}

#[async_trait]
impl Host for FakeHost {
    async fn server_version(&self) -> Result<Version, HostError> {
        Ok(v(&self.state().version))
    }

    async fn pull(&self, image: &ImageRef) -> Result<(), HostError> {
        self.state().pulled.push(image.to_string());
        Ok(())
    }

    async fn latest_migration(&self) -> Result<i64, HostError> {
        Ok(self.state().migration)
    }

    async fn dump_database(&self, dest: &Path) -> Result<(), HostError> {
        if self.state().dump_fails {
            return Err(HostError("pg_dump: connection refused".into()));
        }
        std::fs::write(dest, b"PGDMP").map_err(|e| HostError(e.to_string()))
    }

    async fn migrate(&self) -> Result<(), HostError> {
        let mut state = self.state();
        state.calls.push("migrate");
        if state.migrate_fails {
            return Err(HostError("db-migrate exited with code 1".into()));
        }
        Ok(())
    }

    async fn recreate(&self) -> Result<(), HostError> {
        self.state().calls.push("recreate");
        if self.state().hangs {
            std::future::pending::<()>().await;
        }
        let pins = EnvFile::new(self.env.clone()).pins().unwrap();
        let mut state = self.state();
        match pins.server {
            None => state.version = "0.1.0".into(),
            Some(server) if server.ends_with(BROKEN) => {
                if state.migrates {
                    state.migration += 1;
                }
                return Err(HostError("server is unhealthy".into()));
            }
            Some(server) => {
                let version = ["0.2.0", "0.3.0"]
                    .into_iter()
                    .find(|v| server == image("server", v))
                    .expect("a known image");
                if state.migrates && version != "0.1.0" {
                    state.migration += 1;
                }
                state.version = state.reports.clone().unwrap_or(version.into());
            }
        }
        Ok(())
    }

    async fn import_agent_release(&self, dir: &Path) -> Result<(), HostError> {
        self.state().imported.push(dir.to_owned());
        Ok(())
    }

    async fn replace_updater(&self) -> Result<(), HostError> {
        self.state().replaced = true;
        Ok(())
    }

    async fn image_version(&self) -> Result<Version, HostError> {
        Ok(v(&self.state().version))
    }

    async fn stop_server(&self) -> Result<(), HostError> {
        self.state().calls.push("stop");
        Ok(())
    }

    async fn restore_database(&self, dump: &Path) -> Result<(), HostError> {
        let bytes = std::fs::read(dump).map_err(|e| HostError(e.to_string()))?;
        let mut state = self.state();
        state.calls.push("restore");
        state.restored = Some(bytes);
        Ok(())
    }

    async fn start_all(&self) -> Result<(), HostError> {
        self.state().calls.push("start");
        Ok(())
    }

    async fn recreate_stores(&self) -> Result<(), HostError> {
        let mut state = self.state();
        state.calls.push("stores");
        if std::mem::take(&mut state.stores_fail) {
            return Err(HostError("the stores did not start".into()));
        }
        Ok(())
    }

    async fn apply_compose(&self) -> Result<(), HostError> {
        self.state().calls.push("apply");
        Ok(())
    }

    async fn check_stores(&self) -> Result<(), HostError> {
        let mut state = self.state();
        state.calls.push("check");
        if std::mem::take(&mut state.check_fails) {
            return Err(HostError("VictoriaMetrics does not answer".into()));
        }
        Ok(())
    }
}

/// The volumes and the project's `.env`.
struct Volumes {
    _root: TempDir,
    update: PathBuf,
    state: PathBuf,
    backups: PathBuf,
    serverdata: PathBuf,
    env: PathBuf,
}

impl Volumes {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let dir = |name: &str| {
            let path = root.path().join(name);
            std::fs::create_dir_all(&path).unwrap();
            path
        };
        let (update, state, backups, deploy) =
            (dir("update"), dir("state"), dir("backups"), dir("deploy"));
        let serverdata = dir("serverdata");
        std::fs::create_dir_all(serverdata.join("ca")).unwrap();
        std::fs::write(serverdata.join("ca/ca-key.pem"), "secret").unwrap();
        // The key of the secrets stored in the database (ADR-0110).
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::create_dir_all(serverdata.join("secrets")).unwrap();
            let keys = serverdata.join("secrets/keys");
            std::fs::write(&keys, "key").unwrap();
            std::fs::set_permissions(&keys, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        std::fs::create_dir_all(serverdata.join("mirrors/aptly/pool")).unwrap();
        std::fs::write(serverdata.join("mirrors/aptly/pool/big.deb"), "deb").unwrap();
        std::fs::create_dir_all(serverdata.join("mirrors/gnupg")).unwrap();
        std::fs::write(serverdata.join("mirrors/gnupg/key"), "gpg").unwrap();
        let env = deploy.join(".env");
        std::fs::write(&env, "DOMAIN=mf.test\nPOSTGRES_PASSWORD=x\n").unwrap();
        Self {
            update,
            state,
            backups,
            serverdata,
            env,
            _root: root,
        }
    }

    fn updater<'a>(&self, host: &'a FakeHost, source: &'a dyn Source) -> Updater<'a> {
        Updater {
            host,
            source,
            env: EnvFile::new(self.env.clone()),
            update_dir: self.update.clone(),
            server_update_dir: "/var/lib/monitorflow/update".into(),
            state_dir: self.state.clone(),
            backup_dir: self.backups.clone(),
            serverdata_dir: self.serverdata.clone(),
            reporter: Reporter::open(&self.update),
        }
    }

    fn pins(&self) -> Pins {
        EnvFile::new(self.env.clone()).pins().unwrap()
    }

    fn status(&self) -> Status {
        serde_json::from_slice(&std::fs::read(self.update.join(STATUS_FILE)).unwrap()).unwrap()
    }

    fn backups(&self) -> Vec<String> {
        std::fs::read_dir(&self.backups)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect()
    }
}

fn request(version: &str) -> Request {
    Request {
        id: "r1".into(),
        version: version.into(),
        requested_by: "admin".into(),
        requested_at: Utc::now(),
    }
}

fn dir_source(release: &TempDir, key: PublicKey) -> source::Dir {
    source::Dir {
        dir: release.path().to_owned(),
        keys: vec![key],
    }
}

#[tokio::test]
async fn updates_to_a_signed_release_after_a_backup() {
    let signer = Signer::new();
    let release = signer.release("0.2.0", None);
    let volumes = Volumes::new();
    let host = FakeHost::new(&volumes.env, "0.1.0");
    let source = dir_source(&release, signer.public());
    let mut updater = volumes.updater(&host, &source);

    assert_eq!(
        updater.update(&request("0.2.0"), &v("0.2.0")).await,
        UpdateState::Succeeded
    );

    assert_eq!(host.state().version, "0.2.0");
    let pins = volumes.pins();
    assert_eq!(
        pins.server.as_deref(),
        Some(image("server", "0.2.0").as_str())
    );
    assert_eq!(
        pins.updater.as_deref(),
        Some(image("updater", "0.2.0").as_str())
    );
    assert!(std::fs::read_to_string(&volumes.env)
        .unwrap()
        .starts_with("DOMAIN=mf.test\n"));
    assert_eq!(
        pins.mirror.as_deref(),
        Some(image("mirror", "0.2.0").as_str())
    );
    assert_eq!(host.state().pulled.len(), 4);
    // The agent release went through the server's import, from the path
    // the server container sees, then the staging copy was removed.
    assert_eq!(
        host.state().imported,
        [PathBuf::from("/var/lib/monitorflow/update/release/0.2.0")]
    );
    assert!(!volumes.update.join("release/0.2.0").exists());
    assert!(host.state().replaced, "the updater replaces itself");
    // The new image migrates as the owner before the server restarts on
    // its own role (ADR-0109).
    assert_eq!(host.state().calls, ["migrate", "recreate"]);

    let backups = volumes.backups();
    assert_eq!(backups.len(), 1, "{backups:?}");
    assert!(
        backups[0].starts_with("monitorflow-backup-") && backups[0].ends_with("-update.tar.gz"),
        "{backups:?}"
    );
    let status = volumes.status();
    assert_eq!(status.backups.files.len(), 1);
    assert_eq!(status.backups.files[0].kind, BackupKind::Update);
    let run = volumes.status().update.unwrap();
    assert_eq!(run.state, UpdateState::Succeeded);
    assert_eq!(run.from.as_deref(), Some("0.1.0"));
    assert_eq!(run.backup.as_deref(), Some(backups[0].as_str()));
    assert!(run.finished_at.is_some());
    assert!(run
        .steps
        .iter()
        .any(|s| s.message.contains("agent release 0.2.0 cached")));
    // The whole release was staged, the installer included (ADR-0054).
    assert!(run.steps.iter().any(|s| s.message.contains("install.sh")
        && s.message.contains("monitorflow-agent-linux-x86_64")));
}

#[tokio::test]
async fn refuses_downgrades_and_the_running_version_without_touching_anything() {
    let signer = Signer::new();
    let release = signer.release("0.2.0", None);
    for running in ["0.3.0", "0.2.0"] {
        let volumes = Volumes::new();
        let host = FakeHost::new(&volumes.env, running);
        let source = dir_source(&release, signer.public());
        let mut updater = volumes.updater(&host, &source);
        assert_eq!(
            updater.update(&request("0.2.0"), &v("0.2.0")).await,
            UpdateState::Failed
        );
        let error = volumes.status().update.unwrap().error.unwrap();
        assert!(
            error.contains("older") || error.contains("already running"),
            "{error}"
        );
        assert!(host.state().pulled.is_empty());
        assert!(volumes.backups().is_empty());
        assert_eq!(volumes.pins(), Pins::default());
    }
}

#[tokio::test]
async fn refuses_releases_this_build_does_not_trust() {
    let release = Signer::new().release("0.2.0", None);
    let volumes = Volumes::new();
    let host = FakeHost::new(&volumes.env, "0.1.0");
    let source = dir_source(&release, Signer::new().public());
    let mut updater = volumes.updater(&host, &source);
    assert_eq!(
        updater.update(&request("0.2.0"), &v("0.2.0")).await,
        UpdateState::Failed
    );
    let error = volumes.status().update.unwrap().error.unwrap();
    assert!(error.contains("does not verify"), "{error}");
    assert!(host.state().pulled.is_empty());
    assert_eq!(volumes.pins(), Pins::default());

    // Asked for another version than the directory holds.
    let signer = Signer::new();
    let release = signer.release("0.2.0", None);
    let source = dir_source(&release, signer.public());
    let mut updater = volumes.updater(&host, &source);
    assert_eq!(
        updater.update(&request("0.3.0"), &v("0.3.0")).await,
        UpdateState::Failed
    );
}

#[tokio::test]
async fn a_broken_image_is_rolled_back() {
    let signer = Signer::new();
    let broken = format!("registry.test/monitorflow-server@sha256:{BROKEN}");
    let release = signer.release("0.3.0", Some(broken));
    let volumes = Volumes::new();
    std::fs::write(
        &volumes.env,
        format!("DOMAIN=x\nSERVER_IMAGE={}\n", image("server", "0.2.0")),
    )
    .unwrap();
    let host = FakeHost::new(&volumes.env, "0.2.0");
    let source = dir_source(&release, signer.public());
    let mut updater = volumes.updater(&host, &source);

    assert_eq!(
        updater.update(&request("0.3.0"), &v("0.3.0")).await,
        UpdateState::RolledBack
    );
    assert_eq!(host.state().version, "0.2.0");
    assert_eq!(
        volumes.pins().server.as_deref(),
        Some(image("server", "0.2.0").as_str())
    );
    assert_eq!(volumes.pins().web, None);
    let run = volumes.status().update.unwrap();
    assert!(!run.migrated);
    assert!(run.error.unwrap().contains("did not start"));
    assert!(!host.state().replaced);
    assert_eq!(volumes.backups().len(), 1);
}

#[tokio::test]
async fn a_failed_migration_step_is_rolled_back() {
    let signer = Signer::new();
    let release = signer.release("0.3.0", None);
    let volumes = Volumes::new();
    std::fs::write(
        &volumes.env,
        format!("DOMAIN=x\nSERVER_IMAGE={}\n", image("server", "0.2.0")),
    )
    .unwrap();
    let host = FakeHost::new(&volumes.env, "0.2.0");
    host.state().migrate_fails = true;
    let source = dir_source(&release, signer.public());
    let mut updater = volumes.updater(&host, &source);

    assert_eq!(
        updater.update(&request("0.3.0"), &v("0.3.0")).await,
        UpdateState::RolledBack
    );
    // The new server never started: only the rollback recreated.
    assert_eq!(host.state().calls, ["migrate", "recreate"]);
    assert_eq!(host.state().version, "0.2.0");
    assert_eq!(
        volumes.pins().server.as_deref(),
        Some(image("server", "0.2.0").as_str())
    );
    let error = volumes.status().update.unwrap().error.unwrap();
    assert!(
        error.contains("did not migrate the database: db-migrate exited with code 1"),
        "{error}"
    );
}

#[tokio::test]
async fn a_rollback_after_migrations_points_to_the_backup() {
    let signer = Signer::new();
    let release = signer.release("0.2.0", None);
    let volumes = Volumes::new();
    let host = FakeHost::new(&volumes.env, "0.1.0");
    {
        let mut state = host.state();
        state.migrates = true;
        state.reports = Some("0.1.9".into());
    }
    let source = dir_source(&release, signer.public());
    let mut updater = volumes.updater(&host, &source);

    assert_eq!(
        updater.update(&request("0.2.0"), &v("0.2.0")).await,
        UpdateState::RolledBack
    );
    let run = volumes.status().update.unwrap();
    assert!(run.migrated);
    let backup = run.backup.clone().unwrap();
    assert!(
        run.steps
            .iter()
            .any(|s| s.message.contains(&format!("restore {backup}"))),
        "{:?}",
        run.steps
    );
    assert_eq!(volumes.pins(), Pins::default());
}

#[tokio::test]
async fn an_update_interrupted_before_confirmation_is_rolled_back_on_restart() {
    let signer = Signer::new();
    let release = signer.release("0.2.0", None);
    let volumes = Volumes::new();
    let host = FakeHost::new(&volumes.env, "0.1.0");
    host.state().hangs = true;
    let source = dir_source(&release, signer.public());
    {
        let mut updater = volumes.updater(&host, &source);
        let (request, version) = (request("0.2.0"), v("0.2.0"));
        let update = updater.update(&request, &version);
        // The updater dies while compose recreates the services.
        assert!(tokio::time::timeout(Duration::from_millis(200), update)
            .await
            .is_err());
    }
    assert!(
        volumes.pins().server.is_some(),
        "the new images were pinned"
    );
    host.state().hangs = false;

    let mut updater = volumes.updater(&host, &source);
    assert_eq!(updater.recover().await, Some(UpdateState::RolledBack));
    assert_eq!(volumes.pins(), Pins::default());
    assert_eq!(host.state().version, "0.1.0");
    let run = volumes.status().update.unwrap();
    assert_eq!(run.id, "r1", "the interrupted run is the one finished");
    assert!(run.error.unwrap().contains("restarted"));
    // Nothing left to recover.
    assert_eq!(updater.recover().await, None);
}

/// A record this updater cannot have written, whatever put it in its
/// volume: nothing of it reaches `.env` or the directories it names.
#[tokio::test]
async fn a_forged_record_of_an_update_is_not_acted_on() {
    let signer = Signer::new();
    let release = signer.release("0.2.0", None);
    let volumes = Volumes::new();
    let host = FakeHost::new(&volumes.env, "0.1.0");
    let source = dir_source(&release, signer.public());
    let env = std::fs::read_to_string(&volumes.env).unwrap();
    let kept = volumes.state.join("kept");
    std::fs::create_dir(&kept).unwrap();
    for (to, server) in [
        ("0.2.0", "old:1\nUPDATER_IMAGE=evil\nDOCKER_SOCKET=/x"),
        ("0.2.0", "${DOCKER_SOCKET}"),
        ("../../state/kept", "monitorflow-server:local"),
    ] {
        for phase in ["switching", "confirmed"] {
            let forged = serde_json::json!({
                "to": to,
                "previous": {"server": server, "web": null, "updater": null, "mirror": null},
                "migration_before": 1,
                "phase": phase,
            });
            std::fs::write(volumes.state.join("pending.json"), forged.to_string()).unwrap();
            let mut updater = volumes.updater(&host, &source);
            assert_eq!(updater.recover().await, None, "{to} {server:?} {phase}");
            assert_eq!(std::fs::read_to_string(&volumes.env).unwrap(), env);
            assert!(!volumes.state.join("pending.json").exists());
            assert!(kept.is_dir());
        }
    }
}

#[test]
fn requests_are_consumed_once_and_checked() {
    let volumes = Volumes::new();
    let inbox = volumes.update.join(INBOX);
    std::fs::create_dir(&inbox).unwrap();
    assert!(take_request(&volumes.update).is_none());
    std::fs::write(
        inbox.join(REQUEST_FILE),
        serde_json::to_vec(&request("0.2.0")).unwrap(),
    )
    .unwrap();
    let (taken, version) = take_request(&volumes.update).unwrap().unwrap();
    assert_eq!((taken.id.as_str(), version), ("r1", v("0.2.0")));
    assert!(take_request(&volumes.update).is_none());

    std::fs::write(
        inbox.join(REQUEST_FILE),
        br#"{"id": "r2", "version": "0.2.0", "image": "evil"}"#,
    )
    .unwrap();
    assert!(take_request(&volumes.update).unwrap().is_err());
    assert!(!inbox.join(REQUEST_FILE).exists());
    // A link planted as the request is refused, not followed.
    std::os::unix::fs::symlink(&volumes.env, inbox.join(REQUEST_FILE)).unwrap();
    assert!(take_request(&volumes.update).unwrap().is_err());
    assert!(volumes.env.exists());
}

#[tokio::test]
async fn http_sources_serve_the_release_and_its_agents() {
    let signer = Signer::new();
    let release = signer.release("0.2.0", None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let files = release.path().to_owned();
    tokio::spawn(async move {
        let app = axum::Router::new().route(
            "/mf/{name}",
            axum::routing::get(move |axum::extract::Path(name): axum::extract::Path<String>| {
                let path = files.join(name);
                async move {
                    std::fs::read(path).map_err(|_| axum::http::StatusCode::NOT_FOUND)
                }
            }),
        );
        axum::serve(listener, app).await.unwrap();
    });
    let http = source::Http {
        client: reqwest::Client::new(),
        source: format!("http://{addr}/mf"),
        keys: vec![signer.public()],
    };
    let candidate = http.candidate(&v("0.2.0")).await.unwrap();
    assert_eq!(candidate.base, format!("http://{addr}/mf/"));
    let dest = TempDir::new().unwrap();
    // The whole release, for the agent cache, but not what the update
    // itself reads.
    let names = http.release_files(&candidate, dest.path()).await.unwrap();
    assert_eq!(names, ["install.sh", "monitorflow-agent-linux-x86_64"]);
    assert_eq!(
        std::fs::read(dest.path().join("monitorflow-agent-linux-x86_64")).unwrap(),
        b"agent"
    );
    assert!(http.candidate(&v("0.3.0")).await.is_err());
}

fn tar_list(archive: &Path) -> Vec<String> {
    let out = std::process::Command::new("tar")
        .arg("-tzf")
        .arg(archive)
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| l.trim_end_matches('/').to_owned())
        .collect()
}

#[tokio::test]
async fn a_backup_holds_the_database_and_the_server_data_without_mirrors() {
    let volumes = Volumes::new();
    let host = FakeHost::new(&volumes.env, "0.2.0");
    let source = dir_source(
        &Signer::new().release("0.2.0", None),
        Signer::new().public(),
    );
    let mut updater = volumes.updater(&host, &source);

    let name = updater
        .backup(BackupKind::Manual, "b1".into(), "admin")
        .await
        .unwrap();
    assert_eq!(volumes.backups(), std::slice::from_ref(&name));
    let archive = volumes.backups.join(&name);
    let entries = tar_list(&archive);
    for expected in [
        "manifest.json",
        "database.dump",
        "serverdata/ca/ca-key.pem",
        "serverdata/secrets/keys",
        "serverdata/mirrors/gnupg/key",
    ] {
        assert!(
            entries.iter().any(|e| e == expected),
            "{expected} in {entries:?}"
        );
    }
    assert!(!entries.iter().any(|e| e.contains("aptly")), "{entries:?}");
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&archive).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o640, "readable by the server's group only");

    let status = volumes.status();
    let last = status.backups.last.unwrap();
    assert_eq!(
        (last.state, last.kind),
        (BackupState::Succeeded, BackupKind::Manual)
    );
    assert_eq!(last.requested_by, "admin");
    assert_eq!(last.file.as_deref(), Some(name.as_str()));
    assert_eq!(status.backups.last_success_at, last.finished_at);
    assert_eq!(status.backups.files[0].name, name);
    assert!(status.backups.files[0].size > 0);

    // Restored on a deployment whose data changed since.
    std::fs::write(volumes.serverdata.join("ca/ca-key.pem"), "new").unwrap();
    std::fs::remove_dir_all(volumes.serverdata.join("secrets")).unwrap();
    std::fs::write(volumes.serverdata.join("stray"), "x").unwrap();
    let work = TempDir::new().unwrap();
    let mut lines = Vec::new();
    let manifest: Manifest = restore::restore(
        &host,
        &archive,
        &volumes.serverdata,
        work.path(),
        &mut |l| lines.push(l.to_owned()),
    )
    .await
    .unwrap();
    assert_eq!(manifest.server_version.as_deref(), Some("0.2.0"));
    assert_eq!(manifest.migration, 20261031090000);
    assert_eq!(host.state().calls, ["stop", "restore", "start"]);
    assert_eq!(host.state().restored.as_deref(), Some(&b"PGDMP"[..]));
    assert_eq!(
        std::fs::read_to_string(volumes.serverdata.join("ca/ca-key.pem")).unwrap(),
        "secret"
    );
    // Without the key, the restored database's secrets would not open.
    let keys = volumes.serverdata.join("secrets/keys");
    assert_eq!(std::fs::read_to_string(&keys).unwrap(), "key");
    assert_eq!(
        std::fs::metadata(&keys).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!volumes.serverdata.join("stray").exists());
    assert!(
        volumes
            .serverdata
            .join("mirrors/aptly/pool/big.deb")
            .exists(),
        "mirror repositories are left alone"
    );
    assert!(lines.last().unwrap().contains("restored"), "{lines:?}");
}

#[tokio::test]
async fn a_restore_refuses_a_backup_newer_than_the_server_image() {
    let volumes = Volumes::new();
    let host = FakeHost::new(&volumes.env, "0.3.0");
    let source = dir_source(
        &Signer::new().release("0.2.0", None),
        Signer::new().public(),
    );
    let mut updater = volumes.updater(&host, &source);
    let name = updater
        .backup(BackupKind::Daily, "d".into(), "")
        .await
        .unwrap();
    host.state().version = "0.2.0".into();
    let work = TempDir::new().unwrap();
    let error = restore::restore(
        &host,
        &volumes.backups.join(name),
        &volumes.serverdata,
        work.path(),
        &mut |_| {},
    )
    .await
    .unwrap_err();
    assert!(error.contains("install 0.3.0 or later"), "{error}");
    assert!(host.state().calls.is_empty(), "nothing was touched");
}

#[tokio::test]
async fn a_failed_backup_is_reported_and_retried_within_the_hour() {
    let volumes = Volumes::new();
    let host = FakeHost::new(&volumes.env, "0.2.0");
    host.state().dump_fails = true;
    let source = dir_source(
        &Signer::new().release("0.2.0", None),
        Signer::new().public(),
    );
    let mut updater = volumes.updater(&host, &source);
    let started = Utc::now() - chrono::Duration::days(1);
    assert!(
        updater.next_backup(started) <= Utc::now(),
        "first one is due"
    );

    let error = updater
        .backup(BackupKind::Daily, "d1".into(), "")
        .await
        .unwrap_err();
    assert!(error.contains("connection refused"), "{error}");
    assert!(volumes.backups().is_empty(), "nothing partial is left");
    let last = volumes.status().backups.last.unwrap();
    assert_eq!(last.state, BackupState::Failed);
    assert!(last.error.unwrap().contains("connection refused"));
    let retry = updater.next_backup(started) - Utc::now();
    assert!(retry > chrono::Duration::minutes(55) && retry <= chrono::Duration::hours(1));

    host.state().dump_fails = false;
    updater
        .backup(BackupKind::Daily, "d2".into(), "")
        .await
        .unwrap();
    let next = updater.next_backup(started) - Utc::now();
    assert!(next > chrono::Duration::hours(23), "{next}");
    // A failed backup stops an update before anything changes.
    host.state().dump_fails = true;
    let signer = Signer::new();
    let release = signer.release("0.3.0", None);
    let source = dir_source(&release, signer.public());
    let mut updater = volumes.updater(&host, &source);
    assert_eq!(
        updater.update(&request("0.3.0"), &v("0.3.0")).await,
        UpdateState::Failed
    );
    assert!(volumes
        .status()
        .update
        .unwrap()
        .error
        .unwrap()
        .contains("backup failed"));
    assert_eq!(volumes.pins(), Pins::default());
}

#[tokio::test]
async fn old_backups_and_dumps_are_pruned_but_a_needed_one_is_kept() {
    let volumes = Volumes::new();
    let host = FakeHost::new(&volumes.env, "0.2.0");
    let source = dir_source(
        &Signer::new().release("0.2.0", None),
        Signer::new().public(),
    );
    let legacy = "monitorflow-0.1.0-to-0.2.0-20260101T000000Z.dump";
    // One clock reading: names have seconds, and the test may cross one.
    let now = Utc::now();
    let old = |days: i64| file_name(now - chrono::Duration::days(days), BackupKind::Daily);
    std::fs::write(volumes.backups.join(legacy), "PGDMP").unwrap();
    std::fs::File::options()
        .write(true)
        .open(volumes.backups.join(legacy))
        .unwrap()
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(20 * 86400))
        .unwrap();
    let needed = old(30);
    for days in 1..=10 {
        std::fs::write(volumes.backups.join(old(days)), "x").unwrap();
    }
    std::fs::write(volumes.backups.join(&needed), "x").unwrap();
    // The last update rolled back after migrating: its backup is needed.
    let mut updater = volumes.updater(&host, &source);
    updater.reporter.status.update = Some(monitorflow_common::server_update::UpdateRun {
        id: "u".into(),
        from: Some("0.1.0".into()),
        to: "0.2.0".into(),
        requested_by: "admin".into(),
        state: UpdateState::RolledBack,
        steps: Vec::new(),
        error: None,
        backup: Some(needed.clone()),
        migrated: true,
        started_at: Utc::now(),
        finished_at: Some(Utc::now()),
    });
    let new = updater
        .backup(BackupKind::Manual, "m".into(), "admin")
        .await
        .unwrap();
    let mut left = volumes.backups();
    left.sort();
    assert_eq!(left.len(), KEEP, "{left:?}");
    assert!(left.contains(&new) && left.contains(&needed), "{left:?}");
    assert!(!left.contains(&legacy.to_owned()));
    assert!(
        left.contains(&old(1)) && !left.contains(&old(6)),
        "{left:?}"
    );
    assert_eq!(volumes.status().backups.files.len(), KEEP);
}

#[test]
fn backup_requests_are_consumed_once_and_checked() {
    let volumes = Volumes::new();
    let inbox = volumes.update.join(INBOX);
    std::fs::create_dir(&inbox).unwrap();
    assert!(take_backup_request(&volumes.update).is_none());
    std::fs::write(
        inbox.join(BACKUP_REQUEST_FILE),
        br#"{"id": "b1", "requested_by": "admin", "requested_at": "2026-10-29T00:00:00Z"}"#,
    )
    .unwrap();
    assert_eq!(
        take_backup_request(&volumes.update).unwrap().unwrap().id,
        "b1"
    );
    assert!(take_backup_request(&volumes.update).is_none());
    std::fs::write(
        inbox.join(BACKUP_REQUEST_FILE),
        br#"{"id": "b2", "dir": "/"}"#,
    )
    .unwrap();
    assert!(take_backup_request(&volumes.update).unwrap().is_err());
}

#[test]
fn an_interrupted_backup_is_marked_failed_on_restart() {
    let volumes = Volumes::new();
    let host = FakeHost::new(&volumes.env, "0.2.0");
    let source = dir_source(
        &Signer::new().release("0.2.0", None),
        Signer::new().public(),
    );
    let mut updater = volumes.updater(&host, &source);
    updater.reporter.status.backups.last =
        Some(monitorflow_common::server_update::backup::BackupRun {
            id: "d".into(),
            kind: BackupKind::Daily,
            requested_by: String::new(),
            state: BackupState::Running,
            error: None,
            warning: None,
            file: None,
            started_at: Utc::now(),
            finished_at: None,
        });
    std::fs::create_dir(volumes.backups.join(".staging-x")).unwrap();
    std::fs::write(volumes.backups.join(".x.part"), "x").unwrap();
    updater.recover_backup();
    let last = volumes.status().backups.last.unwrap();
    assert_eq!(last.state, BackupState::Failed);
    assert!(volumes.backups().is_empty());
}

#[tokio::test]
async fn retention_changes_reach_the_env_file_and_failures_undo_them() {
    let volumes = Volumes::new();
    let host = FakeHost::new(&volumes.env, "0.2.0");
    let source = dir_source(&TempDir::new().unwrap(), Signer::new().public());
    let mut updater = volumes.updater(&host, &source);
    let change = |metrics_days, logs_days| RetentionRequest {
        id: "r1".into(),
        metrics_days,
        logs_days,
        requested_by: "alice".into(),
        requested_at: Utc::now(),
    };
    updater.read_retention();
    let status = |updater: &Updater<'_>| updater.reporter.status.retention.clone().unwrap();
    assert_eq!(status(&updater).metrics.value, "90d");

    updater.set_retention(&change(Some(60), None)).await;
    let retention = status(&updater);
    assert_eq!(retention.metrics.value, "60d");
    assert_eq!(retention.logs.value, "30d");
    assert!(!retention.applying);
    assert_eq!(retention.error, None);
    assert_eq!(host.state().calls, ["stores"]);

    host.state().stores_fail = true;
    updater.set_retention(&change(Some(7), Some(7))).await;
    let retention = status(&updater);
    assert_eq!(retention.metrics.value, "60d", "the old value is back");
    assert_eq!(retention.logs.value, "30d");
    assert!(retention.error.unwrap().contains("did not start"));
    assert_eq!(host.state().calls, ["stores", "stores", "stores"]);
    let written = std::fs::read_to_string(&volumes.env).unwrap();
    assert!(written.contains("UI_METRICS_RETENTION=60d\n"), "{written}");
    assert!(!written.contains("UI_LOGS_RETENTION"), "{written}");
}

/// The bundle of the updater image and a deploy directory holding an older
/// release's files (ADR-0087).
struct ComposeDirs {
    bundle: TempDir,
    deploy: PathBuf,
    files: Vec<PathBuf>,
}

impl ComposeDirs {
    fn new(volumes: &Volumes) -> Self {
        let bundle = TempDir::new().unwrap();
        let deploy = volumes.env.parent().unwrap().to_owned();
        for (dir, text) in [(bundle.path(), "tls"), (deploy.as_path(), "plain")] {
            for file in compose_file::FILES {
                let path = dir.join(file);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, format!("{file}: {text}\n")).unwrap();
            }
        }
        let files = vec![deploy.join("docker-compose.yml")];
        Self {
            bundle,
            deploy,
            files,
        }
    }

    fn compose(&self) -> String {
        std::fs::read_to_string(self.deploy.join("docker-compose.yml")).unwrap()
    }

    async fn refresh(&self, volumes: &Volumes, host: &FakeHost) {
        let source = source::Dir {
            dir: self.bundle.path().to_owned(),
            keys: vec![],
        };
        let mut updater = volumes.updater(host, &source);
        updater
            .refresh_compose(self.bundle.path(), &self.deploy, &self.files)
            .await;
    }
}

/// The status an update to this release left, as the next updater reads it.
fn last_update(volumes: &Volumes, requested_by: &str) {
    let status = Status {
        updater_version: "0.1.0".into(),
        heartbeat_at: Utc::now(),
        running: None,
        update: Some(UpdateRun {
            id: "web-1".into(),
            from: Some("0.1.0".into()),
            to: monitorflow_common::VERSION.into(),
            requested_by: requested_by.into(),
            state: UpdateState::Succeeded,
            steps: Vec::new(),
            error: None,
            backup: None,
            migrated: false,
            started_at: Utc::now(),
            finished_at: Some(Utc::now()),
        }),
        backups: Default::default(),
        retention: None,
    };
    std::fs::write(
        volumes.update.join(STATUS_FILE),
        serde_json::to_vec(&status).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn the_compose_file_follows_an_update_from_the_web_interface() {
    let volumes = Volumes::new();
    let dirs = ComposeDirs::new(&volumes);
    let host = FakeHost::new(&volumes.env, monitorflow_common::VERSION);
    last_update(&volumes, "admin");

    dirs.refresh(&volumes, &host).await;
    assert_eq!(dirs.compose(), "docker-compose.yml: tls\n");
    assert_eq!(host.state().calls, ["apply", "check"]);
    assert!(!compose_file::pending(&volumes.state));
    // The password of the server's own database role, which the compose
    // file of ADR-0109 needs, is added once and kept.
    let env = std::fs::read_to_string(&volumes.env).unwrap();
    let line = env
        .lines()
        .find(|l| l.starts_with("POSTGRES_APP_PASSWORD="))
        .expect("a password for the server's role");
    assert_eq!(line.len(), "POSTGRES_APP_PASSWORD=".len() + 48);
    assert!(env.contains("POSTGRES_PASSWORD=x\n"), "{env}");
    let steps = volumes.status().update.unwrap().steps;
    assert!(steps
        .iter()
        .any(|s| s.message.contains("compose file of this release in place")));

    // Once per update: a restart changes nothing, even if the file did.
    std::fs::write(dirs.deploy.join("docker-compose.yml"), "edited\n").unwrap();
    dirs.refresh(&volumes, &host).await;
    assert_eq!(dirs.compose(), "edited\n");
    assert_eq!(host.state().calls, ["apply", "check"]);
    assert_eq!(std::fs::read_to_string(&volumes.env).unwrap(), env);
}

#[tokio::test]
async fn a_compose_file_that_does_not_work_is_put_back() {
    let volumes = Volumes::new();
    let dirs = ComposeDirs::new(&volumes);
    let host = FakeHost::new(&volumes.env, monitorflow_common::VERSION);
    last_update(&volumes, "admin");
    host.state().check_fails = true;

    dirs.refresh(&volumes, &host).await;
    assert_eq!(dirs.compose(), "docker-compose.yml: plain\n");
    assert_eq!(host.state().calls, ["apply", "check", "apply"]);
    assert!(!compose_file::pending(&volumes.state));
    let steps = volumes.status().update.unwrap().steps;
    let last = &steps.last().unwrap().message;
    assert!(
        last.contains("did not work (VictoriaMetrics does not answer); the previous one is back"),
        "{last}"
    );
    // Not tried again at each restart.
    dirs.refresh(&volumes, &host).await;
    assert_eq!(host.state().calls, ["apply", "check", "apply"]);
}

#[tokio::test]
async fn the_installer_and_the_offline_path_keep_their_compose_file() {
    let volumes = Volumes::new();
    let dirs = ComposeDirs::new(&volumes);
    let host = FakeHost::new(&volumes.env, monitorflow_common::VERSION);
    dirs.refresh(&volumes, &host).await;
    last_update(&volumes, COMMAND_LINE);
    dirs.refresh(&volumes, &host).await;
    assert_eq!(dirs.compose(), "docker-compose.yml: plain\n");
    assert!(host.state().calls.is_empty());
    // The installer adds the line itself.
    let env = std::fs::read_to_string(&volumes.env).unwrap();
    assert!(!env.contains("POSTGRES_APP_PASSWORD"), "{env}");
}

#[tokio::test]
async fn an_interrupted_compose_change_is_finished_on_restart() {
    let volumes = Volumes::new();
    let dirs = ComposeDirs::new(&volumes);
    // Killed after saving the files and writing the new ones.
    compose_file::save_previous(&dirs.deploy, &volumes.state).unwrap();
    compose_file::install(dirs.bundle.path(), &dirs.deploy).unwrap();
    let host = FakeHost::new(&volumes.env, monitorflow_common::VERSION);
    dirs.refresh(&volumes, &host).await;
    assert_eq!(host.state().calls, ["apply", "check"]);
    assert!(!compose_file::pending(&volumes.state));
}
