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

/// A minimal MSI package with a Property table.
fn msi_package(properties: &[(&str, &str)]) -> Vec<u8> {
    let mut package = msi::Package::create(
        msi::PackageType::Installer,
        std::io::Cursor::new(Vec::new()),
    )
    .unwrap();
    package
        .create_table(
            "Property",
            vec![
                msi::Column::build("Property").primary_key().id_string(72),
                msi::Column::build("Value").nullable().text_string(0),
            ],
        )
        .unwrap();
    let rows = properties
        .iter()
        .map(|(k, v)| vec![msi::Value::from(*k), msi::Value::from(*v)])
        .collect();
    package
        .insert_rows(msi::Insert::into("Property").rows(rows))
        .unwrap();
    package.into_inner().unwrap().into_inner()
}

/// Settings whose MSI files live in `dir`, with a mirror URL.
fn msi_settings(dir: &std::path::Path) -> ApiSettings {
    let mut settings = ApiSettings::default();
    settings.software_files = monitorflow_server::software_files::Store::in_mirror_dir(dir);
    settings.live = settings
        .live
        .clone()
        .with_mirror_url(Some("http://mf.example:7480"));
    settings
}

impl Harness {
    /// Sends `file` as the raw body of `POST uri`.
    async fn upload(
        &self,
        settings: ApiSettings,
        uri: &str,
        cookie: &str,
        file: Vec<u8>,
    ) -> (StatusCode, Value) {
        let state = ApiState::new(self.pool.clone(), Registry::default(), settings);
        let request = Request::post(uri)
            .header(CSRF_HEADER, "1")
            .header("content-type", "application/octet-stream")
            .header("cookie", cookie)
            .body(Body::from(file))
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
        // A winget entry is nothing for a Linux server.
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

    #[sqlx::test]
    async fn linux_entries_name_packages_and_maybe_a_mirror(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let global = h.login("root", Role::Administrator, GrantScope::Global).await;
        let admin = h.login("admin", Role::Administrator, GrantScope::Customer(h.customer)).await;
        let mirror: Uuid = sqlx::query_scalar(
            "INSERT INTO mirrors (name, family, upstream_url, distribution, components)
             VALUES ('nginx-stable', 'apt', 'https://nginx.example/packages/debian', 'trixie',
                     '{nginx}') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let add = |body: Value| h.send("POST", "/api/software", &global, Some(body));
        let (status, nginx) = add(json!({"name": "nginx", "manager": "apt",
            "package_id": "nginx", "mirror": "nginx-stable"})).await;
        assert_eq!(status, StatusCode::CREATED, "{nginx}");
        assert_eq!((nginx["source"].as_str(), nginx["mirror"].as_str()), (Some(""), Some("nginx-stable")));
        // The same name for dnf is another entry.
        let (status, httpd) = add(json!({"name": "nginx", "manager": "dnf", "package_id": "nginx"})).await;
        assert_eq!(status, StatusCode::CREATED, "{httpd}");
        assert_eq!(httpd["mirror"], Value::Null);
        for bad in [
            json!({"name": "x", "manager": "apt", "package_id": "--force-yes"}),
            json!({"name": "x", "manager": "apt", "package_id": "./x.deb"}),
            json!({"name": "x", "manager": "dnf", "package_id": "@core"}),
            json!({"name": "x", "manager": "apt", "package_id": "x", "source": "winget"}),
            // A mirror of another family, or none of that name.
            json!({"name": "x", "manager": "dnf", "package_id": "x", "mirror": "nginx-stable"}),
            json!({"name": "x", "manager": "apt", "package_id": "x", "mirror": "missing"}),
            json!({"name": "x", "package_id": "a.b", "mirror": "nginx-stable"}),
            json!({"name": "x", "manager": "snap", "package_id": "lxd"}),
        ] {
            let (status, body) = add(bad.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
        }

        // A Debian server allowing software takes the apt entry alone.
        sqlx::query("UPDATE servers SET os_family = 'debian' WHERE id = $1")
            .bind(h.server)
            .execute(&pool)
            .await
            .unwrap();
        h.kinds(Some(json!({"software": "accepted"}))).await;
        let uri = format!("/api/servers/{}/commands", h.server);
        let both = json!({"kind": "software",
            "software": {"action": "remove", "catalog_ids": [httpd["id"]]}});
        let (status, body) = h.send("POST", &uri, &admin, Some(both)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body["error"].as_str().unwrap().contains("none of this software"), "{body}");
        // An entry from a mirror needs a campaign, which adds the mirror.
        let mirrored = json!({"kind": "software",
            "software": {"action": "install", "catalog_ids": [nginx["id"]]}});
        let (status, body) = h.send("POST", &uri, &admin, Some(mirrored)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body["error"].as_str().unwrap().contains("campaign"), "{body}");
        sqlx::query("UPDATE software_catalog SET mirror_id = NULL WHERE id = $1")
            .bind(nginx["id"].as_str().unwrap().parse::<Uuid>().unwrap())
            .execute(&pool)
            .await
            .unwrap();
        let plain = json!({"kind": "software",
            "software": {"action": "install", "catalog_ids": [nginx["id"], httpd["id"]]}});
        let (status, command) = h.send("POST", &uri, &admin, Some(plain)).await;
        assert_eq!(status, StatusCode::CREATED, "{command}");
        assert_eq!(command["parameters"]["manager"], "apt");
        let change = SoftwareChange::decode(h.payload(&command).await.as_slice()).unwrap();
        assert_eq!(change.packages.len(), 1);
        assert_eq!((change.packages[0].id.as_str(), change.packages[0].source.as_str()), ("nginx", ""));

        // A campaign of mirrored software pins the mirror's newest snapshot,
        // which it needs first.
        sqlx::query("UPDATE software_catalog SET mirror_id = $1")
            .bind(mirror)
            .execute(&pool)
            .await
            .unwrap();
        let campaign = json!({"customer_id": h.customer, "name": "nginx", "site_id": h.site,
            "software": {"action": "install", "catalog_ids": [nginx["id"]]}});
        let (status, body) = h.send("POST", "/api/campaigns", &admin, Some(campaign.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body["error"].as_str().unwrap().contains("no published snapshot"), "{body}");
        sqlx::query("INSERT INTO mirror_snapshots (mirror_id, name, state, published_at)
                     VALUES ($1, 's1', 'published', now() - interval '1 day'),
                            ($1, 's2', 'published', now())")
            .bind(mirror)
            .execute(&pool)
            .await
            .unwrap();
        // Then mirrors must be served by this server, as for any pin.
        let (status, body) = h.send("POST", "/api/campaigns", &admin, Some(campaign)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body["error"].as_str().unwrap().contains("mirror"), "{body}");
    }

    #[sqlx::test]
    async fn software_is_turned_off_from_the_web_never_on(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let admin = h.login("admin", Role::Administrator, GrantScope::Customer(h.customer)).await;
        let operator = h.login("op", Role::Operator, GrantScope::Customer(h.customer)).await;
        let uri = format!("/api/servers/{}/software", h.server);
        let off = json!({"on": false});
        let (status, _) = h.send("POST", &uri, &operator, Some(off.clone())).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        // An agent that does not know the command, then one already off.
        h.kinds(Some(json!({"software": "accepted"}))).await;
        let (status, body) = h.send("POST", &uri, &admin, Some(off.clone())).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body["error"].as_str().unwrap().contains("updates by itself"), "{body}");
        h.kinds(Some(json!({"software": "off", "software_off": "accepted"}))).await;
        let (status, body) = h.send("POST", &uri, &admin, Some(off.clone())).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body["error"].as_str().unwrap().contains("already off"), "{body}");

        h.kinds(Some(json!({"software": "accepted", "software_off": "accepted"}))).await;
        // On is the server's own administrator's, on the server.
        let (status, body) = h.send("POST", &uri, &admin, Some(json!({"on": true}))).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body["error"].as_str().unwrap().contains("monitorflow-agent.exe software on"), "{body}");
        let (status, first) = h.send("POST", &uri, &admin, Some(off.clone())).await;
        assert_eq!(status, StatusCode::CREATED, "{first}");
        assert_eq!(first["kind"], "software_off");
        // A newer request replaces one still queued.
        let (status, second) = h.send("POST", &uri, &admin, Some(off)).await;
        assert_eq!(status, StatusCode::CREATED);
        let states: Vec<String> = sqlx::query_scalar(
            "SELECT state::text FROM commands WHERE server_id = $1 AND kind = 'software_off'
             ORDER BY issued_at, id",
        )
        .bind(h.server)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(states.iter().filter(|s| *s == "cancelled").count(), 1, "{states:?}");
        assert_eq!(h.audit_count("server.software_off").await, 2);
        let (_, server) = h.send("GET", &format!("/api/servers/{}", h.server), &operator, None).await;
        assert_eq!(server["software_off_change"]["command_id"], second["id"]);
        assert_eq!(server["software_off_change"]["requested_by"], "admin");
    }

    #[sqlx::test]
    async fn msi_files_are_uploaded_listed_sent_by_digest_and_swept(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let dir = tempfile::tempdir().unwrap();
        let settings = || msi_settings(dir.path());
        let admin = h.login("admin", Role::Administrator, GrantScope::Customer(h.customer)).await;
        let operator = h.login("op", Role::Operator, GrantScope::Customer(h.customer)).await;
        let other: Uuid = sqlx::query_scalar("INSERT INTO customers (name) VALUES ('globex') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
        let stranger = h.login("stranger", Role::Administrator, GrantScope::Customer(other)).await;
        let code = "{6F1C2E7A-3B9D-4C55-A0E2-8D4F7B1A9C30}";
        let file = msi_package(&[
            ("ProductCode", &code.to_lowercase()),
            ("ProductName", "Example Tool"),
            ("ProductVersion", "1.2.0"),
            ("Manufacturer", "Example"),
        ]);
        let uri = "/api/software/files?file_name=C:%5Cdl%5Ctool-1.2.msi";

        let (status, _) = h.upload(settings(), uri, &operator, file.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, body) = h.upload(settings(), uri, &admin, b"MZ not an msi".to_vec()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body["error"].as_str().unwrap().contains("not a Windows Installer"), "{body}");
        let (status, uploaded) = h.upload(settings(), uri, &admin, file.clone()).await;
        assert_eq!(status, StatusCode::CREATED, "{uploaded}");
        assert_eq!(uploaded["product_code"], code);
        assert_eq!(uploaded["product_name"], "Example Tool");
        assert_eq!(uploaded["file_name"], "tool-1.2.msi");
        assert_eq!(uploaded["size"], file.len());
        let sha256 = uploaded["sha256"].as_str().unwrap().to_owned();
        let stored = dir.path().join("software").join(format!("{sha256}.msi"));
        assert_eq!(std::fs::read(&stored).unwrap(), file);
        assert_eq!(h.audit_count("software.upload").await, 1);

        let entry = json!({"customer_id": h.customer, "name": "Example Tool", "manager": "msi",
                           "file_sha256": sha256, "properties": [{"name": "MARKER", "value": "x"}]});
        // Only who uploaded a file may name it.
        let theirs = json!({"customer_id": other, "name": "Example Tool", "manager": "msi",
                            "file_sha256": sha256});
        let (status, _) = h.send("POST", "/api/software", &stranger, Some(theirs)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let mut reboot = entry.clone();
        reboot["properties"] = json!([{"name": "REBOOT", "value": "Force"}]);
        let (status, _) = h.send("POST", "/api/software", &admin, Some(reboot)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, created) = h.send("POST", "/api/software", &admin, Some(entry)).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["package_id"], code);
        assert_eq!(created["file"]["sha256"], sha256.as_str());
        assert_eq!(created["properties"][0]["name"], "MARKER");
        let (_, list) = h.send("GET", "/api/software", &operator, None).await;
        assert_eq!(list["software"][0]["file"]["product_version"], "1.2.0");

        // A command carries the digest and the URLs; only agents that know
        // MSI packages get one.
        let command = json!({"kind": "software",
                             "software": {"action": "install", "catalog_ids": [created["id"]]}});
        let commands = format!("/api/servers/{}/commands", h.server);
        h.kinds(Some(json!({"software": "accepted"}))).await;
        let (status, body) = h.send_with(settings(), "POST", &commands, &admin, Some(command.clone())).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body["error"].as_str().unwrap().contains("too old to install MSI"), "{body}");
        h.kinds(Some(json!({"software": "accepted", "software_msi": "accepted"}))).await;
        let (status, queued) = h.send_with(settings(), "POST", &commands, &admin, Some(command)).await;
        assert_eq!(status, StatusCode::CREATED, "{queued}");
        let change = SoftwareChange::decode(h.payload(&queued).await.as_slice()).unwrap();
        let msi = change.packages[0].msi.as_ref().unwrap();
        assert_eq!(change.packages[0].id, code);
        assert_eq!(msi.sha256, sha256);
        assert_eq!(msi.size, file.len() as u64);
        assert_eq!(msi.urls, [format!("http://mf.example:7480/software/{sha256}.msi")]);
        assert_eq!(msi.properties[0].value, "x");

        // Bounded in all: 20 GiB already stored refuses a new file.
        for i in 0..10 {
            sqlx::query("INSERT INTO software_files (sha256, size) VALUES ($1, 2147483648)")
                .bind(format!("{i:064x}"))
                .execute(&pool)
                .await
                .unwrap();
        }
        let (status, body) = h.upload(settings(), uri, &admin, msi_package(&[("ProductName", "Other")])).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        // The same file again is no new storage.
        let (status, _) = h.upload(settings(), uri, &admin, file.clone()).await;
        assert_eq!(status, StatusCode::CREATED);

        // Named, it stays; once nothing names it, it goes 8 days later.
        let store = monitorflow_server::software_files::Store::in_mirror_dir(dir.path());
        monitorflow_server::software_files::sweep(&pool, &store).await.unwrap();
        assert!(stored.is_file());
        let path = format!("/api/software/{}", created["id"].as_str().unwrap());
        let (status, _) = h.send("DELETE", &path, &admin, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        monitorflow_server::software_files::sweep(&pool, &store).await.unwrap();
        assert!(stored.is_file(), "removed before 8 days");
        sqlx::query("UPDATE software_files SET unreferenced_since = now() - interval '9 days'")
            .execute(&pool)
            .await
            .unwrap();
        let swept = monitorflow_server::software_files::sweep(&pool, &store).await.unwrap();
        assert_eq!(swept.removed, 11);
        assert!(!stored.exists());
        let left: i64 = sqlx::query_scalar("SELECT count(*) FROM software_files")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(left, 0);
    }
}
