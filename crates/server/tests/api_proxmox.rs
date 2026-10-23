//! Proxmox VM mappings through the API (ADR-0044): access, validation,
//! snapshot availability and the audit log. Ignored without
//! `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
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
    settings: ApiSettings,
    customer: Uuid,
    other_customer: Uuid,
    servers: [Uuid; 2],
}

impl Harness {
    async fn new(pool: &PgPool) -> Self {
        let id = |sql: &'static str| sqlx::query_scalar::<_, Uuid>(sql);
        let mut customers = Vec::new();
        let mut servers = Vec::new();
        for (n, name) in ["acme", "globex"].iter().enumerate() {
            let customer = id("INSERT INTO customers (name) VALUES ($1) RETURNING id")
                .bind(name)
                .fetch_one(pool)
                .await
                .unwrap();
            let site = id("INSERT INTO sites (name, customer_id) VALUES ($1, $2) RETURNING id")
                .bind(format!("{name}-site"))
                .bind(customer)
                .fetch_one(pool)
                .await
                .unwrap();
            for i in 0..2 {
                let server = id("INSERT INTO servers (peer_id, site_id, hostname, os_family)
                                 VALUES ($1, $2, 'web', 'debian') RETURNING id")
                .bind(format!("agent-{n}-{i}"))
                .bind(site)
                .fetch_one(pool)
                .await
                .unwrap();
                servers.push(server);
            }
            customers.push(customer);
        }
        Self {
            pool: pool.clone(),
            settings: ApiSettings {
                proxmox_enabled: true,
                proxmox_clusters: vec!["pve".into()],
                ..ApiSettings::default()
            },
            customer: customers[0],
            other_customer: customers[1],
            servers: [servers[0], servers[1]],
        }
    }

    async fn call(
        &self,
        method: &str,
        uri: &str,
        cookie: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
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
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
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
        let body = json!({ "username": name, "password": PASSWORD });
        let state = ApiState::new(
            self.pool.clone(),
            Registry::default(),
            self.settings.clone(),
        );
        let request = Request::post("/api/auth/login")
            .header(CSRF_HEADER, "1")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
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
}

db_tests! {
    #[sqlx::test]
    async fn administrators_map_servers_and_see_snapshot_support(pool: PgPool) {
        let mut h = Harness::new(&pool).await;
        let admin = h.login("boss", Role::Administrator, GrantScope::Customer(h.customer)).await;
        let uri = format!("/api/servers/{}/proxmox", h.servers[0]);

        let (status, body) = h.call("GET", &uri, &admin, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["mapping"], Value::Null);
        assert_eq!(body["snapshots"], json!({ "available": false, "reason": "not_mapped" }));

        let vm = json!({ "cluster": "pve", "node": "pve1", "vmid": 101 });
        let (status, body) = h.call("PUT", &uri, &admin, Some(vm.clone())).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["mapping"]["vmid"], 101);
        assert_eq!(body["mapping"]["node"], "pve1");
        assert_eq!(body["snapshots"], json!({ "available": true, "reason": null }));

        // Moved to another node of the cluster.
        let moved = json!({ "cluster": "pve", "node": "pve2", "vmid": 101 });
        assert_eq!(h.call("PUT", &uri, &admin, Some(moved)).await.0, StatusCode::OK);
        // One server per VM.
        let other = format!("/api/servers/{}/proxmox", h.servers[1]);
        let (status, body) = h.call("PUT", &other, &admin, Some(vm.clone())).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");

        for (bad, why) in [
            (json!({ "cluster": "lab", "node": "pve1", "vmid": 101 }), "unknown Proxmox cluster"),
            (json!({ "cluster": "pve", "node": "pve 1", "vmid": 101 }), "node"),
            (json!({ "cluster": "pve", "node": "pve1", "vmid": 99 }), "vmid"),
        ] {
            let (status, body) = h.call("PUT", &other, &admin, Some(bad)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert!(body["error"].as_str().unwrap().contains(why), "{body}");
        }
        let (status, _) = h.call("PUT", &other, &admin, Some(json!({ "cluster": "pve", "node": "n", "vmid": 1, "x": 1 }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // A cluster removed from the configuration, then the integration off.
        h.settings.proxmox_clusters = vec!["lab".into()];
        let (_, body) = h.call("GET", &uri, &admin, None).await;
        assert_eq!(body["snapshots"]["reason"], "unknown_cluster");
        h.settings.proxmox_enabled = false;
        let (_, body) = h.call("GET", &uri, &admin, None).await;
        assert_eq!(body["snapshots"], json!({ "available": false, "reason": "disabled" }));
        h.settings.proxmox_clusters = vec!["pve".into()];

        assert_eq!(h.call("DELETE", &uri, &admin, None).await.0, StatusCode::NO_CONTENT);
        assert_eq!(h.call("DELETE", &uri, &admin, None).await.0, StatusCode::NOT_FOUND);
        let (_, body) = h.call("GET", &uri, &admin, None).await;
        assert_eq!(body["mapping"], Value::Null);

        let audit: Vec<(String, String, Value)> = sqlx::query_as(
            "SELECT action, target_id, details FROM audit_log WHERE action LIKE 'server.proxmox.%' ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        let actions: Vec<&str> = audit.iter().map(|a| a.0.as_str()).collect();
        assert_eq!(actions, ["server.proxmox.set", "server.proxmox.set", "server.proxmox.clear"]);
        assert!(audit.iter().all(|a| a.1 == h.servers[0].to_string()));
        assert_eq!(audit[0].2["previous"], Value::Null);
        assert_eq!(audit[1].2["previous"], json!({ "cluster": "pve", "node": "pve1", "vmid": 101 }));
        assert_eq!(audit[2].2["node"], "pve2");
    }

    #[sqlx::test]
    async fn only_customer_administrators_reach_mappings(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let uri = format!("/api/servers/{}/proxmox", h.servers[0]);
        let vm = json!({ "cluster": "pve", "node": "pve1", "vmid": 101 });
        let operator = h.login("op", Role::Operator, GrantScope::Customer(h.customer)).await;
        assert_eq!(h.call("GET", &uri, &operator, None).await.0, StatusCode::FORBIDDEN);
        assert_eq!(h.call("PUT", &uri, &operator, Some(vm.clone())).await.0, StatusCode::FORBIDDEN);
        assert_eq!(h.call("DELETE", &uri, &operator, None).await.0, StatusCode::FORBIDDEN);
        let stranger = h.login("other", Role::Administrator, GrantScope::Customer(h.other_customer)).await;
        assert_eq!(h.call("GET", &uri, &stranger, None).await.0, StatusCode::NOT_FOUND);
        assert_eq!(h.call("PUT", &uri, &stranger, Some(vm)).await.0, StatusCode::NOT_FOUND);
        assert_eq!(h.call("GET", "/api/servers/nope/proxmox", &stranger, None).await.0, StatusCode::NOT_FOUND);
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM server_proxmox_vms").fetch_one(&pool).await.unwrap();
        assert_eq!(rows, 0);
    }
}
