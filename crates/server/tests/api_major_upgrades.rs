//! Major upgrades through the API and the campaign runner (ADR-0182): the
//! checks of each server, what a campaign refuses, one server at a time,
//! and a server done once back on the new release. Ignored without
//! `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use monitorflow_common::proto::v1::{
    MajorUpgrade, MajorUpgradeProgress, MajorUpgradeReport, PackageReport, UpgradeCheck,
};
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::campaign::{Runner, Step};
use monitorflow_server::commands::{CommandStore, NewCommand, PackageStore};
use monitorflow_server::grpc::Registry;
use prost::Message;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";

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

struct Harness {
    pool: PgPool,
    customer: Uuid,
    site: Uuid,
    /// web-1 and web-2.
    servers: Vec<Uuid>,
}

impl Harness {
    async fn new(pool: &PgPool) -> Self {
        let customer: Uuid =
            sqlx::query_scalar("INSERT INTO customers (name) VALUES ('acme') RETURNING id")
                .fetch_one(pool)
                .await
                .unwrap();
        let site: Uuid = sqlx::query_scalar(
            "INSERT INTO sites (name, customer_id) VALUES ('paris', $1) RETURNING id",
        )
        .bind(customer)
        .fetch_one(pool)
        .await
        .unwrap();
        let mut servers = Vec::new();
        for n in 1..=2 {
            servers.push(
                sqlx::query_scalar(
                    "INSERT INTO servers (peer_id, site_id, hostname, os_family)
                     VALUES ($1, $2, $3, 'debian') RETURNING id",
                )
                .bind(format!("agent-{n}"))
                .bind(site)
                .bind(format!("web-{n}"))
                .fetch_one(pool)
                .await
                .unwrap(),
            );
        }
        Self {
            pool: pool.clone(),
            customer,
            site,
            servers,
        }
    }

    async fn send(
        &self,
        method: &str,
        uri: &str,
        cookie: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let state = ApiState::new(
            self.pool.clone(),
            Registry::default(),
            ApiSettings::default(),
        )
        .with_campaigns(Runner::new(self.pool.clone()));
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
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 22)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn login(&self, name: &str, role: Role, scope: GrantScope) -> String {
        users::create_user_with(
            &self.pool,
            name,
            &Secret::new(PASSWORD),
            role,
            scope,
            "test",
        )
        .await
        .unwrap();
        let state = ApiState::new(
            self.pool.clone(),
            Registry::default(),
            ApiSettings::default(),
        );
        let body = json!({ "username": name, "password": PASSWORD }).to_string();
        let request = Request::post("/api/auth/login")
            .header(CSRF_HEADER, "1")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let response = api::router(state).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned()
    }

    /// A major upgrade of the site's Debian 12 servers.
    fn major(&self, accept: bool) -> Value {
        json!({
            "customer_id": self.customer,
            "name": "Trixie",
            "site_id": self.site,
            "major_upgrade": {"from": "debian 12", "to": "debian 13", "accept_limited_rollback": accept}
        })
    }

    /// web-1 reports what a Debian 12 agent of this version reports, and
    /// accepts major upgrades.
    async fn report(&self, server: Uuid, progress: Option<MajorUpgradeProgress>) {
        self.report_of(server, "debian 12", "debian 13", progress)
            .await;
    }

    /// `server` reports `release`, its `target` and `progress`.
    async fn report_of(
        &self,
        server: Uuid,
        release: &str,
        target: &str,
        progress: Option<MajorUpgradeProgress>,
    ) {
        let peer: String = sqlx::query_scalar("SELECT peer_id FROM servers WHERE id = $1")
            .bind(server)
            .fetch_one(&self.pool)
            .await
            .unwrap();
        let report = PackageReport {
            collected_at_unix_ms: Utc::now().timestamp_millis(),
            major_upgrade: Some(Box::new(MajorUpgradeReport {
                release: release.into(),
                codename: String::new(),
                target: target.into(),
                target_codename: String::new(),
                checks: vec![UpgradeCheck {
                    name: "held_packages".into(),
                    state: "ok".into(),
                    detail: "no package is held".into(),
                    items: vec![],
                }],
                progress,
            })),
            ..PackageReport::default()
        };
        assert!(PackageStore::new(self.pool.clone())
            .replace(&peer, &report)
            .await
            .unwrap());
    }
}

impl Harness {
    /// `server` runs `release` with `kinds` accepted, as `Hello` says.
    async fn runs(&self, server: Uuid, family: &str, release: &str, kinds: &[&str]) {
        let kinds: serde_json::Map<String, Value> = kinds
            .iter()
            .map(|k| ((*k).to_owned(), json!("accepted")))
            .collect();
        sqlx::query(
            "UPDATE servers SET os_family = $2, os_version = $3, package_updates = true,
                 command_kinds = $4
             WHERE id = $1",
        )
        .bind(server)
        .bind(family)
        .bind(release)
        .bind(Value::Object(kinds))
        .execute(&self.pool)
        .await
        .unwrap();
    }

    /// `server` was pointed at the mirrors from its page: a
    /// `configure_sources` that succeeded outside campaigns.
    async fn on_mirrors(&self, server: Uuid, payload: &[u8]) -> Uuid {
        let new = NewCommand {
            server_id: server,
            kind: "configure_sources".into(),
            payload: payload.to_vec(),
            ttl: std::time::Duration::from_secs(3600),
            requested_by: None,
            campaign_id: None,
        };
        let row = CommandStore::enqueue_in(&self.pool, &new).await.unwrap();
        finish(&self.pool, row.id, "succeeded").await;
        row.id
    }

    /// The campaign's commands, oldest first: server and kind.
    async fn commands(&self, campaign: Uuid) -> Vec<(Uuid, Uuid, String)> {
        sqlx::query_as(
            "SELECT id, server_id, kind FROM commands WHERE campaign_id = $1 ORDER BY issued_at, id",
        )
        .bind(campaign)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }

    /// An Ubuntu 22.04 to 24.04 campaign of the site, started.
    async fn ubuntu_campaign(&self, admin: &str) -> Uuid {
        let body = json!({
            "customer_id": self.customer,
            "name": "Noble",
            "site_id": self.site,
            "major_upgrade": {"from": "ubuntu 22.04", "to": "ubuntu 24.04", "accept_limited_rollback": true}
        });
        let (status, created) = self.send("POST", "/api/campaigns", admin, Some(body)).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let id = created["id"].as_str().unwrap().to_owned();
        let (status, _) = self
            .send("POST", &format!("/api/campaigns/{id}/start"), admin, None)
            .await;
        assert_eq!(status, StatusCode::OK);
        id.parse().unwrap()
    }
}

async fn finish(pool: &PgPool, command: Uuid, state: &str) {
    sqlx::query(
        "UPDATE commands SET state = $2, sent_at = now(), started_at = now(), finished_at = now(),
             exit_code = CASE WHEN $2 = 'succeeded' THEN 0 ELSE 1 END
         WHERE id = $1",
    )
    .bind(command)
    .bind(state)
    .execute(pool)
    .await
    .unwrap();
}

db_tests! {
    #[sqlx::test]
    async fn ubuntu_on_the_mirrors_gets_its_own_sources_back_first(pool: PgPool) {
        let h = Harness::new(&pool).await;
        for server in &h.servers {
            h.runs(*server, "debian", "ubuntu 22.04", &["major_upgrade"]).await;
        }
        h.on_mirrors(h.servers[0], b"mirror spec").await;
        let admin = h.login("admin", Role::Administrator, GrantScope::Customer(h.customer)).await;

        // The page says what happens to the sources, without blocking.
        let (_, list) = h.send("GET", &format!("/api/major-upgrades?server_id={}", h.servers[0]), &admin, None).await;
        let mirror = &list["servers"][0]["checks"][2];
        assert_eq!(mirror["name"], "mirror", "{list}");
        assert_eq!(mirror["state"], "warning", "{list}");
        assert!(mirror["detail"].as_str().unwrap().contains("puts its own sources back before the upgrade"), "{list}");

        let campaign = h.ubuntu_campaign(&admin).await;
        let runner = Runner::new(pool.clone());
        let now = Utc::now();
        runner.advance(campaign, now).await.unwrap();
        let commands = h.commands(campaign).await;
        assert_eq!(commands.len(), 1, "one server at a time: {commands:?}");
        assert_eq!((commands[0].1, commands[0].2.as_str()), (h.servers[0], "revert_sources"));
        // Its sources are the campaign's now.
        let (_, sources) = h.send("GET", &format!("/api/servers/{}/sources", h.servers[0]), &admin, None).await;
        assert_eq!(sources["campaign"], true, "{sources}");

        // Back on its own sources: the upgrade (no snapshot is possible).
        finish(&pool, commands[0].0, "succeeded").await;
        runner.advance(campaign, now).await.unwrap();
        let commands = h.commands(campaign).await;
        assert_eq!(commands.len(), 2, "{commands:?}");
        assert_eq!(commands[1].2, "major_upgrade");
        let upgrade = commands[1].0;

        // Done on Ubuntu 24.04: no mirror carries it here, so it keeps its
        // own sources, and the audit log says why.
        finish(&pool, upgrade, "succeeded").await;
        let progress = MajorUpgradeProgress {
            command_id: upgrade.to_string(),
            from: "ubuntu 22.04".into(),
            to: "ubuntu 24.04".into(),
            phase: "done".into(),
            ..MajorUpgradeProgress::default()
        };
        h.report_of(h.servers[0], "ubuntu 22.04", "ubuntu 24.04", Some(progress)).await;
        sqlx::query("UPDATE servers SET os_version = 'ubuntu 24.04' WHERE id = $1")
            .bind(h.servers[0])
            .execute(&pool)
            .await
            .unwrap();
        runner.advance(campaign, now).await.unwrap();
        runner.advance(campaign, now + chrono::TimeDelta::minutes(6)).await.unwrap();
        let (restore, revert): (Option<Uuid>, Option<Uuid>) = sqlx::query_as(
            "SELECT restore_command_id, sources_command_id FROM campaign_targets
             WHERE campaign_id = $1 AND server_id = $2",
        )
        .bind(campaign)
        .bind(h.servers[0])
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(restore, revert, "kept on its own sources");
        let kept: Value = sqlx::query_scalar(
            "SELECT details FROM audit_log WHERE action = 'campaign.sources_kept_own'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(kept["release"], "ubuntu 24.04", "{kept}");
        // web-2, on its own sources, goes straight to its upgrade.
        runner.advance(campaign, now + chrono::TimeDelta::minutes(7)).await.unwrap();
        let commands = h.commands(campaign).await;
        assert_eq!((commands[2].1, commands[2].2.as_str()), (h.servers[1], "major_upgrade"), "{commands:?}");
    }

    #[sqlx::test]
    async fn a_failed_upgrade_puts_the_previous_mirror_sources_back(pool: PgPool) {
        let h = Harness::new(&pool).await;
        h.runs(h.servers[0], "debian", "ubuntu 22.04", &["major_upgrade"]).await;
        sqlx::query("UPDATE servers SET os_version = 'ubuntu 24.04' WHERE id = $1")
            .bind(h.servers[1])
            .execute(&pool)
            .await
            .unwrap();
        h.on_mirrors(h.servers[0], b"mirror spec of 22.04").await;
        let admin = h.login("admin", Role::Administrator, GrantScope::Customer(h.customer)).await;
        let body = json!({
            "customer_id": h.customer,
            "name": "Noble",
            "server_ids": [h.servers[0]],
            "major_upgrade": {"from": "ubuntu 22.04", "to": "ubuntu 24.04", "accept_limited_rollback": true}
        });
        let (status, created) = h.send("POST", "/api/campaigns", &admin, Some(body)).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let campaign: Uuid = created["id"].as_str().unwrap().parse().unwrap();
        let (status, _) = h.send("POST", &format!("/api/campaigns/{campaign}/start"), &admin, None).await;
        assert_eq!(status, StatusCode::OK);
        let runner = Runner::new(pool.clone());
        let now = Utc::now();
        runner.advance(campaign, now).await.unwrap();
        finish(&pool, h.commands(campaign).await[0].0, "succeeded").await;
        runner.advance(campaign, now).await.unwrap();
        let upgrade = h.commands(campaign).await[1].0;
        // do-release-upgrade refused: still on 22.04.
        finish(&pool, upgrade, "failed").await;
        runner.advance(campaign, now).await.unwrap();
        let commands = h.commands(campaign).await;
        assert_eq!(commands.len(), 3, "{commands:?}");
        assert_eq!(commands[2].2, "configure_sources");
        let payload: Vec<u8> = sqlx::query_scalar("SELECT payload FROM commands WHERE id = $1")
            .bind(commands[2].0)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(payload, b"mirror spec of 22.04", "the sources it had before");
        // The campaign ends once they are back.
        finish(&pool, commands[2].0, "succeeded").await;
        runner.advance(campaign, now).await.unwrap();
        let state: String = sqlx::query_scalar("SELECT state FROM campaigns WHERE id = $1")
            .bind(campaign)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(state, "failed");
    }

    #[sqlx::test]
    async fn leapp_preupgrade_runs_alone_from_the_servers_page(pool: PgPool) {
        let h = Harness::new(&pool).await;
        h.runs(h.servers[0], "rhel", "rocky 8.10", &["major_upgrade", "leapp_preupgrade"]).await;
        h.runs(h.servers[1], "debian", "debian 12", &["major_upgrade"]).await;
        let admin = h.login("admin", Role::Administrator, GrantScope::Customer(h.customer)).await;
        let site_admin = h.login("site-admin", Role::Administrator, GrantScope::Site(h.site)).await;

        let (_, list) = h.send("GET", "/api/major-upgrades", &admin, None).await;
        let servers = list["servers"].as_array().unwrap();
        assert_eq!(servers[0]["can_preupgrade"], true, "{list}");
        assert_eq!(servers[1]["can_preupgrade"], false, "Debian has no leapp");

        let uri = |server: Uuid| format!("/api/servers/{server}/major-upgrade/preupgrade");
        // Like a campaign: never a site administrator.
        let (status, _) = h.send("POST", &uri(h.servers[0]), &site_admin, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, body) = h.send("POST", &uri(h.servers[1]), &admin, None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body["error"].as_str().unwrap().contains("RHEL family"), "{body}");

        let (status, command) = h.send("POST", &uri(h.servers[0]), &admin, None).await;
        assert_eq!(status, StatusCode::CREATED, "{command}");
        assert_eq!(command["kind"], "leapp_preupgrade", "{command}");
        assert_eq!(command["parameters"], json!({"from": "rocky 8", "to": "rocky 9"}), "{command}");
        let payload: Vec<u8> = sqlx::query_scalar("SELECT payload FROM commands WHERE server_id = $1")
            .bind(h.servers[0])
            .fetch_one(&pool)
            .await
            .unwrap();
        let payload = MajorUpgrade::decode(payload.as_slice()).unwrap();
        assert_eq!((payload.from.as_str(), payload.to.as_str()), ("rocky 8", "rocky 9"));

        // Its report comes back as the server's progress.
        let progress = MajorUpgradeProgress {
            command_id: command["id"].as_str().unwrap().into(),
            from: "rocky 8".into(),
            to: "rocky 9".into(),
            phase: "failed".into(),
            error: "`leapp preupgrade` found 1 inhibitors: Old kernel".into(),
            report: "Risk Factor: high (inhibitor)\nTitle: Old kernel\n".into(),
            preupgrade_only: true,
            ..MajorUpgradeProgress::default()
        };
        h.report_of(h.servers[0], "rocky 8.10", "rocky 9", Some(progress)).await;
        let (_, list) = h.send("GET", &format!("/api/major-upgrades?server_id={}", h.servers[0]), &admin, None).await;
        let progress = &list["servers"][0]["progress"];
        assert_eq!(progress["preupgrade_only"], true, "{list}");
        assert!(progress["report"].as_str().unwrap().contains("Old kernel"));
        let audited: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit_log WHERE action = 'command.create' AND details ->> 'kind' = 'leapp_preupgrade'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(audited, 1);
    }

    #[sqlx::test]
    async fn a_major_upgrade_runs_as_a_campaign_one_server_at_a_time(pool: PgPool) {
        let h = Harness::new(&pool).await;
        sqlx::query(
            "UPDATE servers SET os_version = 'debian 12', package_updates = true,
                 command_kinds = '{\"major_upgrade\": \"accepted\"}'
             WHERE id = ANY($1)",
        )
        .bind(&h.servers)
        .execute(&pool)
        .await
        .unwrap();
        let admin = h.login("admin", Role::Administrator, GrantScope::Customer(h.customer)).await;

        // The checks, before anything: web-1's agent reported, web-2's did not.
        h.report(h.servers[0], None).await;
        let (status, list) = h.send("GET", "/api/major-upgrades", &admin, None).await;
        assert_eq!(status, StatusCode::OK, "{list}");
        let servers = list["servers"].as_array().unwrap();
        assert_eq!(servers.len(), 2, "{list}");
        assert_eq!(servers[0]["hostname"], "web-1");
        assert_eq!(servers[0]["target_label"], "Debian 13 (trixie)");
        assert_eq!(servers[0]["ready"], true, "{list}");
        assert_eq!(servers[0]["can_upgrade"], true);
        let names: Vec<&str> = servers[0]["checks"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["allowed", "agent_version", "mirror", "held_packages"]);
        assert_eq!(servers[1]["ready"], false);
        assert_eq!(servers[1]["checks"][1]["state"], "failed");
        assert_eq!(servers[0]["rollback"]["available"], false);

        // No snapshot is possible: refused unless accepted.
        let (status, body) = h.send("POST", "/api/campaigns", &admin, Some(h.major(false))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body["error"].as_str().unwrap().contains("rollback is limited on web-1"), "{body}");
        // Never two releases at once.
        let mut two = h.major(true);
        two["major_upgrade"]["from"] = json!("debian 11");
        let (status, _) = h.send("POST", "/api/campaigns", &admin, Some(two)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // A named server on another release.
        sqlx::query("UPDATE servers SET os_version = 'debian 13' WHERE id = $1")
            .bind(h.servers[1])
            .execute(&pool)
            .await
            .unwrap();
        let mut named = h.major(true);
        named["server_ids"] = json!([h.servers[1]]);
        let (status, body) = h.send("POST", "/api/campaigns", &admin, Some(named)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "every server of the upgrade must run debian 12: web-2 runs debian 13");
        sqlx::query("UPDATE servers SET os_version = 'debian 12' WHERE id = $1")
            .bind(h.servers[1])
            .execute(&pool)
            .await
            .unwrap();

        let (status, created) = h.send("POST", "/api/campaigns", &admin, Some(h.major(true))).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["major_upgrade"], json!({"from": "debian 12", "to": "debian 13", "accept_limited_rollback": true, "parallel": 1}));
        let id = created["id"].as_str().unwrap().to_owned();
        let (status, _) = h.send("POST", &format!("/api/campaigns/{id}/start"), &admin, None).await;
        assert_eq!(status, StatusCode::OK);

        // One server at a time: web-1 gets its command, web-2 waits.
        let campaign: Uuid = id.parse().unwrap();
        let runner = Runner::new(pool.clone());
        let now = Utc::now();
        assert!(matches!(runner.advance(campaign, now).await.unwrap(), Step::Queued(_)));
        let commands: Vec<(Uuid, String, Vec<u8>)> = sqlx::query_as(
            "SELECT server_id, kind, payload FROM commands WHERE campaign_id = $1",
        )
        .bind(campaign)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(commands.len(), 1, "{commands:?}");
        assert_eq!(commands[0].0, h.servers[0]);
        assert_eq!(commands[0].1, "major_upgrade");
        let payload = MajorUpgrade::decode(commands[0].2.as_slice()).unwrap();
        assert_eq!((payload.from.as_str(), payload.to.as_str()), ("debian 12", "debian 13"));
        let audited: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit_log WHERE action = 'campaign.queue' AND details ->> 'impact' = 'high_impact'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(audited, 1);

        // The command succeeds: the upgrade started, web-1 is upgrading and
        // web-2 still waits.
        let command: Uuid = sqlx::query_scalar("SELECT id FROM commands WHERE campaign_id = $1")
            .bind(campaign)
            .fetch_one(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE commands SET state = 'succeeded', sent_at = now(), started_at = now(), finished_at = now(), exit_code = 0 WHERE id = $1")
            .bind(command)
            .execute(&pool)
            .await
            .unwrap();
        runner.advance(campaign, now).await.unwrap();
        let (_, detail) = h.send("GET", &format!("/api/campaigns/{id}"), &admin, None).await;
        assert_eq!(detail["waves"][0]["servers"][0]["status"], "upgrading", "{detail}");
        assert_eq!(detail["progress"]["in_progress"], 1, "{detail}");
        let queued: i64 = sqlx::query_scalar("SELECT count(*) FROM commands WHERE campaign_id = $1")
            .bind(campaign)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(queued, 1, "web-2 waits for web-1");

        // Back on Debian 13 after the reboot, with its checks as before.
        h.report(h.servers[0], Some(MajorUpgradeProgress {
            command_id: command.to_string(),
            from: "debian 12".into(),
            to: "debian 13".into(),
            phase: "rebooting".into(),
            ..MajorUpgradeProgress::default()
        })).await;
        let (_, detail) = h.send("GET", &format!("/api/campaigns/{id}"), &admin, None).await;
        assert_eq!(detail["waves"][0]["servers"][0]["upgrade_phase"], "rebooting");
        h.report(h.servers[0], Some(MajorUpgradeProgress {
            command_id: command.to_string(),
            from: "debian 12".into(),
            to: "debian 13".into(),
            phase: "done".into(),
            ..MajorUpgradeProgress::default()
        })).await;
        sqlx::query("UPDATE servers SET os_version = 'debian 13' WHERE id = $1")
            .bind(h.servers[0])
            .execute(&pool)
            .await
            .unwrap();
        runner.advance(campaign, now).await.unwrap();
        runner.advance(campaign, now + chrono::TimeDelta::minutes(6)).await.unwrap();
        let (_, detail) = h.send("GET", &format!("/api/campaigns/{id}"), &admin, None).await;
        assert_eq!(detail["waves"][0]["servers"][0]["status"], "succeeded", "{detail}");
        let done: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'campaign.upgrade_done'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(done, 1);
        // The next batch: web-2.
        runner.advance(campaign, now + chrono::TimeDelta::minutes(7)).await.unwrap();
        let next: Option<Uuid> = sqlx::query_scalar(
            "SELECT server_id FROM commands WHERE campaign_id = $1 AND id <> $2",
        )
        .bind(campaign)
        .bind(command)
        .fetch_optional(&pool)
        .await
        .unwrap();
        assert_eq!(next, Some(h.servers[1]));
    }
}
