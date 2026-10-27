//! Campaigns pinned to mirror snapshots (ADR-0038): validation through
//! the API, the runner's configure → update → restore sequence per
//! server, restores after a pause or a failed configuration, and the
//! `.pinned` files relays learn the snapshots from. Ignored without
//! `DATABASE_URL`.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use monitorflow_common::command::{APPLY_UPDATES, CONFIGURE_SOURCES, REVERT_SOURCES};
use monitorflow_common::mirror::read_pinned;
use monitorflow_common::proto::v1::configure_sources::Mode;
use monitorflow_common::proto::v1::{CommandResult, CommandStatus, ConfigureSources};
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::campaign::{
    write_pins, Action, CampaignState, CampaignStore, FailureThreshold, NewCampaign, PinnedMirror,
    Runner, SourcesMode, SourcesPin, Step, UpdateSpec, WaveSpec,
};
use monitorflow_server::commands::{CommandStore, NewCommand, State};
use monitorflow_server::grpc::Registry;
use monitorflow_server::mirror::{
    Family, Invocation, Layout, Mirrors, Output, RunError, Runner as Tool,
};
use prost::Message;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";
const USER: (&str, &str) = ("user", "tester");
const URL: &str = "http://mf.example:7480";

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

/// gpg knowing one secret key.
struct FakeGpg;

#[async_trait::async_trait]
impl Tool for FakeGpg {
    async fn run(&self, _: &Invocation) -> Result<Output, RunError> {
        Ok(Output {
            success: true,
            status: "exit status: 0".into(),
            stdout: "sec:u:4096:1:X:1::::::scESC:\nfpr:::::::::FPR1:\n".into(),
            stderr: String::new(),
        })
    }
}

struct Fleet {
    pool: PgPool,
    store: CampaignStore,
    commands: CommandStore,
    mirrors: Mirrors,
    customer: Uuid,
    site: Uuid,
    /// `(id, peer_id)` of web-1 and web-2, debian.
    servers: Vec<(Uuid, String)>,
    _dir: tempfile::TempDir,
}

impl Fleet {
    async fn new(pool: &PgPool) -> Self {
        let one = |sql: &'static str| sqlx::query_scalar::<_, Uuid>(sql);
        let customer = one("INSERT INTO customers (name) VALUES ('acme') RETURNING id")
            .fetch_one(pool)
            .await
            .unwrap();
        let site = one("INSERT INTO sites (name, customer_id) VALUES ('paris', $1) RETURNING id")
            .bind(customer)
            .fetch_one(pool)
            .await
            .unwrap();
        let mut servers = Vec::new();
        for i in 1..=2 {
            let peer = format!("agent-{i}");
            let id = one("INSERT INTO servers (peer_id, site_id, hostname, os_family)
                 VALUES ($1, $2, $3, 'debian') RETURNING id")
            .bind(peer.clone())
            .bind(site)
            .bind(format!("web-{i}"))
            .fetch_one(pool)
            .await
            .unwrap();
            servers.push((id, peer));
        }
        let apt = one(
            "INSERT INTO mirrors (name, family, upstream_url, distribution, components)
             VALUES ('debian-12', 'apt', 'https://deb.debian.org/debian', 'bookworm', '{main}')
             RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let dnf = one(
            "INSERT INTO mirrors (name, family, upstream_url, distribution, components)
             VALUES ('rocky-9', 'dnf', 'https://dl.rockylinux.org/pub/rocky/9/BaseOS/x86_64/os', '', '{}')
             RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        for (mirror, name, state) in [
            (apt, "s1", "published"),
            (apt, "s2", "creating"),
            (dnf, "r1", "published"),
        ] {
            sqlx::query(
                "INSERT INTO mirror_snapshots (mirror_id, name, state, published_at)
                 VALUES ($1, $2, $3, CASE WHEN $3 = 'published' THEN now() END)",
            )
            .bind(mirror)
            .bind(name)
            .bind(state)
            .execute(pool)
            .await
            .unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        std::fs::create_dir_all(layout.keys()).unwrap();
        std::fs::write(
            layout.public_key(),
            "-----BEGIN PGP PUBLIC KEY BLOCK-----\n",
        )
        .unwrap();
        std::fs::create_dir_all(layout.served(Family::Apt).join("debian-12/s1")).unwrap();
        let mirrors = Mirrors::new(pool.clone(), Arc::new(FakeGpg), layout);
        Self {
            pool: pool.clone(),
            store: CampaignStore::new(pool.clone()),
            commands: CommandStore::new(pool.clone()),
            mirrors,
            customer,
            site,
            servers,
            _dir: dir,
        }
    }

    fn runner(&self) -> Runner {
        Runner::new(self.pool.clone()).with_sources(Some(URL.into()), self.mirrors.clone())
    }

    fn campaign(&self, waves: Vec<WaveSpec>) -> NewCampaign {
        NewCampaign {
            customer_id: self.customer,
            name: "october".into(),
            site_id: Some(self.site),
            server_ids: Vec::new(),
            waves,
            spec: UpdateSpec::default(),
            command_ttl: Duration::from_secs(3600),
            failure_threshold: FailureThreshold::default(),
            windows: Vec::new(),
            sources: Some(SourcesPin {
                mode: SourcesMode::Replace,
                mirrors: vec![PinnedMirror {
                    name: "debian-12".into(),
                    snapshot: "s1".into(),
                }],
            }),
            agent_version: None,
            created_by: None,
        }
    }

    async fn start(&self, new: &NewCampaign) -> Uuid {
        let id = self.store.create(new, USER).await.unwrap().id;
        self.store.act(id, Action::Start, USER).await.unwrap();
        id
    }

    /// `(kind, state)` of the campaign's commands for server `i`, oldest
    /// first.
    async fn history(&self, campaign: Uuid, i: usize) -> Vec<(String, State)> {
        sqlx::query_as(
            "SELECT kind, state FROM commands WHERE campaign_id = $1 AND server_id = $2
             ORDER BY issued_at, id",
        )
        .bind(campaign)
        .bind(self.servers[i].0)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }

    /// The last campaign command of server `i`.
    async fn last(&self, campaign: Uuid, i: usize) -> (Uuid, String, Vec<u8>) {
        sqlx::query_as(
            "SELECT id, kind, payload FROM commands WHERE campaign_id = $1 AND server_id = $2
             ORDER BY issued_at DESC, id DESC LIMIT 1",
        )
        .bind(campaign)
        .bind(self.servers[i].0)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    /// Server `i` receives the command `id` and reports `status`.
    async fn report(&self, i: usize, id: Uuid, status: CommandStatus) {
        assert!(self.commands.mark_sent(id).await.unwrap());
        let result = CommandResult {
            command_id: id.to_string(),
            exit_code: i32::from(status != CommandStatus::Succeeded),
            status: status as i32,
            ..CommandResult::default()
        };
        self.commands
            .record(&self.servers[i].1, &result)
            .await
            .unwrap();
    }

    /// Server `i` reports `status` for its last campaign command.
    async fn finish_last(&self, campaign: Uuid, i: usize, status: CommandStatus) -> String {
        let (id, kind, _) = self.last(campaign, i).await;
        self.report(i, id, status).await;
        kind
    }

    async fn state(&self, id: Uuid) -> CampaignState {
        self.store.get(id).await.unwrap().unwrap().state
    }

    async fn send(&self, uri: &str, public_url: Option<&str>, body: Value) -> (StatusCode, Value) {
        users::create_user_with(
            &self.pool,
            "admin",
            &Secret::new(PASSWORD),
            Role::Administrator,
            GrantScope::Customer(self.customer),
            "test",
        )
        .await
        .ok();
        let settings = ApiSettings {
            mirror_public_url: public_url.map(str::to_owned),
            ..ApiSettings::default()
        };
        let state = ApiState::new(self.pool.clone(), Registry::default(), settings)
            .with_mirrors(self.mirrors.clone());
        let login = json!({ "username": "admin", "password": PASSWORD }).to_string();
        let request = Request::post("/api/auth/login")
            .header(CSRF_HEADER, "1")
            .header("content-type", "application/json")
            .body(Body::from(login))
            .unwrap();
        let response = api::router(state.clone()).oneshot(request).await.unwrap();
        let cookie = response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let method = if body.is_null() { "GET" } else { "POST" };
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header(CSRF_HEADER, "1")
            .header("content-type", "application/json")
            .header("cookie", cookie)
            .body(if body.is_null() {
                Body::empty()
            } else {
                Body::from(body.to_string())
            })
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
}

fn queued(step: Step) -> Vec<String> {
    match step {
        Step::Queued(mut peers) => {
            peers.sort();
            peers
        }
        other => panic!("expected queued commands, got {other:?}"),
    }
}

fn kinds(history: &[(String, State)]) -> Vec<(&str, State)> {
    history.iter().map(|(k, s)| (k.as_str(), *s)).collect()
}

db_tests! {
    #[sqlx::test]
    async fn pinned_snapshots_are_validated_and_shown(pool: PgPool) {
        let f = Fleet::new(&pool).await;
        let body = |mirrors: Value| json!({
            "customer_id": f.customer, "name": "pinned", "site_id": f.site,
            "sources": {"mode": "replace", "mirrors": mirrors}
        });
        for (mirrors, error) in [
            (json!([{"name": "debian-12", "snapshot": "current"}]), "not `current`"),
            (json!([{"name": "debian-13", "snapshot": "s1"}]), "no mirror named `debian-13`"),
            (json!([{"name": "debian-12", "snapshot": "s2"}]), "no published snapshot `s2`"),
            (json!([{"name": "rocky-9", "snapshot": "r1"}]), "no mirror for the campaign's debian servers"),
            (json!([]), "1 to 32 mirrors"),
        ] {
            let (status, answer) = f.send("/api/campaigns", Some(URL), body(mirrors)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
            assert!(answer["error"].as_str().unwrap().contains(error), "{answer}");
        }
        let good = json!([{"name": "debian-12", "snapshot": "s1"}]);
        let (status, answer) = f.send("/api/campaigns", None, body(good.clone())).await;
        assert_eq!(status, StatusCode::CONFLICT, "{answer}");
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM campaigns")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(count, 0);

        // Mirrors of another family may be pinned beside.
        let both = json!([{"name": "debian-12", "snapshot": "s1"}, {"name": "rocky-9", "snapshot": "r1"}]);
        let (status, created) = f.send("/api/campaigns", Some(URL), body(both)).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let sources = json!({"mode": "replace", "mirrors": [
            {"name": "debian-12", "snapshot": "s1"}, {"name": "rocky-9", "snapshot": "r1"}]});
        assert_eq!(created["sources"], sources);
        let server = &created["waves"][0]["servers"][0];
        assert_eq!(server["sources_command_id"], Value::Null);
        let id = created["id"].as_str().unwrap();
        let (status, detail) = f.send(&format!("/api/campaigns/{id}"), Some(URL), Value::Null).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(detail["sources"], sources);
        let audited: Value = sqlx::query_scalar(
            "SELECT details FROM audit_log WHERE action = 'campaign.create'",
        )
        .fetch_one(&pool).await.unwrap();
        assert_eq!(audited["sources"], sources);
    }

    #[sqlx::test]
    async fn servers_are_pinned_updated_then_restored(pool: PgPool) {
        let f = Fleet::new(&pool).await;
        // web-2 was configured by hand before: that is what it gets back.
        let manual = f.commands.enqueue(&NewCommand {
            server_id: f.servers[1].0,
            kind: CONFIGURE_SOURCES.into(),
            payload: b"previous".to_vec(),
            ttl: Duration::from_secs(600),
            requested_by: None,
            campaign_id: None,
        }).await.unwrap();
        f.report(1, manual.id, CommandStatus::Succeeded).await;

        let runner = f.runner();
        let now = Utc::now();
        let mut new = f.campaign(Vec::new());
        new.failure_threshold.value = 1;
        let id = f.start(&new).await;

        // 1. Sources first, pointing at the pinned snapshot.
        assert_eq!(queued(runner.advance(id, now).await.unwrap()), ["agent-1", "agent-2"]);
        let (_, kind, payload) = f.last(id, 0).await;
        assert_eq!(kind, CONFIGURE_SOURCES);
        let spec = ConfigureSources::decode(payload.as_slice()).unwrap();
        assert_eq!(spec.mirror_base_url, URL);
        assert_eq!(spec.mode, Mode::Replace as i32);
        assert_eq!((spec.apt.len(), spec.apt[0].mirror.as_str(), spec.apt[0].snapshot.as_str()), (1, "debian-12", "s1"));
        assert_eq!(spec.keys[0].fingerprint, "FPR1");
        assert_eq!(runner.advance(id, now).await.unwrap(), Step::Idle);

        // 2. The update once the sources are in place.
        for i in 0..2 {
            assert_eq!(f.finish_last(id, i, CommandStatus::Succeeded).await, CONFIGURE_SOURCES);
        }
        assert_eq!(queued(runner.advance(id, now).await.unwrap()), ["agent-1", "agent-2"]);
        assert_eq!(f.last(id, 0).await.1, APPLY_UPDATES);

        // 3. Restored after it, whatever its result; not done until then.
        assert_eq!(f.finish_last(id, 0, CommandStatus::Succeeded).await, APPLY_UPDATES);
        assert_eq!(f.finish_last(id, 1, CommandStatus::Failed).await, APPLY_UPDATES);
        assert_eq!(runner.advance(id, now).await.unwrap(), Step::Idle);
        let (_, kind, _) = f.last(id, 0).await;
        assert_eq!(kind, REVERT_SOURCES);
        let (_, kind, payload) = f.last(id, 1).await;
        assert_eq!((kind.as_str(), payload.as_slice()), (CONFIGURE_SOURCES, b"previous".as_slice()));
        assert_eq!(f.state(id).await, CampaignState::Running);
        // Restores are not cancelled by a pause.
        f.store.act(id, Action::Pause, USER).await.unwrap();
        f.store.act(id, Action::Resume, USER).await.unwrap();
        for i in 0..2 {
            f.finish_last(id, i, CommandStatus::Succeeded).await;
        }
        assert_eq!(runner.advance(id, now).await.unwrap(), Step::Completed);
        assert_eq!(
            kinds(&f.history(id, 0).await),
            [(CONFIGURE_SOURCES, State::Succeeded), (APPLY_UPDATES, State::Succeeded),
             (REVERT_SOURCES, State::Succeeded)]
        );
        assert_eq!(
            kinds(&f.history(id, 1).await),
            [(CONFIGURE_SOURCES, State::Succeeded), (APPLY_UPDATES, State::Failed),
             (CONFIGURE_SOURCES, State::Succeeded)]
        );
        // Nothing more to restore, even when looked at again.
        runner.tick(now).await;
        assert_eq!(f.history(id, 0).await.len(), 3);
    }

    #[sqlx::test]
    async fn windows_servers_are_updated_without_sources(pool: PgPool) {
        let f = Fleet::new(&pool).await;
        sqlx::query("UPDATE servers SET os_family = 'windows' WHERE peer_id = 'agent-2'")
            .execute(&pool)
            .await
            .unwrap();
        // Named updates must fit both platforms; KB numbers do not fit debian.
        let mut named = f.campaign(Vec::new());
        named.spec.packages = vec!["KB5034441".into()];
        let error = f.store.create(&named, USER).await.unwrap_err().to_string();
        assert!(error.contains("which debian servers cannot take"), "{error}");
        named.spec.packages = vec!["openssl".into()];
        let error = f.store.create(&named, USER).await.unwrap_err().to_string();
        assert!(error.contains("which windows servers need"), "{error}");

        // The debian mirror is enough: the windows server needs none.
        let runner = f.runner();
        let now = Utc::now();
        let id = f.start(&f.campaign(Vec::new())).await;
        assert_eq!(queued(runner.advance(id, now).await.unwrap()), ["agent-1", "agent-2"]);
        assert_eq!(f.last(id, 0).await.1, CONFIGURE_SOURCES);
        assert_eq!(f.last(id, 1).await.1, APPLY_UPDATES);
        assert_eq!(f.finish_last(id, 1, CommandStatus::Succeeded).await, APPLY_UPDATES);
        assert_eq!(f.finish_last(id, 0, CommandStatus::Succeeded).await, CONFIGURE_SOURCES);
        assert_eq!(queued(runner.advance(id, now).await.unwrap()), ["agent-1"]);
        assert_eq!(f.finish_last(id, 0, CommandStatus::Succeeded).await, APPLY_UPDATES);
        assert_eq!(runner.advance(id, now).await.unwrap(), Step::Idle);
        assert_eq!(f.finish_last(id, 0, CommandStatus::Succeeded).await, REVERT_SOURCES);
        assert_eq!(runner.advance(id, now).await.unwrap(), Step::Completed);
        // Nothing to restore on the windows server.
        runner.tick(now).await;
        assert_eq!(kinds(&f.history(id, 1).await), [(APPLY_UPDATES, State::Succeeded)]);
    }

    #[sqlx::test]
    async fn pauses_and_failed_configurations_restore_too(pool: PgPool) {
        let f = Fleet::new(&pool).await;
        let runner = f.runner();
        let now = Utc::now();
        let id = f.start(&f.campaign(vec![WaveSpec::Size(1)])).await;
        queued(runner.advance(id, now).await.unwrap());
        f.finish_last(id, 0, CommandStatus::Succeeded).await;
        assert_eq!(queued(runner.advance(id, now).await.unwrap()), ["agent-1"]);

        // Paused before its update is sent: restored at once, and pinned
        // again on resume.
        f.store.act(id, Action::Pause, USER).await.unwrap();
        assert_eq!(runner.advance(id, now).await.unwrap(), Step::Idle);
        assert_eq!(f.last(id, 0).await.1, REVERT_SOURCES);
        f.store.act(id, Action::Resume, USER).await.unwrap();
        assert_eq!(runner.advance(id, now).await.unwrap(), Step::Idle);
        f.finish_last(id, 0, CommandStatus::Succeeded).await;
        assert_eq!(queued(runner.advance(id, now).await.unwrap()), ["agent-1"]);
        assert_eq!(
            kinds(&f.history(id, 0).await),
            [(CONFIGURE_SOURCES, State::Succeeded), (APPLY_UPDATES, State::Cancelled),
             (REVERT_SOURCES, State::Succeeded), (CONFIGURE_SOURCES, State::Queued)]
        );

        // The configuration fails on the host: no update, a restore, and
        // the wave fails once it is done.
        f.finish_last(id, 0, CommandStatus::Failed).await;
        assert!(queued(runner.advance(id, now).await.unwrap()).is_empty());
        assert_eq!(runner.advance(id, now).await.unwrap(), Step::Failed);
        assert_eq!(f.last(id, 0).await.1, REVERT_SOURCES);
        let error: Option<String> = sqlx::query_scalar(
            "SELECT error FROM campaign_targets WHERE campaign_id = $1 AND server_id = $2",
        )
        .bind(id)
        .bind(f.servers[0].0)
        .fetch_one(&pool).await.unwrap();
        assert!(error.unwrap().starts_with("configure_sources failed"));
        // Final, yet its restore still goes out once.
        runner.tick(now).await;
        assert_eq!(f.history(id, 0).await.len(), 5);
        assert!(f.history(id, 1).await.is_empty());
    }

    #[sqlx::test]
    async fn unfinished_campaigns_publish_their_snapshots(pool: PgPool) {
        let f = Fleet::new(&pool).await;
        let runner = f.runner();
        let dir = f.mirrors.layout().served(Family::Apt).join("debian-12");
        runner.tick(Utc::now()).await;
        assert!(read_pinned(&dir).is_empty());

        // A draft already pins: relays copy ahead of the window.
        let id = f.store.create(&f.campaign(Vec::new()), USER).await.unwrap().id;
        write_pins(&pool, f.mirrors.layout()).await.unwrap();
        assert_eq!(read_pinned(&dir), ["s1"]);
        // rocky-9 has no served directory yet: skipped.
        assert!(!f.mirrors.layout().served(Family::Dnf).join("rocky-9").exists());

        f.store.act(id, Action::Stop, USER).await.unwrap();
        runner.tick(Utc::now()).await;
        assert!(read_pinned(&dir).is_empty());
    }
}
