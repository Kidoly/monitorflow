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
use monitorflow_server::campaign::Runner;
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
                "command_ttl_s", "created_at", "created_by", "current_wave", "customer_id",
                "customer_name", "error", "failure_threshold", "finished_at", "id", "name",
                "progress", "site_id", "spec", "started_at", "state", "wave_count", "waves",
                "windows"
            ]
        );
        assert_eq!(created["state"], "draft");
        assert_eq!(created["customer_name"], "acme");
        assert_eq!(created["spec"], json!({"packages": [], "security_only": true, "allow_reboot": false}));
        assert_eq!(created["command_ttl_s"], 3600);
        assert_eq!(created["failure_threshold"], json!({"value": 10, "unit": "percent"}));
        assert_eq!(created["windows"], h.body()["windows"]);
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
            ["command_id", "error", "exit_code", "finished_at", "hostname", "peer_id", "server_id",
             "site_id", "status"]
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
}
