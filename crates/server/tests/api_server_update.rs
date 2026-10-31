//! Server update endpoints (ADR-0056) against a real PostgreSQL, a
//! directory standing in for the updater's volume and an HTTP server
//! serving signed releases. Ignored without `DATABASE_URL`.

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use monitorflow_common::redact::Secret;
use monitorflow_common::release::{sha256, PublicKey};
use monitorflow_common::server_update::backup::{
    file_name, BackupFile, BackupKind, BackupRun, BackupState, Backups,
};
use monitorflow_common::server_update::{Running, Status, UpdateRun, UpdateState};
use monitorflow_server::agent_releases::ReleaseCache;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::server_update::ServerUpdateSettings;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use monitorflow_server::release_check;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::{json, Value};
use sqlx::PgPool;
use tempfile::TempDir;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
const NEWER: &str = "99.0.0";
const AGENT: &str = "monitorflow-agent-linux-x86_64";

macro_rules! db_tests {
    ($($item:item)*) => {
        $(
            #[cfg_attr(
                not(db_tests),
                ignore = "DATABASE_URL is not set; run `docker compose -f deploy/docker-compose.dev.yml up -d postgres` and export DATABASE_URL from .env.example"
            )]
            $item
        )*
    };
}

fn image(name: &str) -> String {
    format!("registry.test/monitorflow-{name}@sha256:{}", "a".repeat(64))
}

/// A signed release of `version` in a directory, and the key that trusts
/// it.
fn release(version: &str) -> (TempDir, PublicKey) {
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
    let mut der = vec![
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    der.extend_from_slice(pair.public_key().as_ref());
    let dir = TempDir::new().unwrap();
    let images = json!({"version": version, "images": {
        "server": image("server"), "web": image("web"), "updater": image("updater")}});
    let files = [
        ("VERSION", format!("{version}\n").into_bytes()),
        ("images.json", images.to_string().into_bytes()),
        // Past the API's usual body limit, as real binaries are.
        (AGENT, vec![b'x'; 256 * 1024]),
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
        pair.sign(sums.as_bytes()).as_ref(),
    )
    .unwrap();
    (dir, PublicKey::from_spki(&der).unwrap())
}

/// Serves `dir` at `http://…/mf/`.
async fn serve(dir: PathBuf) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = axum::Router::new().route(
        "/mf/{name}",
        axum::routing::get(
            move |axum::extract::Path(name): axum::extract::Path<String>| {
                let path = dir.join(name);
                async move { std::fs::read(path).map_err(|_| StatusCode::NOT_FOUND) }
            },
        ),
    );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/mf")
}

struct Harness {
    pool: PgPool,
    settings: ApiSettings,
    volume: TempDir,
}

impl Harness {
    fn new(pool: &PgPool, source: &str, keys: Vec<PublicKey>) -> Self {
        let volume = TempDir::new().unwrap();
        std::fs::create_dir(volume.path().join("inbox")).unwrap();
        std::fs::create_dir(volume.path().join("backups")).unwrap();
        let settings = ApiSettings {
            server_update: ServerUpdateSettings {
                dir: Some(volume.path().to_owned()),
                backup_dir: Some(volume.path().join("backups")),
                source: source.into(),
                keys,
                client: reqwest::Client::new(),
            },
            ..ApiSettings::default()
        };
        Self {
            pool: pool.clone(),
            settings,
            volume,
        }
    }

    fn dir(&self) -> &Path {
        self.volume.path()
    }

    /// What the updater would write.
    fn write_status(&self, age_s: i64, update: Option<UpdateRun>) {
        self.write_full_status(age_s, update, Backups::default());
    }

    fn write_full_status(&self, age_s: i64, update: Option<UpdateRun>, backups: Backups) {
        let status = Status {
            updater_version: "0.1.0".into(),
            heartbeat_at: Utc::now() - chrono::Duration::seconds(age_s),
            running: Some(Running {
                version: monitorflow_common::VERSION.into(),
                server_image: None,
                web_image: Some(image("web")),
                updater_image: None,
            }),
            update,
            backups,
        };
        std::fs::write(
            self.dir().join("status.json"),
            serde_json::to_vec(&status).unwrap(),
        )
        .unwrap();
    }

    async fn call(
        &self,
        method: &str,
        uri: &str,
        cookie: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let (status, _, bytes) = self.call_raw(method, uri, cookie, body).await;
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn call_raw(
        &self,
        method: &str,
        uri: &str,
        cookie: &str,
        body: Option<Value>,
    ) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        let state = ApiState::new(
            self.pool.clone(),
            Registry::default(),
            self.settings.clone(),
        );
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header(CSRF_HEADER, "1")
            .header("content-type", "application/json")
            .header("cookie", cookie)
            .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
            .unwrap();
        let response = api::router(state).oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 22)
            .await
            .unwrap();
        (status, headers, bytes.to_vec())
    }

    /// Posts `names` from `dir` as the web page does, with a stray file.
    async fn upload(&self, cookie: &str, dir: &Path, names: &[&str]) -> (StatusCode, Value) {
        let mut body = Vec::new();
        for name in names.iter().chain(&["notes.txt"]) {
            let head = format!(
                "--xx\r\ncontent-disposition: form-data; name=\"files\"; filename=\"{name}\"\r\n\r\n"
            );
            body.extend_from_slice(head.as_bytes());
            body.extend_from_slice(&std::fs::read(dir.join(name)).unwrap_or_default());
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(b"--xx--\r\n");
        let state = ApiState::new(
            self.pool.clone(),
            Registry::default(),
            self.settings.clone(),
        );
        let request = Request::builder()
            .method("POST")
            .uri("/api/agent-releases/import")
            .header(CSRF_HEADER, "1")
            .header("content-type", "multipart/form-data; boundary=xx")
            .header("cookie", cookie)
            .body(Body::from(body))
            .unwrap();
        let response = api::router(state).oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 22)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// Imports go to a cache of this harness, trusting `key`.
    fn with_cache(mut self, key: PublicKey) -> Self {
        let mirror = self.volume.path().join("mirror");
        self.settings.agent_releases.cache = ReleaseCache::in_mirror_dir(&mirror);
        self.settings.agent_releases.keys = vec![key];
        self
    }

    fn cached(&self) -> Vec<String> {
        let cache = &self.settings.agent_releases.cache;
        let versions = cache.versions().unwrap();
        versions.iter().map(ToString::to_string).collect()
    }

    /// Nothing but versions is left in the cache.
    fn assert_no_leftovers(&self) {
        let cache = &self.settings.agent_releases.cache;
        let names: Vec<_> = std::fs::read_dir(cache.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(names.iter().all(|n| !n.starts_with('.')), "{names:?}");
    }

    async fn user(&self, name: &str, role: Role) -> String {
        users::create_user_with(
            &self.pool,
            name,
            &Secret::new(PASSWORD),
            role,
            GrantScope::Global,
            "test",
        )
        .await
        .unwrap();
        let state = ApiState::new(
            self.pool.clone(),
            Registry::default(),
            ApiSettings::default(),
        );
        let request = Request::builder()
            .method("POST")
            .uri("/api/auth/login")
            .header(CSRF_HEADER, "1")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "username": name, "password": PASSWORD }).to_string(),
            ))
            .unwrap();
        let response = api::router(state).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response
            .headers()
            .get("set-cookie")
            .unwrap()
            .to_str()
            .unwrap();
        cookie.split(';').next().unwrap().to_owned()
    }
}

fn run(state: UpdateState) -> UpdateRun {
    UpdateRun {
        id: "r0".into(),
        from: Some("0.0.9".into()),
        to: monitorflow_common::VERSION.into(),
        requested_by: "root".into(),
        state,
        steps: vec![],
        error: None,
        backup: Some("monitorflow-0.0.9-to-x.dump".into()),
        migrated: false,
        started_at: Utc::now(),
        finished_at: None,
    }
}

db_tests! {
    #[sqlx::test]
    async fn only_global_administrators_see_and_start_updates(pool: PgPool) {
        let h = Harness::new(&pool, "http://127.0.0.1:9/mf", vec![]);
        let viewer = h.user("viewer", Role::Viewer).await;
        for (method, uri) in [
            ("GET", "/api/server-update"),
            ("POST", "/api/server-update/check"),
            ("POST", "/api/server-update"),
        ] {
            let body = (method == "POST").then(|| json!({ "version": NEWER }));
            let (status, _) = h.call(method, uri, &viewer, body).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
        }
        let (status, _) = h.call("GET", "/api/server-update", "", None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[sqlx::test]
    async fn the_status_is_the_updaters(pool: PgPool) {
        let h = Harness::new(&pool, "http://127.0.0.1:9/mf", vec![]);
        let admin = h.user("root", Role::Administrator).await;
        let (status, body) = h.call("GET", "/api/server-update", &admin, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["running_version"], monitorflow_common::VERSION);
        assert_eq!(body["enabled"], true);
        assert_eq!(body["updater"], Value::Null);
        assert_eq!(body["requested"], false);

        let mut done = run(UpdateState::RolledBack);
        done.migrated = true;
        h.write_status(10, Some(done));
        let (_, body) = h.call("GET", "/api/server-update", &admin, None).await;
        assert_eq!(body["updater"]["alive"], true);
        assert_eq!(body["updater"]["running"]["web_image"], image("web"));
        assert_eq!(body["update"]["state"], "rolled_back");
        assert_eq!(body["update"]["migrated"], true);

        h.write_status(3600, None);
        let (_, body) = h.call("GET", "/api/server-update", &admin, None).await;
        assert_eq!(body["updater"]["alive"], false);

        let mut off = Harness::new(&pool, "http://127.0.0.1:9/mf", vec![]);
        off.settings.server_update.dir = None;
        let (_, body) = off.call("GET", "/api/server-update", &admin, None).await;
        assert_eq!(body["enabled"], false);
    }

    #[sqlx::test]
    async fn checks_verify_the_latest_release_and_say_what_changes(pool: PgPool) {
        let (files, key) = release(NEWER);
        let url = serve(files.path().to_owned()).await;
        let h = Harness::new(&pool, &url, vec![key.clone()]);
        h.write_status(5, None);
        let admin = h.user("root", Role::Administrator).await;
        let (status, body) = h.call("POST", "/api/server-update/check", &admin, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["reachable"], true);
        assert_eq!(body["error"], Value::Null);
        let latest = &body["latest"];
        assert_eq!(latest["version"], NEWER);
        assert_eq!(latest["newer"], true);
        assert_eq!(latest["release_notes_url"], Value::Null, "not GitHub");
        assert_eq!(latest["images"]["server"], image("server"));
        assert_eq!(latest["web_changes"], false, "the same web image runs");
        assert_eq!(latest["updater_changes"], true, "a local build runs");
        assert_eq!(latest["agent_release_cached"], false);

        // Another key signed it: no release is offered.
        let (_, other) = release(NEWER);
        let h = Harness::new(&pool, &url, vec![other]);
        let (_, body) = h.call("POST", "/api/server-update/check", &admin, None).await;
        assert_eq!((body["reachable"].clone(), body["latest"].clone()), (json!(true), Value::Null));
        assert!(body["error"].as_str().unwrap().contains("does not verify"), "{body}");

        // An offline server.
        let h = Harness::new(&pool, "http://127.0.0.1:9/mf", vec![key]);
        let (status, body) = h.call("POST", "/api/server-update/check", &admin, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["reachable"], false);
        assert!(body["error"].as_str().unwrap().contains("127.0.0.1:9"), "{body}");
    }

    #[sqlx::test]
    async fn starting_an_update_writes_one_audited_request(pool: PgPool) {
        let h = Harness::new(&pool, "http://127.0.0.1:9/mf", vec![]);
        let admin = h.user("root", Role::Administrator).await;
        let start = |version: &str| json!({ "version": version });

        let (status, body) = h.call("POST", "/api/server-update", &admin, Some(start(NEWER))).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("the updater is not running")));

        h.write_status(5, Some(run(UpdateState::Succeeded)));
        for (version, why) in [("0.0.1", "older"), (monitorflow_common::VERSION, "the same"), ("latest", "not a version")] {
            let (status, _) = h.call("POST", "/api/server-update", &admin, Some(start(version))).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{why}");
        }
        let (status, _) = h.call("POST", "/api/server-update", &admin, Some(json!({ "version": NEWER, "image": "evil" }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, body) = h.call("POST", "/api/server-update", &admin, Some(start(NEWER))).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        let id = body["id"].as_str().unwrap().to_owned();
        let written: Value = serde_json::from_slice(&std::fs::read(h.dir().join("inbox/request.json")).unwrap()).unwrap();
        assert_eq!(written["id"], id.as_str());
        assert_eq!(written["version"], NEWER);
        assert_eq!(written["requested_by"], "root");

        // One at a time: while the request waits, and while it runs.
        let (status, _) = h.call("POST", "/api/server-update", &admin, Some(start(NEWER))).await;
        assert_eq!(status, StatusCode::CONFLICT);
        std::fs::remove_file(h.dir().join("inbox/request.json")).unwrap();
        h.write_status(5, Some(run(UpdateState::BackingUp)));
        let (status, _) = h.call("POST", "/api/server-update", &admin, Some(start(NEWER))).await;
        assert_eq!(status, StatusCode::CONFLICT);

        let (details,): (Value,) = sqlx::query_as("SELECT details FROM audit_log WHERE action = 'server.update'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(details["request_id"], id.as_str());
        assert_eq!(details["to"], NEWER);
        assert_eq!(details["from"], monitorflow_common::VERSION);

        let mut off = Harness::new(&pool, "http://127.0.0.1:9/mf", vec![]);
        off.settings.server_update.dir = None;
        let (status, _) = off.call("POST", "/api/server-update", &admin, Some(start(NEWER))).await;
        assert_eq!(status, StatusCode::CONFLICT);
    }

    #[sqlx::test]
    async fn backups_are_shown_requested_and_downloaded_by_global_administrators(pool: PgPool) {
        let h = Harness::new(&pool, "http://127.0.0.1:9/mf", vec![]);
        let admin = h.user("root", Role::Administrator).await;
        let viewer = h.user("viewer", Role::Viewer).await;
        let name = file_name(Utc::now(), BackupKind::Daily);
        for (method, uri) in [("GET", "/api/backups".to_owned()), ("POST", "/api/backups".to_owned()), ("GET", format!("/api/backups/{name}"))] {
            let (status, _, _) = h.call_raw(method, &uri, &viewer, None).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
        }

        let (status, body) = h.call("POST", "/api/backups", &admin, None).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("the updater is not running")));

        std::fs::write(h.dir().join("backups").join(&name), b"archive").unwrap();
        let failed = BackupRun {
            id: "d1".into(),
            kind: BackupKind::Daily,
            requested_by: String::new(),
            state: BackupState::Failed,
            error: Some("pg_dump: connection refused".into()),
            warning: None,
            file: None,
            started_at: Utc::now(),
            finished_at: Some(Utc::now()),
        };
        let backups = Backups {
            last: Some(failed),
            last_success_at: Some(Utc::now()),
            next_at: Some(Utc::now()),
            files: vec![BackupFile { name: name.clone(), size: 7, created_at: Utc::now(), kind: BackupKind::Daily }],
        };
        h.write_full_status(5, None, backups.clone());
        let (status, body) = h.call("GET", "/api/backups", &admin, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!((body["enabled"].clone(), body["downloads"].clone(), body["updater_alive"].clone()), (json!(true), json!(true), json!(true)));
        assert_eq!(body["keep"], 7);
        assert_eq!(body["last"]["state"], "failed");
        assert_eq!(body["last"]["error"], "pg_dump: connection refused");
        assert_eq!(body["files"][0]["name"], name.as_str());

        let (status, body) = h.call("POST", "/api/backups", &admin, None).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        let written: Value = serde_json::from_slice(&std::fs::read(h.dir().join("inbox/backup.json")).unwrap()).unwrap();
        assert_eq!((written["id"].as_str(), written["requested_by"].as_str()), (body["id"].as_str(), Some("root")));
        let (_, body) = h.call("GET", "/api/backups", &admin, None).await;
        assert_eq!(body["requested"], true);
        let (status, _) = h.call("POST", "/api/backups", &admin, None).await;
        assert_eq!(status, StatusCode::CONFLICT, "one at a time");

        let (status, headers, bytes) = h.call_raw("GET", &format!("/api/backups/{name}"), &admin, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(bytes, b"archive");
        assert_eq!(headers["content-disposition"], format!("attachment; filename=\"{name}\"").as_str());
        assert_eq!(headers["cache-control"], "no-store");
        for bad in ["status.json", "..%2Fstatus.json", "monitorflow-backup-20260101T000000Z-daily.tar.gz"] {
            let (status, _, _) = h.call_raw("GET", &format!("/api/backups/{bad}"), &admin, None).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{bad}");
        }
        let audited: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action IN ('server.backup', 'server.backup.download')")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(audited, 2);
    }

    #[sqlx::test]
    async fn the_release_check_tells_of_a_version_once(pool: PgPool) {
        let (files, key) = release(NEWER);
        let url = serve(files.path().to_owned()).await;
        let h = Harness::new(&pool, &url, vec![key.clone()]);
        let admin = h.user("root", Role::Administrator).await;
        let settings = &h.settings.server_update;
        let running = release_check::running_version();
        let told = || async {
            let rows: Vec<(Value,)> = sqlx::query_as(
                "SELECT details FROM notification_outbox WHERE event = 'update.available'",
            )
            .fetch_all(&pool)
            .await
            .unwrap();
            rows.into_iter().map(|r| r.0).collect::<Vec<_>>()
        };

        let (_, body) = h.call("GET", "/api/server-update", &admin, None).await;
        assert_eq!(body["available"], Value::Null);

        assert!(release_check::check(&pool, settings, &running).await);
        let events = told().await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["release"]["version"], NEWER);
        assert_eq!(events[0]["release"]["running_version"], monitorflow_common::VERSION);
        let (_, body) = h.call("GET", "/api/server-update", &admin, None).await;
        assert_eq!(body["available"]["version"], NEWER);

        // Once per version, even after the outbox forgot it or from a
        // check by hand.
        sqlx::query("DELETE FROM notification_outbox").execute(&pool).await.unwrap();
        assert!(release_check::check(&pool, settings, &running).await);
        let (status, _) = h.call("POST", "/api/server-update/check", &admin, None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(told().await.is_empty());

        // The running version catches up: the banner goes.
        let same: monitorflow_common::release::Version = NEWER.parse().unwrap();
        assert_eq!(release_check::available(&pool, &same).await.unwrap(), None);

        // A release that is no newer tells nobody, and replaces what the
        // source offered before.
        let (files, key) = release(monitorflow_common::VERSION);
        let url = serve(files.path().to_owned()).await;
        let current = Harness::new(&pool, &url, vec![key]);
        assert!(release_check::check(&pool, &current.settings.server_update, &running).await);
        assert!(told().await.is_empty());
        let (_, body) = h.call("GET", "/api/server-update", &admin, None).await;
        assert_eq!(body["available"], Value::Null);
    }

    #[sqlx::test]
    async fn a_failed_release_check_is_quiet(pool: PgPool) {
        let (files, _) = release(NEWER);
        let (_, other) = release(NEWER);
        let url = serve(files.path().to_owned()).await;
        let running = release_check::running_version();
        for h in [
            Harness::new(&pool, "http://127.0.0.1:9/mf", vec![other.clone()]),
            Harness::new(&pool, &url, vec![other.clone()]),
        ] {
            assert!(!release_check::check(&pool, &h.settings.server_update, &running).await);
        }
        assert_eq!(release_check::available(&pool, &running).await.unwrap(), None);
        let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM notification_outbox")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[sqlx::test]
    async fn releases_import_from_files_the_browser_uploads(pool: PgPool) {
        let (files, key) = release("0.3.0");
        let h = Harness::new(&pool, "http://127.0.0.1:9/mf", vec![]).with_cache(key);
        let viewer = h.user("viewer", Role::Viewer).await;
        let admin = h.user("root", Role::Administrator).await;
        let all = ["SHA256SUMS", "SHA256SUMS.sig", "VERSION", AGENT];

        let (status, _) = h.upload(&viewer, files.path(), &all).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let (status, body) = h.upload(&admin, files.path(), &all[1..]).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().contains("SHA256SUMS:"), "{body}");

        let (status, body) = h.upload(&admin, files.path(), &all).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["version"], "0.3.0");
        assert!(body["missing"].as_array().unwrap().contains(&json!("install.sh")));
        assert_eq!(h.cached(), ["0.3.0"]);
        h.assert_no_leftovers();

        let (actor, target, details): (String, String, Value) = sqlx::query_as(
            "SELECT actor_id, target_id, details FROM audit_log
             WHERE action = 'agent_release.import'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(!actor.is_empty());
        assert_eq!(target, "0.3.0");
        assert_eq!(details["from"], "upload");
    }

    #[sqlx::test]
    async fn uploads_that_do_not_verify_leave_no_trace(pool: PgPool) {
        let (files, _) = release("0.3.0");
        let (_, other) = release("0.3.0");
        let h = Harness::new(&pool, "http://127.0.0.1:9/mf", vec![]).with_cache(other);
        let admin = h.user("root", Role::Administrator).await;
        let all = ["SHA256SUMS", "SHA256SUMS.sig", "VERSION", AGENT];
        let (status, body) = h.upload(&admin, files.path(), &all).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().contains("not a genuine"), "{body}");
        assert!(h.cached().is_empty());
        h.assert_no_leftovers();
    }

    #[sqlx::test]
    async fn releases_import_from_a_url(pool: PgPool) {
        let (files, key) = release("0.3.0");
        let url = serve(files.path().to_owned()).await;
        let h = Harness::new(&pool, "http://127.0.0.1:9/mf", vec![]).with_cache(key);
        let admin = h.user("root", Role::Administrator).await;
        let import = "/api/agent-releases/import-url";

        let (status, body) = h.call("POST", import, &admin, Some(json!({ "url": "nope" }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().contains("not a URL"));

        let (status, body) = h
            .call("POST", import, &admin, Some(json!({ "url": "http://127.0.0.1:9/mf" })))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().contains("Could not download"), "{body}");

        // A directory without a release.
        let empty = TempDir::new().unwrap();
        let nothing = serve(empty.path().to_owned()).await;
        let (status, body) = h.call("POST", import, &admin, Some(json!({ "url": nothing }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().contains("A release needs"), "{body}");

        let (status, body) = h
            .call("POST", import, &admin, Some(json!({ "url": format!("{url}/") })))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["version"], "0.3.0");
        assert_eq!(h.cached(), ["0.3.0"]);
        h.assert_no_leftovers();
    }
}
