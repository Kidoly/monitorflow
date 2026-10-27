//! The updater's flow (ADR-0056) against a fake Docker host and real
//! signed release directories, served from disk or over HTTP.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use monitorflow_common::release::{sha256, PublicKey, Version};
use monitorflow_common::server_update::{
    ImageRef, Request, Status, UpdateState, REQUEST_FILE, STATUS_FILE,
};
use monitorflow_updater::env_file::{EnvFile, Pins};
use monitorflow_updater::host::{Host, HostError};
use monitorflow_updater::source::{self, Source};
use monitorflow_updater::update::{Reporter, Updater};
use monitorflow_updater::{take_request, INBOX};
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
            r#"{{"version": "{version}", "images": {{"server": "{}", "web": "{}", "updater": "{}"}}}}"#,
            server.unwrap_or(image("server", version)),
            image("web", version),
            image("updater", version)
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
        std::fs::write(dest, b"PGDMP").map_err(|e| HostError(e.to_string()))
    }

    async fn recreate(&self) -> Result<(), HostError> {
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
}

/// The volumes and the project's `.env`.
struct Volumes {
    _root: TempDir,
    update: PathBuf,
    state: PathBuf,
    backups: PathBuf,
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
        let env = deploy.join(".env");
        std::fs::write(&env, "DOMAIN=mf.test\nPOSTGRES_PASSWORD=x\n").unwrap();
        Self {
            update,
            state,
            backups,
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
    assert_eq!(host.state().pulled.len(), 3);
    // The agent release went through the server's import, from the path
    // the server container sees, then the staging copy was removed.
    assert_eq!(
        host.state().imported,
        [PathBuf::from("/var/lib/monitorflow/update/release/0.2.0")]
    );
    assert!(!volumes.update.join("release/0.2.0").exists());
    assert!(host.state().replaced, "the updater replaces itself");

    let backups = volumes.backups();
    assert_eq!(backups.len(), 1, "{backups:?}");
    assert!(
        backups[0].starts_with("monitorflow-0.1.0-to-0.2.0-"),
        "{backups:?}"
    );
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
