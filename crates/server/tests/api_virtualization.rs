//! The virtual inventory of ADR-0156 against a real PostgreSQL: samples
//! of a vCenter and a Hyper-V host written, matched to servers of their
//! customer only, and read through the API. Ignored without
//! `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use monitorflow_common::proto::v1::SampleBatch;
use monitorflow_common::redact::Secret;
use monitorflow_sensors::virt::{self, Datastore, Host, Vm};
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users;
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use monitorflow_server::ingest::{check_batch, Identity, Series};
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

/// What a check named `check` of `peer` writes, through the server's
/// validation as a probe's or an agent's samples go.
fn series(
    peer: &str,
    check: &str,
    vms: &[Vm],
    hosts: &[Host],
    datastores: &[Datastore],
    at: i64,
) -> Vec<Series> {
    let mut samples = virt::samples(vms, hosts, &[], datastores, at);
    samples.push(virt::up(
        None,
        &[("product", "VMware vCenter Server 8.0.2")],
        at,
    ));
    let batch = SampleBatch {
        samples: samples
            .into_iter()
            .map(|mut s| {
                s.sensor = format!("{check}_{}", s.sensor);
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

const VM_UUID: &str = "63d40cc5-9cba-5cc1-884a-f7f19070ecea";
/// The same UUID with its first three fields byte-swapped.
const AGENT_UUID: &str = "c50cd463-ba9c-c15c-884a-f7f19070ecea";

fn vm(key: &str, name: &str) -> Vm {
    Vm {
        key: key.into(),
        name: name.into(),
        power_state: "poweredOn".into(),
        powered_on: true,
        host: Some("esx-01".into()),
        ..Vm::default()
    }
}

db_tests! {
    #[sqlx::test]
    async fn an_inventory_is_kept_matched_and_read(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let other: Uuid = sqlx::query_scalar("INSERT INTO customers (name) VALUES ('other') RETURNING id")
            .fetch_one(&pool).await.unwrap();
        let elsewhere: Uuid = sqlx::query_scalar("INSERT INTO sites (name, customer_id) VALUES ('elsewhere', $1) RETURNING id")
            .bind(other).fetch_one(&pool).await.unwrap();
        let vcenter = server(&pool, paris, "vcenter-1", "device", Some("192.0.2.50")).await;
        let web = server(&pool, paris, "web-01", "agent", None).await;
        sqlx::query("UPDATE servers SET system_uuid = $2 WHERE id = $1")
            .bind(web).bind(Uuid::parse_str(AGENT_UUID).unwrap()).execute(&pool).await.unwrap();
        let db = server(&pool, paris, "db-01", "agent", None).await;
        sqlx::query("UPDATE servers SET mac_addresses = '{00:50:56:00:00:02}' WHERE id = $1")
            .bind(db).execute(&pool).await.unwrap();
        let appliance = server(&pool, paris, "appliance", "device", Some("192.0.2.77")).await;
        // Another customer's server of the same name is never matched.
        server(&pool, elsewhere, "files", "agent", None).await;

        let now = chrono::Utc::now().timestamp_millis();
        let vms = vec![
            Vm { uuid: Some(VM_UUID.into()), snapshots: vec![now - 8 * 86_400_000], ..vm("vm-1", "web-01-vm") },
            Vm { macs: vec!["00:50:56:00:00:02".into()], ..vm("vm-2", "database") },
            Vm { addresses: vec!["192.0.2.77".into()], ..vm("vm-3", "appliance-vm") },
            vm("vm-4", "files"),
        ];
        let hosts = vec![Host { key: "host-1".into(), name: "esx-01".into(), connection_state: "disconnected".into(), ..Host::default() }];
        let datastores = vec![Datastore { key: "ds-1".into(), name: "ds1".into(), accessible: true,
            capacity_bytes: Some(100.0), free_bytes: Some(2.0), ..Datastore::default() }];
        let first = series("vcenter-1", "vmware", &vms, &hosts, &datastores, now - 1000);
        // A large collection comes in several batches.
        let (a, b) = first.split_at(first.len() / 2);
        monitorflow_server::virt::apply(&pool, &[a.to_vec(), b.to_vec()]).await.unwrap();

        let links: Vec<(String, Option<Uuid>, Option<String>)> = sqlx::query_as(
            "SELECT instance, server_id, matched_by FROM virtual_objects WHERE kind = 'vm' ORDER BY instance")
            .fetch_all(&pool).await.unwrap();
        assert_eq!(links, vec![
            ("appliance-vm".into(), Some(appliance), Some("address".into())),
            ("database".into(), Some(db), Some("mac".into())),
            ("files".into(), None, None),
            ("web-01-vm".into(), Some(web), Some("uuid".into())),
        ]);

        let admin = session(&pool, "admin", Role::Administrator, None).await;
        let (status, body) = call(&pool, &admin, "GET", &format!("/api/servers/{vcenter}/virtualization"), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["source"]["up"], true);
        assert_eq!(body["source"]["details"]["product"], "VMware vCenter Server 8.0.2");
        let objects = body["objects"].as_array().unwrap();
        assert_eq!(objects.len(), 6);
        assert_eq!(objects[0]["kind"], "host");
        assert_eq!(objects[0]["metrics"]["connected"], 0.0);
        let web_vm = objects.iter().find(|o| o["name"] == "web-01-vm").unwrap();
        assert_eq!(web_vm["metrics"]["snapshots"], 1.0);
        assert!(web_vm["metrics"]["snapshot_age_seconds"].as_f64().unwrap() >= 7.9 * 86_400.0);
        assert_eq!(web_vm["server"]["hostname"], "web-01");
        assert_eq!(web_vm["details"]["key"], Value::Null, "the key is a field of its own");
        assert_eq!(web_vm["key"], "vm-1");
        let ds = objects.iter().find(|o| o["kind"] == "datastore").unwrap();
        assert_eq!(ds["metrics"]["free_ratio"], 0.02);

        let (_, page) = call(&pool, &admin, "GET", &format!("/api/servers/{web}/virtualization"), None).await;
        assert_eq!(page["source"], Value::Null);
        assert_eq!(page["guest_of"][0]["source_name"], "vcenter-1");
        assert_eq!(page["guest_of"][0]["object"]["details"]["host"], "esx-01");

        // The next collection replaces the numbers: the snapshot is gone.
        let vms = vec![Vm { uuid: Some(VM_UUID.into()), ..vm("vm-1", "web-01-vm") }];
        monitorflow_server::virt::apply(&pool, &[series("vcenter-1", "vmware", &vms, &[], &[], now)]).await.unwrap();
        let metrics: Value = sqlx::query_scalar("SELECT metrics FROM virtual_objects WHERE instance = 'web-01-vm'")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(metrics["snapshots"], 0.0);
        assert!(metrics.get("snapshot_age_seconds").is_none(), "{metrics}");

        // A viewer of another site sees the inventory of their own only.
        let stranger = session(&pool, "stranger", Role::Viewer, Some(elsewhere)).await;
        let (status, _) = call(&pool, &stranger, "GET", &format!("/api/servers/{vcenter}/virtualization"), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[sqlx::test]
    async fn a_failing_source_says_why(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let host = server(&pool, paris, "hv-01", "agent", None).await;
        let up = virt::up(Some(("denied", monitorflow_sensors::hyperv::DENIED)), &[], chrono::Utc::now().timestamp_millis());
        let mut sample = monitorflow_probe::sample_to_proto(up);
        sample.sensor = "hyperv_up".into();
        let identity = Identity { peer: "hv-01".into(), site: "paris".into() };
        let checked = check_batch(&identity, SampleBatch { samples: vec![sample] }, chrono::Utc::now().timestamp_millis());
        monitorflow_server::virt::apply(&pool, &[checked.series]).await.unwrap();
        let admin = session(&pool, "admin", Role::Administrator, None).await;
        let (_, body) = call(&pool, &admin, "GET", &format!("/api/servers/{host}/virtualization"), None).await;
        assert_eq!(body["source"]["up"], false);
        assert_eq!(body["source"]["reason"], "denied");
        assert!(body["source"]["error"].as_str().unwrap().contains("Hyper-V Administrators"));
    }
}
