//! The hardware and software inventory of ADR-0187 against a real
//! PostgreSQL: reports stored whole, confirmed by their hash and changed
//! by deltas, refused with a request when the server cannot use them,
//! package changes in the history, and the API: a server's inventory,
//! its changes, the warranty and the fleet search with versions and CSV,
//! within the caller's sites. Ignored without `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use monitorflow_common::inventory::{self, Items};
use monitorflow_common::proto::v1::{
    inventory_report, Inventory, InventoryReport, Package, PackageReport,
};
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users;
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::commands::PackageStore;
use monitorflow_server::grpc::Registry;
use monitorflow_server::server_inventory::{record, Outcome};
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

async fn send(pool: &PgPool, request: Request<Body>) -> (StatusCode, Vec<u8>, Option<String>) {
    let app = api::router(ApiState::new(
        pool.clone(),
        Registry::default(),
        ApiSettings::default(),
    ));
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let cookie = response
        .headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 22)
        .await
        .unwrap();
    (status, bytes.to_vec(), cookie)
}

async fn session(pool: &PgPool, username: &str, role: Role, site: Option<Uuid>) -> String {
    let id = users::create_user(pool, username, &Secret::new(PASSWORD), role, "test")
        .await
        .unwrap();
    sqlx::query("UPDATE user_roles SET site_id = $2 WHERE user_id = $1")
        .bind(id)
        .bind(site)
        .execute(pool)
        .await
        .unwrap();
    let body = json!({ "username": username, "password": PASSWORD }).to_string();
    let request = Request::post("/api/auth/login")
        .header("content-type", "application/json")
        .header(CSRF_HEADER, "1")
        .body(Body::from(body))
        .unwrap();
    let (status, _, cookie) = send(pool, request).await;
    assert_eq!(status, StatusCode::OK);
    cookie.unwrap()
}

async fn call(
    pool: &PgPool,
    cookie: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let (status, bytes) = raw(pool, cookie, method, uri, body).await;
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
}

async fn raw(
    pool: &PgPool,
    cookie: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Vec<u8>) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("cookie", cookie)
        .header(CSRF_HEADER, "1")
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let (status, bytes, _) = send(pool, request).await;
    (status, bytes)
}

async fn site(pool: &PgPool, name: &str) -> Uuid {
    sqlx::query_scalar("INSERT INTO sites (name) VALUES ($1) RETURNING id")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn server(pool: &PgPool, site: Uuid, peer: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family)
         VALUES ($1, $2, $1, 'debian') RETURNING id",
    )
    .bind(peer)
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap()
}

fn machine(disk_model: &str, program_version: &str) -> Inventory {
    let mut items = Items::new();
    items.push(
        "system",
        "system",
        [
            ("manufacturer", "Dell Inc.".to_owned()),
            ("model", "PowerEdge R650".to_owned()),
            ("serial", "7XK2LM3".to_owned()),
            ("bios_version", "1.10.2".to_owned()),
        ],
    );
    items.push(
        "disk",
        "sda",
        [
            ("model", disk_model.to_owned()),
            ("firmware", "HXT7404Q".to_owned()),
        ],
    );
    items.push(
        "software",
        "7-Zip 24.08 (x64)",
        [("version", program_version.to_owned())],
    );
    items.push(
        "service",
        "nginx.service",
        [("start", "enabled".to_owned())],
    );
    items.finish()
}

fn full(inventory: &Inventory) -> InventoryReport {
    InventoryReport {
        collected_at_unix_ms: chrono::Utc::now().timestamp_millis(),
        hash: inventory::hash(inventory),
        body: Some(inventory_report::Body::Full(inventory.clone())),
    }
}

fn hash_only(inventory: &Inventory) -> InventoryReport {
    InventoryReport {
        body: None,
        ..full(inventory)
    }
}

fn delta(from: &Inventory, to: &Inventory) -> InventoryReport {
    InventoryReport {
        hash: inventory::hash(to),
        body: Some(inventory_report::Body::Delta(inventory::delta(from, to))),
        ..full(to)
    }
}

async fn changes(pool: &PgPool, server: Uuid) -> Vec<(String, String, String)> {
    sqlx::query_as(
        "SELECT kind, key, change FROM server_inventory_changes WHERE server_id = $1 ORDER BY id",
    )
    .bind(server)
    .fetch_all(pool)
    .await
    .unwrap()
}

fn package(name: &str, version: &str) -> Package {
    Package {
        name: name.into(),
        version: version.into(),
        arch: "amd64".into(),
        ..Package::default()
    }
}

db_tests! {
    #[sqlx::test]
    async fn reports_are_stored_confirmed_and_changed(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let web = server(&pool, paris, "web-1").await;
        let first = machine("SAMSUNG MZ7LH480", "23.01");

        // Unknown agents are ignored; a hash the server does not hold is
        // asked for with nothing known.
        assert_eq!(record(&pool, "nobody", hash_only(&first)).await.unwrap(), Outcome::UnknownServer);
        assert_eq!(record(&pool, "web-1", hash_only(&first)).await.unwrap(), Outcome::Want(Vec::new()));

        // The first inventory is stored, with no changes recorded.
        assert_eq!(
            record(&pool, "web-1", full(&first)).await.unwrap(),
            Outcome::Stored { changes: 4 }
        );
        assert!(changes(&pool, web).await.is_empty());
        assert_eq!(record(&pool, "web-1", hash_only(&first)).await.unwrap(), Outcome::Confirmed);

        // A delta from the stored inventory is applied and recorded.
        let second = machine("SAMSUNG MZ7LH480", "24.08");
        assert_eq!(
            record(&pool, "web-1", delta(&first, &second)).await.unwrap(),
            Outcome::Stored { changes: 1 }
        );
        assert_eq!(
            changes(&pool, web).await,
            [("software".to_owned(), "7-Zip 24.08 (x64)".to_owned(), "changed".to_owned())]
        );

        // A delta from what the server no longer holds is refused with the
        // hash it holds; a delta whose result does not match its hash
        // asks for the whole inventory.
        let third = machine("WDC WD40EFRX", "24.08");
        assert_eq!(
            record(&pool, "web-1", delta(&first, &third)).await.unwrap(),
            Outcome::Want(inventory::hash(&second))
        );
        let mut lying = delta(&second, &third);
        lying.hash = inventory::hash(&first);
        assert_eq!(record(&pool, "web-1", lying).await.unwrap(), Outcome::Want(Vec::new()));

        // A whole inventory replaces the rows; unknown kinds are dropped.
        let mut odd = third.clone();
        odd.items.push(monitorflow_common::proto::v1::InventoryItem {
            kind: "gadget".into(),
            key: "x".into(),
            fields: Vec::new(),
        });
        assert_eq!(
            record(&pool, "web-1", full(&odd)).await.unwrap(),
            Outcome::Stored { changes: 1 }
        );
        let stored: Vec<(String,)> = sqlx::query_as(
            "SELECT kind FROM server_inventory_items WHERE server_id = $1 ORDER BY kind",
        )
        .bind(web)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(stored.len(), 4);
        assert_eq!(record(&pool, "web-1", hash_only(&third)).await.unwrap(), Outcome::Confirmed);
    }

    #[sqlx::test]
    async fn package_changes_go_in_the_history(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let web = server(&pool, paris, "web-1").await;
        let store = PackageStore::new(pool.clone());
        let report = |packages: Vec<Package>, at: i64| PackageReport {
            packages,
            collected_at_unix_ms: at,
            ..PackageReport::default()
        };
        let now = chrono::Utc::now().timestamp_millis();
        store
            .replace("web-1", &report(vec![package("openssl", "3.0.14-1"), package("curl", "7.88")], now - 2000))
            .await
            .unwrap();
        assert!(changes(&pool, web).await.is_empty(), "the first list is no change");
        store
            .replace("web-1", &report(vec![package("openssl", "3.0.15-1"), package("vim", "9.0")], now - 1000))
            .await
            .unwrap();
        assert_eq!(
            changes(&pool, web).await,
            [
                ("package".to_owned(), "curl".to_owned(), "removed".to_owned()),
                ("package".to_owned(), "openssl".to_owned(), "changed".to_owned()),
                ("package".to_owned(), "vim".to_owned(), "added".to_owned()),
            ]
        );
    }

    #[sqlx::test]
    async fn the_api_reads_searches_and_exports_within_reach(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let lyon = site(&pool, "lyon").await;
        let web = server(&pool, paris, "web-1").await;
        let db = server(&pool, lyon, "db-1").await;
        record(&pool, "web-1", full(&machine("SAMSUNG MZ7LH480", "23.01"))).await.unwrap();
        record(&pool, "db-1", full(&machine("WDC WD40EFRX", "24.08"))).await.unwrap();
        PackageStore::new(pool.clone())
            .replace("web-1", &PackageReport {
                packages: vec![package("openssl", "3.0.14-1~deb12u2"), package("libssl3", "3.0.14-1~deb12u2")],
                ..PackageReport::default()
            })
            .await
            .unwrap();
        PackageStore::new(pool.clone())
            .replace("db-1", &PackageReport {
                packages: vec![package("openssl", "3.0.15-1~deb12u1")],
                ..PackageReport::default()
            })
            .await
            .unwrap();

        let admin = session(&pool, "admin", Role::Administrator, None).await;
        let (status, body) = call(&pool, &admin, "GET", &format!("/api/servers/{web}/inventory"), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["items"][0]["kind"], "system");
        assert_eq!(body["asset"]["serial"], "7XK2LM3");
        assert_eq!(body["package_count"], 2);
        assert!(body["reported"]["checked_at"].is_string());
        assert!(body["warranty"].is_null());

        // Which servers have OpenSSL below 3.0.15?
        let (status, body) = call(
            &pool, &admin, "GET",
            "/api/inventory?kind=software&q=openssl&exact=true&version_below=3.0.15", None,
        ).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let hits = body["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 1, "{body}");
        assert_eq!(hits[0]["hostname"], "web-1");
        assert_eq!(hits[0]["source"], "package");

        // Windows programs are software too.
        let (_, body) = call(&pool, &admin, "GET", "/api/inventory?q=7-zip&version_at_least=24", None).await;
        assert_eq!(body["hits"].as_array().unwrap().len(), 1, "{body}");
        assert_eq!(body["hits"][0]["source"], "program");

        // Which disk models? Every field matches.
        let (_, body) = call(&pool, &admin, "GET", "/api/inventory?kind=disk&q=wdc", None).await;
        assert_eq!(body["hits"][0]["hostname"], "db-1", "{body}");
        let (status, _) = call(&pool, &admin, "GET", "/api/inventory?kind=software&q=o", None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = call(&pool, &admin, "GET", "/api/inventory?kind=gadget", None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // CSV.
        let (status, csv) = raw(&pool, &admin, "GET", "/api/inventory?kind=disk&format=csv", None).await;
        assert_eq!(status, StatusCode::OK);
        let csv = String::from_utf8(csv).unwrap();
        assert!(csv.starts_with("hostname,site,source,name,version,details\n"), "{csv}");
        assert!(csv.contains("db-1,lyon,disk,sda,HXT7404Q,"), "{csv}");

        // A viewer of Paris sees Paris only, and may not set a warranty.
        let viewer = session(&pool, "viewer", Role::Viewer, Some(paris)).await;
        let (_, body) = call(&pool, &viewer, "GET", "/api/inventory?q=openssl", None).await;
        let hosts: Vec<&str> = body["hits"].as_array().unwrap().iter().map(|h| h["hostname"].as_str().unwrap()).collect();
        assert_eq!(hosts, ["web-1"]);
        let (status, _) = call(&pool, &viewer, "GET", &format!("/api/servers/{db}/inventory"), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let warranty = json!({"ends_on": "2028-03-31", "note": "ProSupport"});
        let (status, _) = call(&pool, &viewer, "PUT", &format!("/api/servers/{web}/warranty"), Some(warranty.clone())).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // An operator sets it, audited, and removes it.
        let operator = session(&pool, "operator", Role::Operator, Some(paris)).await;
        let (status, body) = call(&pool, &operator, "PUT", &format!("/api/servers/{web}/warranty"), Some(warranty)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["ends_on"], "2028-03-31");
        let (_, body) = call(&pool, &admin, "GET", &format!("/api/servers/{web}/inventory"), None).await;
        assert_eq!(body["warranty"]["note"], "ProSupport");
        let audited: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'server.warranty'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(audited, 1);
        let (status, body) = call(
            &pool, &operator, "PUT", &format!("/api/servers/{web}/warranty"),
            Some(json!({"ends_on": null, "note": "  "})),
        ).await;
        assert_eq!((status, body), (StatusCode::OK, Value::Null));

        // The history pages newest first.
        record(&pool, "web-1", full(&machine("SAMSUNG MZ7LH480", "24.08"))).await.unwrap();
        record(&pool, "web-1", full(&machine("WDC WD40EFRX", "24.08"))).await.unwrap();
        let (_, body) = call(&pool, &admin, "GET", &format!("/api/servers/{web}/inventory/changes?limit=1"), None).await;
        assert_eq!(body["changes"][0]["kind"], "disk", "{body}");
        assert_eq!(body["changes"][0]["after"]["model"], "WDC WD40EFRX");
        let cursor = body["next_cursor"].as_str().unwrap().to_owned();
        let (_, body) = call(
            &pool, &admin, "GET",
            &format!("/api/servers/{web}/inventory/changes?limit=1&cursor={cursor}"), None,
        ).await;
        assert_eq!(body["changes"][0]["kind"], "software", "{body}");
    }
}
