//! The software catalogue and software commands through the API
//! (ADR-0188): who may change the catalogue, what campaigns and commands
//! resolve from it, and which servers take software or update approvals.
//! Ignored without `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use monitorflow_common::proto::v1::{software_change, ApplyUpdates, SoftwareChange};
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
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
    server: Uuid,
}

impl Harness {
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
        let server = one("INSERT INTO servers (peer_id, site_id, hostname, os_family)
             VALUES ('agent-1', $1, 'win-1', 'windows') RETURNING id")
        .bind(site)
        .fetch_one(pool)
        .await
        .unwrap();
        Self {
            pool: pool.clone(),
            customer,
            site,
            server,
        }
    }

    async fn send(
        &self,
        method: &str,
        uri: &str,
        cookie: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        self.send_with(ApiSettings::default(), method, uri, cookie, body)
            .await
    }

    async fn send_with(
        &self,
        settings: ApiSettings,
        method: &str,
        uri: &str,
        cookie: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let state = ApiState::new(self.pool.clone(), Registry::default(), settings);
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

    async fn audit_count(&self, action: &str) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = $1")
            .bind(action)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }
}

impl Harness {
    /// Sets what the server's agent reported for its command kinds.
    async fn kinds(&self, kinds: Option<Value>) {
        sqlx::query("UPDATE servers SET command_kinds = $2 WHERE id = $1")
            .bind(self.server)
            .bind(kinds)
            .execute(&self.pool)
            .await
            .unwrap();
    }

    async fn payload(&self, command: &Value) -> Vec<u8> {
        let id: Uuid = command["id"].as_str().unwrap().parse().unwrap();
        sqlx::query_scalar("SELECT payload FROM commands WHERE id = $1")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }
}

db_tests! {
    #[sqlx::test]
    async fn the_catalogue_is_shared_or_per_customer(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let global = h.login("root", Role::Administrator, GrantScope::Global).await;
        let admin = h.login("admin", Role::Administrator, GrantScope::Customer(h.customer)).await;
        let operator = h.login("op", Role::Operator, GrantScope::Customer(h.customer)).await;
        let other: Uuid = sqlx::query_scalar("INSERT INTO customers (name) VALUES ('globex') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();

        let shared = json!({"name": "7-Zip", "package_id": "7zip.7zip"});
        // Shared entries are for global administrators.
        let (status, _) = h.send("POST", "/api/software", &admin, Some(shared.clone())).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, seven) = h.send("POST", "/api/software", &global, Some(shared.clone())).await;
        assert_eq!(status, StatusCode::CREATED, "{seven}");
        assert_eq!(seven["customer_id"], Value::Null);
        assert_eq!(seven["source"], "winget");
        let (status, _) = h.send("POST", "/api/software", &global, Some(shared)).await;
        assert_eq!(status, StatusCode::CONFLICT);

        // A customer's administrators manage its own entries.
        let own = json!({"customer_id": h.customer, "name": "Tool", "package_id": "Acme.Tool",
                         "source": "acme-internal"});
        let (status, _) = h.send("POST", "/api/software", &operator, Some(own.clone())).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, tool) = h.send("POST", "/api/software", &admin, Some(own)).await;
        assert_eq!(status, StatusCode::CREATED, "{tool}");
        let theirs = json!({"customer_id": other, "name": "X", "package_id": "Globex.X"});
        let (status, _) = h.send("POST", "/api/software", &admin, Some(theirs.clone())).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = h.send("POST", "/api/software", &global, Some(theirs)).await;
        assert_eq!(status, StatusCode::CREATED);
        let bad = json!({"name": "Store", "package_id": "a.b", "source": "msstore"});
        let (status, _) = h.send("POST", "/api/software", &global, Some(bad)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // Each sees the shared entries and its customers'.
        let names = |list: &Value| {
            list["software"].as_array().unwrap().iter()
                .map(|e| e["name"].as_str().unwrap().to_owned()).collect::<Vec<_>>()
        };
        let (_, list) = h.send("GET", "/api/software", &operator, None).await;
        assert_eq!(names(&list), ["7-Zip", "Tool"]);
        let (_, list) = h.send("GET", "/api/software", &global, None).await;
        assert_eq!(names(&list), ["7-Zip", "Tool", "X"]);
        let uri = format!("/api/software?customer_id={other}");
        let (_, list) = h.send("GET", &uri, &global, None).await;
        assert_eq!(names(&list), ["7-Zip", "X"]);

        // Edited and deleted under the same rules, and audited.
        let uri = format!("/api/software/{}", tool["id"].as_str().unwrap());
        let edit = json!({"customer_id": h.customer, "name": "Acme tool", "package_id": "Acme.Tool",
                          "source": "acme-internal", "description": "Inventory agent"});
        let (status, edited) = h.send("PUT", &uri, &admin, Some(edit)).await;
        assert_eq!(status, StatusCode::OK, "{edited}");
        assert_eq!(edited["description"], "Inventory agent");
        let moved = json!({"name": "Acme tool", "package_id": "Acme.Tool"});
        let (status, _) = h.send("PUT", &uri, &admin, Some(moved)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let shared_uri = format!("/api/software/{}", seven["id"].as_str().unwrap());
        let (status, _) = h.send("DELETE", &shared_uri, &admin, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = h.send("DELETE", &uri, &admin, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = h.send("DELETE", &uri, &admin, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(h.audit_count("software.create").await, 3);
        assert_eq!(h.audit_count("software.update").await, 1);
        assert_eq!(h.audit_count("software.delete").await, 1);
    }

    #[sqlx::test]
    async fn software_commands_need_the_hosts_consent(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let global = h.login("root", Role::Administrator, GrantScope::Global).await;
        let admin = h.login("admin", Role::Administrator, GrantScope::Customer(h.customer)).await;
        let (_, seven) = h.send("POST", "/api/software", &global,
            Some(json!({"name": "7-Zip", "package_id": "7zip.7zip"}))).await;
        let uri = format!("/api/servers/{}/commands", h.server);
        let request = json!({"kind": "software", "allow_reboot": true,
            "software": {"action": "install", "catalog_ids": [seven["id"]]}});

        // An agent too old, then one whose administrator did not allow it.
        let (status, body) = h.send("POST", &uri, &admin, Some(request.clone())).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body["error"].as_str().unwrap().contains("too old"), "{body}");
        h.kinds(Some(json!({"software": "off"}))).await;
        let (status, body) = h.send("POST", &uri, &admin, Some(request.clone())).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body["error"].as_str().unwrap().contains("software on"), "{body}");

        h.kinds(Some(json!({"software": "accepted"}))).await;
        let (status, command) = h.send("POST", &uri, &admin, Some(request)).await;
        assert_eq!(status, StatusCode::CREATED, "{command}");
        assert_eq!(command["kind"], "software");
        assert_eq!(command["parameters"]["action"], "install");
        let change = SoftwareChange::decode(h.payload(&command).await.as_slice()).unwrap();
        assert_eq!(change.action(), software_change::Action::Install);
        assert_eq!((change.packages[0].id.as_str(), change.packages[0].source.as_str()),
            ("7zip.7zip", "winget"));
        assert!(change.allow_reboot);

        for bad in [
            json!({"kind": "software", "software": {"action": "install", "catalog_ids": [Uuid::nil()]}}),
            json!({"kind": "software", "software": {"action": "upgrade", "catalog_ids": [seven["id"]]}}),
            json!({"kind": "software"}),
            json!({"kind": "software", "packages": ["KB1"],
                   "software": {"action": "remove", "catalog_ids": [seven["id"]]}}),
        ] {
            let (status, body) = h.send("POST", &uri, &admin, Some(bad.clone())).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
        }
        // Linux servers take no software yet.
        sqlx::query("UPDATE servers SET os_family = 'debian' WHERE id = $1")
            .bind(h.server)
            .execute(&pool)
            .await
            .unwrap();
        let remove = json!({"kind": "software",
            "software": {"action": "remove", "catalog_ids": [seven["id"]]}});
        let (status, _) = h.send("POST", &uri, &admin, Some(remove)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[sqlx::test]
    async fn approvals_reach_only_agents_that_know_them(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let admin = h.login("admin", Role::Administrator, GrantScope::Customer(h.customer)).await;
        let uri = format!("/api/servers/{}/commands", h.server);
        let request = json!({"kind": "apply_updates", "classifications": ["Security Updates"],
            "hold": ["KB5034441"]});
        let (status, body) = h.send("POST", &uri, &admin, Some(request.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body["error"].as_str().unwrap().contains("approvals"), "{body}");
        // Reporting the software kind, even refused, says the agent knows.
        h.kinds(Some(json!({"software": "off"}))).await;
        let (status, command) = h.send("POST", &uri, &admin, Some(request)).await;
        assert_eq!(status, StatusCode::CREATED, "{command}");
        assert_eq!(command["parameters"]["hold"], json!(["KB5034441"]));
        let apply = ApplyUpdates::decode(h.payload(&command).await.as_slice()).unwrap();
        assert_eq!(apply.classifications, ["Security Updates"]);
        let bad = json!({"kind": "apply_updates", "classifications": ["Drivers"]});
        let (status, _) = h.send("POST", &uri, &admin, Some(bad)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[sqlx::test]
    async fn software_campaigns_resolve_the_catalogue(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let global = h.login("root", Role::Administrator, GrantScope::Global).await;
        let admin = h.login("admin", Role::Administrator, GrantScope::Customer(h.customer)).await;
        let (_, seven) = h.send("POST", "/api/software", &global,
            Some(json!({"name": "7-Zip", "package_id": "7zip.7zip"}))).await;
        // A Linux server of the site is no target of a software campaign.
        sqlx::query("INSERT INTO servers (peer_id, site_id, hostname, os_family)
                     VALUES ('agent-2', $1, 'web-1', 'debian')")
            .bind(h.site)
            .execute(&pool)
            .await
            .unwrap();
        let request = json!({"customer_id": h.customer, "name": "Tools", "site_id": h.site,
            "software": {"action": "install", "catalog_ids": [seven["id"]]}});
        let (status, campaign) = h.send("POST", "/api/campaigns", &admin, Some(request)).await;
        assert_eq!(status, StatusCode::CREATED, "{campaign}");
        assert_eq!(campaign["software"]["action"], "install");
        assert_eq!(campaign["software"]["packages"][0]["id"], "7zip.7zip");
        assert_eq!(campaign["software"]["packages"][0]["catalog_id"], seven["id"]);
        let hosts: Vec<&str> = campaign["waves"][0]["servers"].as_array().unwrap().iter()
            .map(|s| s["hostname"].as_str().unwrap()).collect();
        assert_eq!(hosts, ["win-1"]);

        // The campaign keeps what it resolved when the entry goes.
        let entry = format!("/api/software/{}", seven["id"].as_str().unwrap());
        let (status, _) = h.send("DELETE", &entry, &global, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let uri = format!("/api/campaigns/{}", campaign["id"].as_str().unwrap());
        let (_, again) = h.send("GET", &uri, &admin, None).await;
        assert_eq!(again["software"], campaign["software"]);

        let mixed = json!({"customer_id": h.customer, "name": "Mixed", "site_id": h.site,
            "security_only": true,
            "software": {"action": "install", "catalog_ids": [Uuid::nil()]}});
        let (status, _) = h.send("POST", "/api/campaigns", &admin, Some(mixed)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}
