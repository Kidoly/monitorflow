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
use monitorflow_server::commands::PackageStore;
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
        let peer: String = sqlx::query_scalar("SELECT peer_id FROM servers WHERE id = $1")
            .bind(server)
            .fetch_one(&self.pool)
            .await
            .unwrap();
        let report = PackageReport {
            collected_at_unix_ms: Utc::now().timestamp_millis(),
            major_upgrade: Some(Box::new(MajorUpgradeReport {
                release: "debian 12".into(),
                codename: "bookworm".into(),
                target: "debian 13".into(),
                target_codename: "trixie".into(),
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

db_tests! {
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
