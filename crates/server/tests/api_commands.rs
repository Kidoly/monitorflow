//! Commands and package reports through the API (ADR-0031): who may queue
//! an update, what is stored and audited, listings and the package
//! inventory. Ignored without `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use monitorflow_common::proto::v1::{ApplyUpdates, Package, PackageReport};
use monitorflow_common::redact::Secret;
use monitorflow_server::agent_releases::ReleaseCache;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
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
             VALUES ('agent-1', $1, 'web-1', 'debian') RETURNING id")
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

fn package(name: &str, upgradable: &str) -> Package {
    Package {
        name: name.into(),
        version: "1.0".into(),
        arch: "amd64".into(),
        upgradable_version: upgradable.into(),
        ..Package::default()
    }
}

db_tests! {
    #[sqlx::test]
    async fn customer_administrators_queue_updates_and_readers_follow_them(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let admin = h.login("admin", Role::Administrator, GrantScope::Customer(h.customer)).await;
        let site_admin = h.login("siteadmin", Role::Administrator, GrantScope::Site(h.site)).await;
        let viewer = h.login("viewer", Role::Viewer, GrantScope::Site(h.site)).await;
        let uri = format!("/api/servers/{}/commands", h.server);
        let body = json!({"kind": "apply_updates", "packages": ["libc6"], "security_only": true, "ttl_s": 600});

        for cookie in [&site_admin, &viewer] {
            assert_eq!(h.send("POST", &uri, cookie, Some(body.clone())).await.0, StatusCode::FORBIDDEN);
        }
        let bad = json!({"kind": "shell", "command": "id"});
        assert_eq!(h.send("POST", &uri, &admin, Some(bad)).await.0, StatusCode::BAD_REQUEST);
        let bad = json!({"kind": "apply_updates", "packages": ["--force-yes"]});
        assert_eq!(h.send("POST", &uri, &admin, Some(bad)).await.0, StatusCode::BAD_REQUEST);
        let missing = format!("/api/servers/{}/commands", Uuid::new_v4());
        assert_eq!(h.send("POST", &missing, &admin, Some(body.clone())).await.0, StatusCode::NOT_FOUND);

        let (status, created) = h.send("POST", &uri, &admin, Some(body.clone())).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["state"], "queued");
        assert_eq!(created["kind"], "apply_updates");
        assert_eq!(
            created["parameters"],
            json!({"packages": ["libc6"], "security_only": true, "allow_reboot": false})
        );
        assert!(created["requested_by"].as_str().is_some());
        let id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
        let payload: Vec<u8> = sqlx::query_scalar("SELECT payload FROM commands WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(ApplyUpdates::decode(payload.as_slice()).unwrap().packages, ["libc6"]);
        assert_eq!(h.audit_count("command.create").await, 1);

        // Readers of the site see it, with its output in the detail.
        sqlx::query("UPDATE commands SET state = 'sent', sent_at = now() WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE commands SET state = 'succeeded', started_at = now(), finished_at = now(),
                 exit_code = 0, stdout = 'upgraded' WHERE id = $1",
        )
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
        let (status, detail) = h.send("GET", &format!("/api/commands/{id}"), &viewer, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!((detail["state"].as_str(), detail["stdout"].as_str()), (Some("succeeded"), Some("upgraded")));
        assert_eq!(h.send("GET", &format!("/api/commands/{}", Uuid::new_v4()), &viewer, None).await.0, StatusCode::NOT_FOUND);

        // Newest first, paginated.
        let (_, second) = h.send("POST", &uri, &admin, Some(json!({"kind": "apply_updates"}))).await;
        let (status, page) = h.send("GET", &format!("{uri}?limit=1"), &viewer, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(page["commands"][0]["id"], second["id"]);
        assert!(page["commands"][0].get("stdout").is_none());
        let cursor = page["next_cursor"].as_str().unwrap();
        let (_, page) = h.send("GET", &format!("{uri}?limit=1&cursor={cursor}"), &viewer, None).await;
        assert_eq!(page["commands"][0]["id"], created["id"]);
        assert_eq!(page["next_cursor"], Value::Null);

        // A revoked server takes no new command.
        sqlx::query("UPDATE servers SET revoked_at = now()").execute(&pool).await.unwrap();
        assert_eq!(h.send("POST", &uri, &admin, Some(body)).await.0, StatusCode::CONFLICT);
    }

    #[sqlx::test]
    async fn package_reports_replace_the_inventory_and_list_by_page(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let viewer = h.login("viewer", Role::Viewer, GrantScope::Site(h.site)).await;
        let uri = format!("/api/servers/{}/packages", h.server);
        let (status, empty) = h.send("GET", &uri, &viewer, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!((empty["reboot_required"].clone(), empty["packages"].clone()), (Value::Null, json!([])));

        let store = PackageStore::new(pool.clone());
        let first = PackageReport {
            packages: vec![package("old", "")],
            reboot_required: false,
            collected_at_unix_ms: 1_800_000_000_000,
        };
        assert!(store.replace("agent-1", &first).await.unwrap());
        let report = PackageReport {
            packages: vec![
                package("libc6", "2.36-9+deb12u4"),
                package("bash", ""),
                package("bash", ""),
                package("", "x"),
                package("zlib1g", "1:1.3"),
            ],
            reboot_required: true,
            collected_at_unix_ms: 1_800_000_060_000,
        };
        assert!(store.replace("agent-1", &report).await.unwrap());
        // Older or unknown: ignored.
        assert!(!store.replace("agent-1", &first).await.unwrap());
        assert!(!store.replace("nobody", &report).await.unwrap());

        let (_, all) = h.send("GET", &uri, &viewer, None).await;
        let names: Vec<&str> = all["packages"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["bash", "libc6", "zlib1g"]);
        assert_eq!(all["reboot_required"], true);
        assert_eq!(all["reported_at"], "2027-01-15T08:01:00Z");
        assert_eq!(all["packages"][0]["upgradable_version"], Value::Null);

        let (_, page) = h.send("GET", &format!("{uri}?upgradable=true&limit=1"), &viewer, None).await;
        assert_eq!(page["packages"][0]["name"], "libc6");
        let cursor = page["next_cursor"].as_str().unwrap();
        let (_, page) = h.send("GET", &format!("{uri}?upgradable=true&limit=1&cursor={cursor}"), &viewer, None).await;
        assert_eq!(page["packages"][0]["upgradable_version"], "1:1.3");
        assert_eq!(page["next_cursor"], Value::Null);
        assert_eq!(h.send("GET", &format!("{uri}?limit=0"), &viewer, None).await.0, StatusCode::BAD_REQUEST);
        assert_eq!(h.send("GET", &format!("{uri}?upgradeable=true"), &viewer, None).await.0, StatusCode::BAD_REQUEST);
    }

    #[sqlx::test]
    async fn administrators_queue_self_updates_with_the_cache_first(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let admin = h.login("admin", Role::Administrator, GrantScope::Customer(h.customer)).await;
        let viewer = h.login("viewer", Role::Viewer, GrantScope::Site(h.site)).await;
        let uri = format!("/api/servers/{}/commands", h.server);
        let mirrors = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(mirrors.path().join("agent/0.3.0")).unwrap();
        std::fs::write(mirrors.path().join("agent/0.3.0/VERSION"), "0.3.0\n").unwrap();
        let settings = || {
            let mut settings = ApiSettings::default();
            settings.agent_releases.cache = ReleaseCache::in_mirror_dir(mirrors.path());
            settings.agent_releases.mirror_public_url = Some("http://mf.example:7480".into());
            settings
        };

        let update = json!({"kind": "self_update", "version": "0.3.0", "health_timeout_s": 60});
        assert_eq!(h.send_with(settings(), "POST", &uri, &viewer, Some(update.clone())).await.0, StatusCode::FORBIDDEN);
        for bad in [
            json!({"kind": "self_update"}),
            json!({"kind": "self_update", "version": "latest"}),
            json!({"kind": "self_update", "version": "0.3.0", "packages": ["x"]}),
            json!({"kind": "self_update", "version": "0.3.0", "health_timeout_s": 5}),
            json!({"kind": "apply_updates", "version": "0.3.0"}),
        ] {
            let (status, body) = h.send_with(settings(), "POST", &uri, &admin, Some(bad.clone())).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
        }
        let (status, created) = h.send_with(settings(), "POST", &uri, &admin, Some(update)).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(
            created["parameters"],
            json!({
                "version": "0.3.0",
                "base_urls": [
                    "http://mf.example:7480/agent/0.3.0/",
                    "https://github.com/Kidoly/monitorflow/releases/download/v0.3.0/"
                ],
                "allow_downgrade": false,
                "health_timeout_seconds": 60
            })
        );
        assert_eq!(h.audit_count("command.create").await, 1);
        // A version the server does not hold: GitHub only.
        let (_, other) = h
            .send_with(settings(), "POST", &uri, &admin, Some(json!({"kind": "self_update", "version": "0.4.0", "allow_downgrade": true})))
            .await;
        assert_eq!(other["parameters"]["base_urls"].as_array().unwrap().len(), 1);
        assert_eq!(other["parameters"]["allow_downgrade"], true);

        let (status, releases) = h.send_with(settings(), "GET", "/api/agent-releases", &viewer, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(releases, json!({"versions": ["0.3.0"], "latest": "0.3.0"}));
    }

    #[sqlx::test]
    async fn hosts_without_a_package_manager_take_no_updates_or_sources(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let admin = h.login("admin", Role::Administrator, GrantScope::Customer(h.customer)).await;
        let alpine: Uuid = sqlx::query_scalar(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family, package_updates)
             VALUES ('agent-2', $1, 'edge-1', 'linux', false) RETURNING id",
        )
        .bind(h.site)
        .fetch_one(&pool)
        .await
        .unwrap();
        let unavailable = "package updates aren't available on this system";

        let (status, server) = h.send("GET", &format!("/api/servers/{alpine}"), &admin, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(server["os_family"], "linux");
        assert_eq!(server["package_updates"], false);
        let (_, other) = h.send("GET", &format!("/api/servers/{}", h.server), &admin, None).await;
        assert_eq!(other["package_updates"], true);

        let commands = format!("/api/servers/{alpine}/commands");
        let (status, body) = h.send("POST", &commands, &admin, Some(json!({"kind": "apply_updates"}))).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"], unavailable);
        let sources = format!("/api/servers/{alpine}/sources");
        for (method, body) in [("POST", Some(json!({"mirrors": [{"name": "debian-12"}]}))), ("DELETE", None)] {
            let (status, body) = h.send(method, &sources, &admin, body).await;
            assert_eq!(status, StatusCode::CONFLICT, "{method}");
            assert_eq!(body["error"], unavailable);
        }

        // Its agent still updates itself.
        let (status, body) = h.send("POST", &commands, &admin, Some(json!({"kind": "self_update", "version": "0.3.0"}))).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }

    #[sqlx::test]
    async fn windows_servers_take_windows_update_names_and_no_sources(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let admin = h.login("admin", Role::Administrator, GrantScope::Customer(h.customer)).await;
        let windows: Uuid = sqlx::query_scalar(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family)
             VALUES ('agent-2', $1, 'win-1', 'windows') RETURNING id",
        )
        .bind(h.site)
        .fetch_one(&pool)
        .await
        .unwrap();
        let commands = |id: Uuid| format!("/api/servers/{id}/commands");
        let named = |names: &[&str]| json!({"kind": "apply_updates", "packages": names});

        let (status, body) = h.send("POST", &commands(windows), &admin, Some(named(&["openssl"]))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "`openssl` is not a KB number or update ID, which windows servers need");
        let (status, body) = h.send("POST", &commands(h.server), &admin, Some(named(&["KB5034441"]))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "`KB5034441` is a windows update name, which debian servers cannot take");
        let (status, created) = h.send("POST", &commands(windows), &admin, Some(named(&["KB5034441"]))).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let (status, _) = h.send("POST", &commands(windows), &admin, Some(json!({"kind": "apply_updates", "security_only": true}))).await;
        assert_eq!(status, StatusCode::CREATED);

        // Windows agents update through their update service (ADR-0053).
        let (status, body) = h.send("POST", &commands(windows), &admin, Some(json!({"kind": "self_update", "version": "0.3.0"}))).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");

        let sources = format!("/api/servers/{windows}/sources");
        for (method, body) in [("POST", Some(json!({"mirrors": [{"name": "debian-12"}]}))), ("DELETE", None)] {
            let (status, body) = h.send(method, &sources, &admin, body).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{method}");
            assert_eq!(body["error"], "package sources are not supported on windows servers");
        }

        let report = PackageReport {
            packages: vec![Package {
                name: "KB5034441".into(),
                upgradable_version: "200".into(),
                title: "2024-01 Security Update".into(),
                classification: "Security Updates".into(),
                severity: "Critical".into(),
                size_bytes: 4096,
                reboot_required: true,
                ..Package::default()
            }],
            reboot_required: false,
            collected_at_unix_ms: 1_800_000_000_000,
        };
        assert!(PackageStore::new(pool.clone()).replace("agent-2", &report).await.unwrap());
        let (_, list) = h.send("GET", &format!("/api/servers/{windows}/packages"), &admin, None).await;
        assert_eq!(
            list["packages"],
            json!([{"name": "KB5034441", "arch": "", "version": "", "upgradable_version": "200",
                    "title": "2024-01 Security Update", "classification": "Security Updates",
                    "severity": "Critical", "size_bytes": 4096, "reboot_required": true}])
        );
    }
}
