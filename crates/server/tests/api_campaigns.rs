//! Campaigns through the API (ADR-0033): the documented JSON, who may
//! read, create and act, validation, and progress once the runner queued
//! commands. Ignored without `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::campaign::{Runner, Step};
use monitorflow_server::grpc::Registry;
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

    fn body(&self) -> Value {
        json!({
            "customer_id": self.customer,
            "name": "October updates",
            "site_id": self.site,
            "waves": [{"size": 1}],
            "security_only": true,
            "failure_threshold": {"value": 10, "unit": "percent"},
            "windows": [{"kind": "weekly", "weekdays": [6, 7], "start": "22:00", "end": "04:00",
                         "timezone": "Europe/Paris"}]
        })
    }
}

fn keys(value: &Value) -> Vec<&str> {
    let mut keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    keys
}

db_tests! {
    #[sqlx::test]
    async fn campaigns_have_the_documented_shape(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let admin = h.login("admin", Role::Administrator, GrantScope::Customer(h.customer)).await;
        let (status, created) = h.send("POST", "/api/campaigns", &admin, Some(h.body())).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(
            keys(&created),
            [
                "agent_version", "command_ttl_s", "created_at", "created_by", "current_wave", "customer_id",
                "customer_name", "error", "failure_threshold", "finished_at", "id", "name", "previous_runs",
                "progress", "server_ids", "site_id", "sources", "spec", "started_at", "state",
                "wave_count", "wave_specs", "waves", "windows"
            ]
        );
        assert_eq!(created["state"], "draft");
        assert_eq!(created["customer_name"], "acme");
        assert_eq!(created["spec"], json!({"packages": [], "security_only": true, "allow_reboot": false, "include_os": true, "third_party": []}));
        assert_eq!(created["command_ttl_s"], 3600);
        assert_eq!(created["failure_threshold"], json!({"value": 10, "unit": "percent"}));
        assert_eq!(created["windows"], h.body()["windows"]);
        assert_eq!(created["sources"], Value::Null);
        assert_eq!(created["server_ids"], json!([]));
        assert_eq!(created["wave_specs"], json!([{"size": 1}]));
        assert_eq!(created["wave_count"], 2);
        assert_eq!(created["current_wave"], Value::Null);
        assert_eq!(
            created["progress"],
            json!({"total": 2, "pending": 2, "in_progress": 0, "succeeded": 0, "failed": 0, "cancelled": 0})
        );
        let wave = &created["waves"][0];
        assert_eq!(keys(wave), ["finished_at", "index", "progress", "servers", "started_at", "state"]);
        assert_eq!((wave["index"].clone(), wave["state"].clone()), (json!(0), json!("pending")));
        let server = &wave["servers"][0];
        assert_eq!(
            keys(server),
            ["command_id", "error", "exit_code", "finished_at", "hostname", "peer_id",
             "restore_command_id", "server_id", "site_id", "snapshot_command_id",
             "sources_command_id", "status"]
        );
        assert_eq!(server["hostname"], "web-1");
        assert_eq!(server["status"], "pending");

        // Started, then a step of the runner: the first wave is queued.
        let id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
        let (status, started) = h.send("POST", &format!("/api/campaigns/{id}/start"), &admin, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(started["state"], "scheduled");
        sqlx::query("DELETE FROM campaign_windows WHERE campaign_id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        Runner::new(pool.clone()).advance(id, Utc::now()).await.unwrap();
        let (status, detail) = h.send("GET", &format!("/api/campaigns/{id}"), &admin, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(detail["state"], "running");
        assert_eq!(detail["current_wave"], 0);
        assert_eq!(detail["waves"][0]["state"], "running");
        assert_eq!(detail["waves"][0]["servers"][0]["status"], "queued");
        assert!(detail["waves"][0]["servers"][0]["command_id"].is_string());
        assert_eq!(detail["progress"]["in_progress"], 1);
        assert_eq!(detail["progress"]["pending"], 1);

        let (status, list) = h.send("GET", "/api/campaigns?state=running", &admin, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list["campaigns"].as_array().unwrap().len(), 1);
        assert_eq!(list["campaigns"][0]["progress"], detail["progress"]);
        assert!(list["campaigns"][0].get("waves").is_none());
        assert_eq!(list["next_cursor"], Value::Null);
        let (_, list) = h.send("GET", "/api/campaigns?state=draft", &admin, None).await;
        assert_eq!(list["campaigns"], json!([]));
    }

    #[sqlx::test]
    async fn actions_follow_the_state_machine(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let admin = h.login("admin", Role::Administrator, GrantScope::Global).await;
        let (_, created) = h.send("POST", "/api/campaigns", &admin, Some(h.body())).await;
        let id = created["id"].as_str().unwrap();
        let action = |name: &str| format!("/api/campaigns/{id}/{name}");
        let (status, body) = h.send("POST", &action("pause"), &admin, None).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "cannot pause a campaign that is draft");
        for (name, state) in [("start", "scheduled"), ("pause", "paused"), ("resume", "scheduled"), ("stop", "stopped")] {
            let (status, body) = h.send("POST", &action(name), &admin, None).await;
            assert_eq!((status, body["state"].as_str()), (StatusCode::OK, Some(state)), "{name}");
        }
        assert_eq!(h.send("POST", &action("start"), &admin, None).await.0, StatusCode::CONFLICT);
        let audited: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit_log WHERE target_id = $1 AND action LIKE 'campaign.%'",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(audited, 5);
        let missing = format!("/api/campaigns/{}/start", Uuid::new_v4());
        assert_eq!(h.send("POST", &missing, &admin, None).await.0, StatusCode::NOT_FOUND);
        assert_eq!(h.send("GET", "/api/campaigns/nope", &admin, None).await.0, StatusCode::NOT_FOUND);
    }

    #[sqlx::test]
    async fn roles_decide_who_reads_and_acts(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let admin = h.login("admin", Role::Administrator, GrantScope::Customer(h.customer)).await;
        let (_, created) = h.send("POST", "/api/campaigns", &admin, Some(h.body())).await;
        let detail = format!("/api/campaigns/{}", created["id"].as_str().unwrap());
        let start = format!("{detail}/start");

        // Customer-wide roles below administrator read, but do not act.
        let viewer = h.login("viewer", Role::Viewer, GrantScope::Customer(h.customer)).await;
        let operator = h.login("operator", Role::Operator, GrantScope::Global).await;
        for cookie in [&viewer, &operator] {
            assert_eq!(h.send("GET", &detail, cookie, None).await.0, StatusCode::OK);
            let (_, list) = h.send("GET", "/api/campaigns", cookie, None).await;
            assert_eq!(list["campaigns"].as_array().unwrap().len(), 1);
            assert_eq!(h.send("POST", &start, cookie, None).await.0, StatusCode::FORBIDDEN);
            let (status, _) = h.send("POST", "/api/campaigns", cookie, Some(h.body())).await;
            assert_eq!(status, StatusCode::FORBIDDEN);
        }
        // A site grant, even administrator, does not see campaigns.
        let site_admin = h.login("site-admin", Role::Administrator, GrantScope::Site(h.site)).await;
        let (_, list) = h.send("GET", "/api/campaigns", &site_admin, None).await;
        assert_eq!(list["campaigns"], json!([]));
        assert_eq!(h.send("GET", &detail, &site_admin, None).await.0, StatusCode::NOT_FOUND);
        assert_eq!(h.send("POST", &start, &site_admin, None).await.0, StatusCode::NOT_FOUND);
        let (status, _) = h.send("POST", "/api/campaigns", &site_admin, Some(h.body())).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        // A global administrator acts on any customer's campaign.
        let global = h.login("global", Role::Administrator, GrantScope::Global).await;
        assert_eq!(h.send("POST", &start, &global, None).await.0, StatusCode::OK);
        assert_eq!(h.send("GET", "/api/campaigns", "", None).await.0, StatusCode::UNAUTHORIZED);
    }

    #[sqlx::test]
    async fn invalid_requests_are_refused(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let admin = h.login("admin", Role::Administrator, GrantScope::Customer(h.customer)).await;
        let with = |patch: Value| {
            let mut body = h.body();
            body.as_object_mut().unwrap().extend(patch.as_object().unwrap().clone());
            body
        };
        for patch in [
            json!({"name": ""}),
            json!({"command_ttl_s": 10}),
            json!({"site_id": null}),
            json!({"server_ids": [Uuid::new_v4()]}),
            json!({"packages": ["-o"]}),
            json!({"waves": [{"size": 2}, {"size": 1}]}),
            json!({"waves": [{"percent": 0}]}),
            json!({"failure_threshold": {"value": 101, "unit": "percent"}}),
            json!({"windows": [{"kind": "weekly", "weekdays": [8], "start": "22:00", "end": "04:00", "timezone": "UTC"}]}),
            json!({"windows": [{"kind": "weekly", "weekdays": [1], "start": "22:00", "end": "04:00", "timezone": "Mars/Olympus"}]}),
            json!({"windows": [{"kind": "weekly", "weekdays": [1], "start": "9:00", "end": "10:00", "timezone": "UTC"}]}),
            json!({"windows": [{"kind": "monthly", "month_day": 1, "month_week": 1, "start": "22:00", "end": "04:00", "timezone": "UTC"}]}),
            json!({"windows": [{"kind": "monthly", "month_day": 1, "start": "22:00", "end": "04:00", "timezone": "Mars/Olympus"}]}),
            json!({"unknown": true}),
        ] {
            let (status, body) = h.send("POST", "/api/campaigns", &admin, Some(with(patch.clone()))).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{patch}: {body}");
            assert!(body["error"].is_string());
        }
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM campaigns")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
        // Explicit servers alone are enough.
        let body = with(json!({"site_id": null, "server_ids": [h.servers[1]], "waves": []}));
        let (status, created) = h.send("POST", "/api/campaigns", &admin, Some(body)).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["waves"][0]["servers"][0]["server_id"], json!(h.servers[1]));
    }

    #[sqlx::test]
    async fn campaigns_are_edited_until_queued_and_deleted_once_over(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let admin = h.login("admin", Role::Administrator, GrantScope::Customer(h.customer)).await;
        let (_, created) = h.send("POST", "/api/campaigns", &admin, Some(h.body())).await;
        let id = created["id"].as_str().unwrap().to_owned();
        let detail = format!("/api/campaigns/{id}");

        // The whole request again: one batch of the second server.
        let mut edited = h.body();
        edited["name"] = json!("Edited");
        edited["site_id"] = Value::Null;
        edited["server_ids"] = json!([h.servers[1]]);
        edited["waves"] = json!([]);
        edited["windows"] = json!([]);
        let (status, body) = h.send("PUT", &detail, &admin, Some(edited.clone())).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!((body["name"].as_str(), body["state"].as_str()), (Some("Edited"), Some("draft")));
        assert_eq!(body["server_ids"], json!([h.servers[1]]));
        assert_eq!(body["wave_specs"], json!([]));
        assert_eq!(body["waves"].as_array().unwrap().len(), 1);
        assert_eq!(body["waves"][0]["servers"][0]["server_id"], json!(h.servers[1]));
        assert_eq!(body["windows"], json!([]));
        assert_eq!(body["created_at"], created["created_at"]);
        let mut bad = edited.clone();
        bad["command_ttl_s"] = json!(10);
        assert_eq!(h.send("PUT", &detail, &admin, Some(bad)).await.0, StatusCode::BAD_REQUEST);
        let viewer = h.login("viewer", Role::Viewer, GrantScope::Customer(h.customer)).await;
        assert_eq!(h.send("PUT", &detail, &viewer, Some(edited.clone())).await.0, StatusCode::FORBIDDEN);
        assert_eq!(h.send("DELETE", &detail, &viewer, None).await.0, StatusCode::FORBIDDEN);

        // Scheduled with nothing queued still changes; once queued, not.
        assert_eq!(h.send("POST", &format!("{detail}/start"), &admin, None).await.0, StatusCode::OK);
        let (status, body) = h.send("PUT", &detail, &admin, Some(edited.clone())).await;
        assert_eq!((status, body["state"].as_str()), (StatusCode::OK, Some("scheduled")));
        let (status, body) = h.send("DELETE", &detail, &admin, None).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "cannot delete a campaign that is scheduled");
        let uuid: Uuid = id.parse().unwrap();
        // The refused DELETE's rollback may still hold the row a moment:
        // the runner skips it then, as it would until its next tick.
        for _ in 0..50 {
            let step = Runner::new(pool.clone()).advance(uuid, Utc::now()).await.unwrap();
            if step != Step::Idle {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        // Under way: the name, windows and threshold change, nothing else.
        let mut renamed = edited.clone();
        renamed["name"] = json!("Running");
        renamed["windows"] = h.body()["windows"].clone();
        let (status, body) = h.send("PUT", &detail, &admin, Some(renamed.clone())).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!((body["name"].as_str(), body["state"].as_str()), (Some("Running"), Some("running")));
        assert_eq!(body["windows"].as_array().unwrap().len(), 1);
        assert_eq!(body["waves"][0]["servers"][0]["status"], "queued");
        let mut changed = renamed.clone();
        changed["security_only"] = json!(false);
        changed["server_ids"] = json!(h.servers);
        let (status, body) = h.send("PUT", &detail, &admin, Some(changed)).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(
            body["error"].as_str().unwrap().starts_with(
                "cannot change the servers or batches, the update while the campaign runs"
            ),
            "{body}"
        );
        let edited = renamed;

        // Stopped with its command sent: deleted once that ends.
        let command: Uuid = sqlx::query_scalar("SELECT id FROM commands WHERE campaign_id = $1")
            .bind(uuid)
            .fetch_one(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE commands SET state = 'sent', sent_at = now() WHERE id = $1")
            .bind(command)
            .execute(&pool)
            .await
            .unwrap();
        let (_, stopped) = h.send("POST", &format!("{detail}/stop"), &admin, None).await;
        assert_eq!(stopped["state"], "stopped");
        let (status, body) = h.send("DELETE", &detail, &admin, None).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "the campaign still has commands in flight: delete it once they end");
        let (status, body) = h.send("PUT", &detail, &admin, Some(edited)).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "the campaign still has commands in flight: edit it once they end");
        sqlx::query("UPDATE commands SET state = 'succeeded', finished_at = now() WHERE id = $1")
            .bind(command)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(h.send("DELETE", &detail, &admin, None).await.0, StatusCode::NO_CONTENT);
        assert_eq!(h.send("GET", &detail, &admin, None).await.0, StatusCode::NOT_FOUND);
        assert_eq!(h.send("DELETE", &detail, &admin, None).await.0, StatusCode::NOT_FOUND);
        // The command stays, without its campaign; the audit log keeps everything.
        let kept: Option<Uuid> = sqlx::query_scalar("SELECT campaign_id FROM commands WHERE id = $1")
            .bind(command)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(kept, None);
        let actions: Vec<String> = sqlx::query_scalar(
            "SELECT action FROM audit_log WHERE target_id = $1 AND action LIKE 'campaign.%' ORDER BY id",
        )
        .bind(&id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            actions,
            ["campaign.create", "campaign.update", "campaign.start", "campaign.update",
             "campaign.queue", "campaign.update", "campaign.stop", "campaign.delete"]
        );
        let details: Value = sqlx::query_scalar(
            "SELECT details FROM audit_log WHERE target_id = $1 AND action = 'campaign.delete'",
        )
        .bind(&id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((details["name"].as_str(), details["state"].as_str()), (Some("Running"), Some("stopped")));
        assert_eq!(details["servers"], 1);

        // A draft is deleted at once.
        let (_, draft) = h.send("POST", "/api/campaigns", &admin, Some(h.body())).await;
        let draft = format!("/api/campaigns/{}", draft["id"].as_str().unwrap());
        assert_eq!(h.send("DELETE", &draft, &admin, None).await.0, StatusCode::NO_CONTENT);
    }

    #[sqlx::test]
    async fn finished_campaigns_run_again_and_keep_their_runs(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let admin = h.login("admin", Role::Administrator, GrantScope::Customer(h.customer)).await;
        let viewer = h.login("viewer", Role::Viewer, GrantScope::Customer(h.customer)).await;
        let mut body = h.body();
        body["waves"] = json!([]);
        body["windows"] = json!([]);
        let (_, created) = h.send("POST", "/api/campaigns", &admin, Some(body.clone())).await;
        let id = created["id"].as_str().unwrap().to_owned();
        let uuid: Uuid = id.parse().unwrap();
        let detail = format!("/api/campaigns/{id}");
        let relaunch = format!("{detail}/relaunch");
        let runs = format!("{detail}/runs");
        let runner = Runner::new(pool.clone());
        // Each command of the campaign not ended yet ends: web-1 well, web-2 not.
        let end_commands = || async {
            sqlx::query(
                "UPDATE commands c SET state = CASE WHEN s.hostname = 'web-1' THEN 'succeeded'
                                                    ELSE 'failed' END,
                     sent_at = now(), finished_at = now()
                 FROM servers s
                 WHERE s.id = c.server_id AND c.campaign_id = $1 AND c.state = 'queued'",
            )
            .bind(uuid)
            .execute(&pool)
            .await
            .unwrap();
        };

        let (status, refused) = h.send("POST", &relaunch, &admin, Some(json!({}))).await;
        assert_eq!((status, refused["error"].as_str()), (StatusCode::CONFLICT, Some("cannot relaunch a campaign that is draft")));
        h.send("POST", &format!("{detail}/start"), &admin, None).await;
        runner.advance(uuid, Utc::now()).await.unwrap();
        end_commands().await;
        runner.advance(uuid, Utc::now()).await.unwrap();
        let (_, failed) = h.send("GET", &detail, &admin, None).await;
        assert_eq!(failed["state"], "failed");
        assert_eq!(failed["previous_runs"], 0);

        // Renaming a finished campaign keeps it as it is.
        let mut renamed = body.clone();
        renamed["name"] = json!("Renamed");
        let (status, view) = h.send("PUT", &detail, &admin, Some(renamed.clone())).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!((view["name"].as_str(), view["state"].as_str()), (Some("Renamed"), Some("failed")));
        assert_eq!(view["progress"]["failed"], 1);

        // Only the failed server runs again; the run that failed is kept.
        assert_eq!(h.send("POST", &relaunch, &viewer, Some(json!({}))).await.0, StatusCode::FORBIDDEN);
        assert_eq!(
            h.send("POST", &relaunch, &admin, Some(json!({"servers": "some"}))).await.0,
            StatusCode::BAD_REQUEST
        );
        let (status, view) = h.send("POST", &relaunch, &admin, Some(json!({"servers": "failed"}))).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["state"], "scheduled");
        assert_eq!(view["previous_runs"], 1);
        assert_eq!((view["error"].clone(), view["finished_at"].clone()), (Value::Null, Value::Null));
        assert_eq!(view["waves"].as_array().unwrap().len(), 1);
        assert_eq!(view["waves"][0]["servers"].as_array().unwrap().len(), 1);
        assert_eq!(view["waves"][0]["servers"][0]["server_id"], json!(h.servers[1]));
        assert_eq!(view["server_ids"], json!([]));
        let (status, list) = h.send("GET", &runs, &viewer, None).await;
        assert_eq!(status, StatusCode::OK, "{list}");
        let run = &list["runs"][0];
        assert_eq!((run["number"].as_i64(), run["state"].as_str()), (Some(1), Some("failed")));
        assert_eq!(run["settings"]["name"], "Renamed");
        assert_eq!((run["progress"]["succeeded"].as_i64(), run["progress"]["failed"].as_i64()), (Some(1), Some(1)));
        assert_eq!(run["waves"][0]["servers"][1]["status"], "failed");
        assert_eq!(run["error"], failed["error"]);
        let (status, again) = h.send("POST", &relaunch, &admin, Some(json!({}))).await;
        assert_eq!((status, again["error"].as_str()), (StatusCode::CONFLICT, Some("cannot relaunch a campaign that is scheduled")));

        // Stopped before anything was queued: editing it changes everything
        // and keeps no empty run.
        h.send("POST", &format!("{detail}/stop"), &admin, None).await;
        let mut edited = renamed.clone();
        edited["security_only"] = json!(false);
        let (status, view) = h.send("PUT", &detail, &admin, Some(edited)).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!((view["state"].as_str(), view["previous_runs"].as_i64()), (Some("draft"), Some(1)));
        assert_eq!(view["spec"]["security_only"], false);
        assert_eq!(view["waves"][0]["servers"].as_array().unwrap().len(), 2);

        // Completed: no server failed; all of them run again.
        h.send("POST", &format!("{detail}/start"), &admin, None).await;
        runner.advance(uuid, Utc::now()).await.unwrap();
        sqlx::query("UPDATE commands SET state = 'succeeded', sent_at = now(), finished_at = now()
                     WHERE campaign_id = $1 AND state = 'queued'")
            .bind(uuid)
            .execute(&pool)
            .await
            .unwrap();
        runner.advance(uuid, Utc::now()).await.unwrap();
        let (status, refused) = h.send("POST", &relaunch, &admin, Some(json!({"servers": "failed"}))).await;
        assert_eq!((status, refused["error"].as_str()), (StatusCode::CONFLICT, Some("no server of the campaign failed in its last run")));
        let (status, view) = h.send("POST", &relaunch, &admin, Some(json!({}))).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["previous_runs"], 2);
        assert_eq!(view["progress"]["pending"], 2);
        let (_, list) = h.send("GET", &runs, &admin, None).await;
        let states: Vec<&str> = list["runs"].as_array().unwrap().iter().map(|r| r["state"].as_str().unwrap()).collect();
        assert_eq!(states, ["completed", "failed"]);
        assert_eq!(list["runs"][0]["settings"]["spec"]["security_only"], false);

        // Editing a finished run prepares the next one as a draft.
        h.send("POST", &format!("{detail}/stop"), &admin, None).await;
        let mut next = body.clone();
        next["name"] = json!("Next");
        next["allow_reboot"] = json!(true);
        // Nothing ran since the relaunch: no new run is kept.
        let (_, view) = h.send("PUT", &detail, &admin, Some(next)).await;
        assert_eq!((view["state"].as_str(), view["previous_runs"].as_i64()), (Some("draft"), Some(2)));

        let actions: Vec<String> = sqlx::query_scalar(
            "SELECT action FROM audit_log WHERE target_id = $1 AND action LIKE 'campaign.relaunch' ORDER BY id",
        )
        .bind(&id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(actions.len(), 2);
        // Deleting it removes its runs with it.
        assert_eq!(h.send("DELETE", &detail, &admin, None).await.0, StatusCode::NO_CONTENT);
        let left: i64 = sqlx::query_scalar("SELECT count(*) FROM campaign_run_targets")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(left, 0);
    }
}
