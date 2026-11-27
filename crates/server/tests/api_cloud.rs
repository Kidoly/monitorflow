//! Cloud accounts of ADR-0183 against a real PostgreSQL: added as
//! devices with their secret sealed, run by the central server only,
//! their machines kept, matched and read through the API. No cloud is
//! ever called. Ignored without `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use monitorflow_cloud::CloudVm;
use monitorflow_common::proto::v1::SampleBatch;
use monitorflow_common::redact::Secret;
use monitorflow_sensors::virt::{self, Vm};
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users;
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use monitorflow_server::ingest::{check_batch, Identity, Series};
use monitorflow_server::probes::ProbeStore;
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

fn app(pool: &PgPool) -> Router {
    api::router(ApiState::new(
        pool.clone(),
        Registry::default(),
        ApiSettings::default(),
    ))
}

async fn send(pool: &PgPool, request: Request<Body>) -> (StatusCode, Value, Option<String>) {
    let response = app(pool).oneshot(request).await.unwrap();
    let status = response.status();
    let cookie = response
        .headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body, cookie)
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
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("cookie", cookie)
        .header(CSRF_HEADER, "1")
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let (status, body, _) = send(pool, request).await;
    (status, body)
}

async fn site(pool: &PgPool, name: &str) -> Uuid {
    sqlx::query_scalar("INSERT INTO sites (name) VALUES ($1) RETURNING id")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn server(pool: &PgPool, site: Uuid, peer: &str, kind: &str, address: Option<&str>) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family, kind, address, package_updates)
         VALUES ($1, $2, $1, CASE WHEN $3 = 'device' THEN 'device' ELSE 'debian' END, $3, $4, false)
         RETURNING id",
    )
    .bind(peer)
    .bind(site)
    .bind(kind)
    .bind(address)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// What an `aws` check writes for `vms`, through the server's validation.
fn series(peer: &str, vms: &[CloudVm], at: i64) -> Vec<Series> {
    let mut samples = monitorflow_cloud::sensor::samples(vms, at);
    samples.push(virt::up(
        None,
        &[("product", "Amazon EC2 (eu-west-3)"), ("provider", "aws")],
        at,
    ));
    let batch = SampleBatch {
        samples: samples
            .into_iter()
            .map(|mut s| {
                s.sensor = format!("aws_{}", s.sensor);
                monitorflow_probe::sample_to_proto(s)
            })
            .collect(),
    };
    let identity = Identity {
        peer: peer.into(),
        site: "paris".into(),
    };
    let checked = check_batch(&identity, batch, chrono::Utc::now().timestamp_millis());
    assert_eq!(checked.invalid, 0, "{:?}", checked.first_error);
    checked.series
}

const SECRET: &str = "secret-access-key-1";

fn aws() -> Value {
    json!({"provider": "aws", "key_id": "AKIAIOSFODNN7EXAMPLE", "secret": SECRET, "regions": ["eu-west-3"]})
}

db_tests! {
    #[sqlx::test]
    async fn cloud_accounts_keep_their_secret_sealed(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let paris = site(&pool, "paris").await;
        let operator = session(&pool, "operator", Role::Operator, Some(paris)).await;
        let viewer = session(&pool, "viewer", Role::Viewer, Some(paris)).await;
        let body = json!({"site_id": paris, "name": "AWS production", "cloud": aws()});
        let (status, device) = call(&pool, &operator, "POST", "/api/devices", Some(body)).await;
        assert_eq!(status, StatusCode::CREATED, "{device}");
        assert_eq!(device["address"], "ec2.eu-west-3.amazonaws.com");
        let id = device["id"].as_str().unwrap().to_owned();
        let (_, rows) = call(&pool, &viewer, "GET", &format!("/api/servers/{id}/sensor_configs"), None).await;
        let rows = rows["entries"].as_array().unwrap().clone();
        assert_eq!(rows.len(), 1, "no ping for an account: {rows:?}");
        assert_eq!((rows[0]["kind"].as_str(), rows[0]["probe"].as_str()), (Some("aws"), Some("central")));
        assert_eq!(rows[0]["interval_ms"], 300_000);
        let (_, thresholds) = call(&pool, &viewer, "GET", &format!("/api/servers/{id}/thresholds"), None).await;
        let metrics: Vec<&str> = thresholds["entries"].as_array().unwrap().iter().map(|t| t["metric"].as_str().unwrap()).collect();
        assert!(metrics.contains(&"monitorflow_aws_up"), "{metrics:?}");
        assert!(metrics.contains(&"monitorflow_aws_vm_status_checks_failed"), "{metrics:?}");

        let (status, view) = call(&pool, &viewer, "GET", &format!("/api/devices/{id}/cloud"), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(view["key_id"], "AKIAIOSFODNN7EXAMPLE");
        assert_eq!(view["secret_set"], true);
        assert!(!view.to_string().contains(SECRET));
        let stored: String = sqlx::query_scalar("SELECT secret FROM cloud_credentials").fetch_one(&pool).await.unwrap();
        assert!(stored.starts_with("mfs1:") && !stored.contains(SECRET));
        let audit: Vec<String> = sqlx::query_scalar("SELECT details::text FROM audit_log").fetch_all(&pool).await.unwrap();
        assert!(!audit.is_empty() && audit.iter().all(|d| !d.contains(SECRET)), "{audit:?}");

        let sets = ProbeStore::new(pool.clone()).load().await.unwrap();
        let check = sets["central"].checks.iter().find(|c| c.kind == "aws").unwrap();
        assert_eq!(check.config.params["secret"], SECRET);
        assert!(monitorflow_cloud::parse("aws", &check.config.params).is_ok());

        // Regions change without the secret; another key needs its own.
        let uri = format!("/api/devices/{id}/cloud");
        let (status, view) = call(&pool, &operator, "PUT", &uri, Some(json!({"regions": ["eu-west-3", "us-east-1"]}))).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["regions"], json!(["eu-west-3", "us-east-1"]));
        let sets = ProbeStore::new(pool.clone()).load().await.unwrap();
        let check = sets["central"].checks.iter().find(|c| c.kind == "aws").unwrap();
        assert_eq!(check.config.params["secret"], SECRET);
        assert_eq!(check.config.params["regions"], "eu-west-3,us-east-1");
        let (status, error) = call(&pool, &operator, "PUT", &uri, Some(json!({"key_id": "AKIAOTHERKEYEXAMPLE0"}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error["error"].as_str().unwrap().contains("secret"), "{error}");
        let (status, _) = call(&pool, &operator, "PUT", &uri, Some(json!({"provider": "azure"}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(call(&pool, &viewer, "PUT", &uri, Some(json!({"regions": ["eu-west-1"]}))).await.0, StatusCode::FORBIDDEN);

        // Rows hold no credentials, and the kind is the central server's.
        let rows_uri = format!("/api/servers/{id}/sensor_configs");
        let (status, error) = call(&pool, &operator, "POST", &rows_uri,
            Some(json!({"kind": "aws", "name": "aws_2", "interval_ms": 300000, "probe": "central", "params": {"secret": "x"}}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error["error"].as_str().unwrap().contains("/cloud"), "{error}");
        let agent = server(&pool, paris, "web-01", "agent", None).await;
        let (status, _) = call(&pool, &operator, "POST", &format!("/api/servers/{agent}/sensor_configs"),
            Some(json!({"kind": "aws", "name": "aws", "interval_ms": 300000}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // Never from a relay, and never with other device credentials.
        let (status, error) = call(&pool, &operator, "POST", "/api/devices",
            Some(json!({"site_id": paris, "name": "lab", "probe": Uuid::new_v4(), "cloud": aws()}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error["error"].as_str().unwrap().contains("central"), "{error}");
        let (status, _) = call(&pool, &operator, "POST", "/api/devices",
            Some(json!({"site_id": paris, "name": "lab", "cloud": aws(), "vmware": {"user": "u", "password": "p"}}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // A test with credentials that do not read stops before any call.
        let (status, _) = call(&pool, &operator, "POST", "/api/devices/cloud/test",
            Some(json!({"site_id": paris, "cloud": {"provider": "aws", "key_id": "short", "secret": "s", "regions": ["eu-west-3"]}}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(call(&pool, &viewer, "POST", "/api/devices/cloud/test",
            Some(json!({"site_id": paris, "cloud": aws()}))).await.0, StatusCode::FORBIDDEN);

        assert_eq!(call(&pool, &operator, "DELETE", &uri, None).await.0, StatusCode::NO_CONTENT);
        let sets = ProbeStore::new(pool.clone()).load().await.unwrap();
        assert!(!sets.get("central").is_some_and(|s| s.checks.iter().any(|c| c.kind == "aws")));
        assert_eq!(call(&pool, &operator, "DELETE", &uri, None).await.0, StatusCode::NOT_FOUND);
    }

    #[sqlx::test]
    async fn google_cloud_projects_keep_their_key_sealed(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let paris = site(&pool, "paris").await;
        let operator = session(&pool, "operator", Role::Operator, Some(paris)).await;
        let viewer = session(&pool, "viewer", Role::Viewer, Some(paris)).await;
        let key = include_str!("../../cloud/tests/fixtures/gcp_key.json");
        let body = json!({"site_id": paris, "name": "GCP production", "cloud": {"provider": "gcp", "secret": key}});
        let (status, device) = call(&pool, &operator, "POST", "/api/devices", Some(body)).await;
        assert_eq!(status, StatusCode::CREATED, "{device}");
        assert_eq!(device["address"], "compute.googleapis.com");
        let id = device["id"].as_str().unwrap().to_owned();
        let (_, thresholds) = call(&pool, &viewer, "GET", &format!("/api/servers/{id}/thresholds"), None).await;
        let metrics: Vec<&str> = thresholds["entries"].as_array().unwrap().iter().map(|t| t["metric"].as_str().unwrap()).collect();
        assert_eq!(metrics, ["monitorflow_gcp_up"]);

        let uri = format!("/api/devices/{id}/cloud");
        let (status, view) = call(&pool, &viewer, "GET", &uri, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(view["key_id"], "reader@example-project.iam.gserviceaccount.com");
        assert_eq!(view["project"], "example-project");
        assert_eq!(view["secret_set"], true);
        assert!(!view.to_string().contains("PRIVATE KEY"));
        let stored: String = sqlx::query_scalar("SELECT secret FROM cloud_credentials").fetch_one(&pool).await.unwrap();
        assert!(stored.starts_with("mfs1:") && !stored.contains("PRIVATE KEY"));
        let audit: Vec<String> = sqlx::query_scalar("SELECT details::text FROM audit_log").fetch_all(&pool).await.unwrap();
        assert!(!audit.is_empty() && audit.iter().all(|d| !d.contains("PRIVATE KEY")), "{audit:?}");

        // Another project keeps the key; the store hands it to the check.
        let (status, view) = call(&pool, &operator, "PUT", &uri, Some(json!({"project": "another-project"}))).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["project"], "another-project");
        let sets = ProbeStore::new(pool.clone()).load().await.unwrap();
        let check = sets["central"].checks.iter().find(|c| c.kind == "gcp").unwrap();
        assert_eq!(check.config.params["project"], "another-project");
        assert!(monitorflow_cloud::parse("gcp", &check.config.params).is_ok());
        let (status, error) = call(&pool, &operator, "PUT", &uri,
            Some(json!({"key_id": "other@example-project.iam.gserviceaccount.com"}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error["error"].as_str().unwrap().contains("secret"), "{error}");
        let (status, error) = call(&pool, &operator, "POST", "/api/devices",
            Some(json!({"site_id": paris, "name": "bad", "cloud": {"provider": "gcp", "secret": "{}"}}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    }

    #[sqlx::test]
    async fn cloud_machines_join_the_inventory(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let viewer = session(&pool, "viewer", Role::Viewer, Some(paris)).await;
        let account = server(&pool, paris, "aws-production", "device", Some("ec2.eu-west-3.amazonaws.com")).await;
        let web = server(&pool, paris, "web-01", "agent", None).await;
        sqlx::query("UPDATE servers SET mac_addresses = '{0a:1b:2c:3d:4e:5f}' WHERE id = $1")
            .bind(web).execute(&pool).await.unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        let vms = [
            CloudVm {
                vm: Vm { key: "i-0123456789abcdef0".into(), name: "web".into(), host: Some("eu-west-3a".into()),
                         power_state: "running".into(), powered_on: true,
                         macs: vec!["0a:1b:2c:3d:4e:5f".into()], ..Vm::default() },
                size: Some("t3.micro".into()),
                measures: vec![("cpu_usage_ratio", 0.25), ("network_in_bytes_per_second", 1200.0), ("status_checks_failed", 0.0)],
            },
            CloudVm {
                vm: Vm { key: "i-0fedcba9876543210".into(), name: "batch".into(), power_state: "stopped".into(), ..Vm::default() },
                size: None,
                measures: vec![],
            },
        ];
        monitorflow_server::virt::apply(&pool, &[series("aws-production", &vms, now)]).await.unwrap();
        let (status, inventory) = call(&pool, &viewer, "GET", &format!("/api/servers/{account}/virtualization"), None).await;
        assert_eq!(status, StatusCode::OK, "{inventory}");
        assert_eq!(inventory["source"]["details"]["provider"], "aws");
        let objects = inventory["objects"].as_array().unwrap();
        let machine = objects.iter().find(|o| o["name"] == "web").unwrap();
        assert_eq!(machine["key"], "i-0123456789abcdef0");
        assert_eq!(machine["details"]["size"], "t3.micro");
        assert_eq!(machine["metrics"]["network_in_bytes_per_second"], 1200.0);
        assert_eq!(machine["metrics"]["status_checks_failed"], 0.0);
        assert!(machine["metrics"].get("snapshots").is_none());
        assert_eq!(machine["server"]["id"], web.to_string());
        assert_eq!(machine["matched_by"], "mac");
        // The server's page names the account its machine is in.
        let (_, guest) = call(&pool, &viewer, "GET", &format!("/api/servers/{web}/virtualization"), None).await;
        assert_eq!(guest["guest_of"].as_array().unwrap().len(), 1, "{guest}");
    }
}
