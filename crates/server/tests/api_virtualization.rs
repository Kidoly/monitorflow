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

    /// vCenter devices: credentials sealed and never shown, the check and
    /// alerts added, probes given the password, rows without credentials.
    #[sqlx::test]
    async fn vcenter_devices_keep_their_password_sealed(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let paris = site(&pool, "paris").await;
        let operator = session(&pool, "op", Role::Operator, Some(paris)).await;
        let viewer = session(&pool, "viewer", Role::Viewer, Some(paris)).await;
        let print = "ab".repeat(32);
        let body = json!({"site_id": paris, "name": "vcenter-1", "address": "192.0.2.40", "ping": false,
                          "vmware": {"user": "monitor@vsphere.local", "password": "secret-vc-1", "fingerprint": print}});
        let (status, device) = call(&pool, &operator, "POST", "/api/devices", Some(body)).await;
        assert_eq!(status, StatusCode::CREATED, "{device}");
        let id = device["id"].as_str().unwrap().to_owned();
        let (_, rows) = call(&pool, &viewer, "GET", &format!("/api/servers/{id}/sensor_configs"), None).await;
        let rows = rows["entries"].as_array().unwrap().clone();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0]["kind"].as_str(), rows[0]["name"].as_str()), (Some("vmware"), Some("vmware")));
        assert_eq!(rows[0]["interval_ms"], 300_000);
        let (_, thresholds) = call(&pool, &viewer, "GET", &format!("/api/servers/{id}/thresholds"), None).await;
        let metrics: Vec<String> = thresholds["entries"].as_array().unwrap().iter().map(|t| t["metric"].as_str().unwrap().to_owned()).collect();
        for wanted in ["monitorflow_vmware_host_connected", "monitorflow_vmware_datastore_free_ratio", "monitorflow_vmware_vm_snapshot_age_seconds"] {
            assert!(metrics.contains(&wanted.to_string()), "{metrics:?}");
        }

        let (status, view) = call(&pool, &viewer, "GET", &format!("/api/devices/{id}/vmware"), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(view["user"], "monitor@vsphere.local");
        assert_eq!(view["password_set"], true);
        assert!(view["fingerprint"].as_str().unwrap().starts_with("AB:AB:"));
        let stored: String = sqlx::query_scalar("SELECT secret FROM vmware_credentials").fetch_one(&pool).await.unwrap();
        assert!(stored.starts_with("mfs1:") && !stored.contains("secret-vc"));
        let audit: Vec<String> = sqlx::query_scalar("SELECT details::text FROM audit_log").fetch_all(&pool).await.unwrap();
        assert!(!audit.is_empty() && audit.iter().all(|d| !d.contains("secret-vc")), "{audit:?}");

        let sets = ProbeStore::new(pool.clone()).load().await.unwrap();
        let check = sets["central"].checks.iter().find(|c| c.kind == "vmware").unwrap();
        assert_eq!(check.config.params["password"], "secret-vc-1");
        assert_eq!(check.config.params["host"], "192.0.2.40");
        assert!(monitorflow_vmware::parse(&check.config.params).is_ok());

        // The fingerprint is forgotten without typing the password again.
        let (status, view) = call(&pool, &operator, "PUT", &format!("/api/devices/{id}/vmware"), Some(json!({"fingerprint": ""}))).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["fingerprint"], Value::Null);
        let sets = ProbeStore::new(pool.clone()).load().await.unwrap();
        let check = sets["central"].checks.iter().find(|c| c.kind == "vmware").unwrap();
        assert_eq!(check.config.params["password"], "secret-vc-1");
        assert!(!check.config.params.contains_key("fingerprint"));
        assert_eq!(call(&pool, &viewer, "PUT", &format!("/api/devices/{id}/vmware"), Some(json!({"port": 8443}))).await.0, StatusCode::FORBIDDEN);

        // Rows hold no credentials, and the kind is for devices.
        let (status, error) = call(&pool, &operator, "POST", &format!("/api/servers/{id}/sensor_configs"),
            Some(json!({"kind": "vmware", "name": "vmware_2", "interval_ms": 300000, "params": {"user": "x"}}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error["error"].as_str().unwrap().contains("/vmware"), "{error}");
        let (status, error) = call(&pool, &operator, "POST", &format!("/api/servers/{id}/sensor_configs"),
            Some(json!({"kind": "vmware", "name": "vmware_2", "interval_ms": 300000, "probe": "central", "params": {"host": "198.51.100.7"}}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error["error"].as_str().unwrap().contains("host"), "{error}");
        let agent = server(&pool, paris, "web-01", "agent", None).await;
        let (status, _) = call(&pool, &operator, "POST", &format!("/api/servers/{agent}/sensor_configs"),
            Some(json!({"kind": "vmware", "name": "vmware", "interval_ms": 300000, "probe": "central", "params": {"host": "vc"}}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        for bad in [
            json!({"site_id": paris, "name": "x", "address": "192.0.2.41", "vmware": {"user": "u"}}),
            json!({"site_id": paris, "name": "x", "address": "192.0.2.41", "vmware": {"user": "u", "password": "p", "fingerprint": "zz"}}),
        ] {
            assert_eq!(call(&pool, &operator, "POST", "/api/devices", Some(bad)).await.0, StatusCode::BAD_REQUEST);
        }

        // Credentials set later on a plain device add the check once.
        let (_, nas) = call(&pool, &operator, "POST", "/api/devices", Some(json!({"site_id": paris, "name": "esx-2", "address": "192.0.2.42"}))).await;
        let nas = nas["id"].as_str().unwrap().to_owned();
        let (status, _) = call(&pool, &operator, "PUT", &format!("/api/devices/{nas}/vmware"), Some(json!({"user": "root", "password": "secret-esx"}))).await;
        assert_eq!(status, StatusCode::OK);
        call(&pool, &operator, "PUT", &format!("/api/devices/{nas}/vmware"), Some(json!({"port": 8443}))).await;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM sensor_configs WHERE server_id = $1::uuid AND kind = 'vmware'")
            .bind(&nas).fetch_one(&pool).await.unwrap();
        assert_eq!(count, 1);

        assert_eq!(call(&pool, &operator, "DELETE", &format!("/api/devices/{id}/vmware"), None).await.0, StatusCode::NO_CONTENT);
        let sets = ProbeStore::new(pool.clone()).load().await.unwrap();
        assert!(sets["central"].checks.iter().all(|c| c.kind != "vmware" || c.device != "vcenter-1"));
    }

    #[sqlx::test]
    async fn hyperv_is_watched_on_windows_servers(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let operator = session(&pool, "op", Role::Operator, Some(paris)).await;
        let windows = server(&pool, paris, "hv-01", "agent", None).await;
        sqlx::query("UPDATE servers SET os_family = 'windows' WHERE id = $1").bind(windows).execute(&pool).await.unwrap();
        let linux = server(&pool, paris, "web-01", "agent", None).await;
        let (status, body) = call(&pool, &operator, "POST", &format!("/api/servers/{windows}/hyperv"), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"added": true, "check": "hyperv"}));
        let (_, again) = call(&pool, &operator, "POST", &format!("/api/servers/{windows}/hyperv"), None).await;
        assert_eq!(again["added"], false);
        let (probe, kind): (Option<String>, String) = sqlx::query_as("SELECT probe, kind FROM sensor_configs WHERE server_id = $1")
            .bind(windows).fetch_one(&pool).await.unwrap();
        assert_eq!((probe, kind.as_str()), (None, "hyperv"), "the agent runs it");
        let thresholds: i64 = sqlx::query_scalar("SELECT count(*) FROM thresholds WHERE server_id = $1")
            .bind(windows).fetch_one(&pool).await.unwrap();
        assert_eq!(thresholds, 3);
        assert_eq!(call(&pool, &operator, "POST", &format!("/api/servers/{linux}/hyperv"), None).await.0, StatusCode::BAD_REQUEST);
        let audited: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'server.hyperv_watch'")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(audited, 2);
    }

    /// The connection test from the central server against vcsim, when
    /// `MF_VCSIM` names one (deploy/vmware-test/run-e2e.sh starts it).
    #[sqlx::test]
    async fn a_vcenter_test_shows_its_certificate_before_logging_in(pool: PgPool) {
        let Ok(target) = std::env::var("MF_VCSIM") else {
            eprintln!("MF_VCSIM is not set; skipped");
            return;
        };
        let (host, port) = target.rsplit_once(':').unwrap();
        let port: u16 = port.parse().unwrap();
        let paris = site(&pool, "paris").await;
        let operator = session(&pool, "op", Role::Operator, Some(paris)).await;
        let credentials = json!({"port": port, "user": "reader", "password": "reader-pass"});
        let (status, first) = call(&pool, &operator, "POST", "/api/devices/vmware/test",
            Some(json!({"site_id": paris, "address": host, "probe": "central", "vmware": credentials}))).await;
        assert_eq!(status, StatusCode::OK, "{first}");
        assert_eq!(first["answers"], true);
        assert_eq!(first["api_type"], "VirtualCenter");
        assert_eq!(first["certificate"]["trusted"], false);
        assert_eq!(first["login"], Value::Null, "{first}");
        let print = first["certificate"]["fingerprint"].as_str().unwrap();
        let mut pinned = credentials.clone();
        pinned["fingerprint"] = json!(print);
        let (_, second) = call(&pool, &operator, "POST", "/api/devices/vmware/test",
            Some(json!({"site_id": paris, "address": host, "probe": "central", "vmware": pinned}))).await;
        assert_eq!(second["login"], true, "{second}");
        assert!(second["objects"]["vm"].as_u64().unwrap() >= 4);
        pinned["password"] = json!("wrong password");
        let (_, refused) = call(&pool, &operator, "POST", "/api/devices/vmware/test",
            Some(json!({"site_id": paris, "address": host, "probe": "central", "vmware": pinned}))).await;
        assert_eq!(refused["login"], false);
        assert!(refused["error"].as_str().is_some(), "{refused}");
    }

    /// A person corrects a match, which matching then keeps, and gives a
    /// virtual machine without an agent a server waiting for it.
    #[sqlx::test]
    async fn people_tie_virtual_machines_and_install_the_agent(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let lyon = site(&pool, "lyon").await;
        server(&pool, paris, "vcenter-1", "device", Some("192.0.2.50")).await;
        let web = server(&pool, paris, "web-01", "agent", None).await;
        let other = server(&pool, lyon, "web-02", "agent", None).await;
        let now = chrono::Utc::now().timestamp_millis();
        let vms = vec![
            vm("vm-1", "web-01"),
            Vm { guest_hostname: Some("build-7.corp.example".into()), guest_os: Some("Microsoft Windows Server 2022 (64-bit)".into()), ..vm("vm-2", "build") },
        ];
        monitorflow_server::virt::apply(&pool, &[series("vcenter-1", "vmware", &vms, &[], &[], now)]).await.unwrap();
        let ids: Vec<(String, Uuid)> = sqlx::query_as("SELECT instance, id FROM virtual_objects ORDER BY instance")
            .fetch_all(&pool).await.unwrap();
        let (build, matched) = (ids[0].1, ids[1].1);

        let operator = session(&pool, "op", Role::Operator, Some(paris)).await;
        // Matched by name; a person says it is none, which sticks.
        let (status, view) = call(&pool, &operator, "PUT", &format!("/api/virtual-objects/{matched}/server"), Some(json!({"server_id": null}))).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!((view["server"].clone(), view["link_fixed"].clone()), (Value::Null, json!(true)));
        monitorflow_server::virt::apply(&pool, &[series("vcenter-1", "vmware", &vms, &[], &[], now + 1000)]).await.unwrap();
        let kept: Option<Uuid> = sqlx::query_scalar("SELECT server_id FROM virtual_objects WHERE id = $1").bind(matched).fetch_one(&pool).await.unwrap();
        assert_eq!(kept, None);
        // A server the operator cannot read is refused; back to automatic.
        let (status, _) = call(&pool, &operator, "PUT", &format!("/api/virtual-objects/{matched}/server"), Some(json!({"server_id": other}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (_, view) = call(&pool, &operator, "PUT", &format!("/api/virtual-objects/{matched}/server"), Some(json!({"automatic": true}))).await;
        assert_eq!((view["server"]["id"].as_str(), view["matched_by"].as_str()), (Some(web.to_string().as_str()), Some("name")));
        let (_, view) = call(&pool, &operator, "PUT", &format!("/api/virtual-objects/{matched}/server"), Some(json!({"server_id": web}))).await;
        assert_eq!(view["matched_by"], "person");

        // Installing the agent takes an administrator of the site.
        assert_eq!(call(&pool, &operator, "POST", &format!("/api/virtual-objects/{build}/install-agent"), None).await.0, StatusCode::FORBIDDEN);
        let admin = session(&pool, "admin", Role::Administrator, None).await;
        let (status, waiting) = call(&pool, &admin, "POST", &format!("/api/virtual-objects/{build}/install-agent"), None).await;
        assert_eq!(status, StatusCode::CREATED, "{waiting}");
        assert_eq!(waiting["hostname"], "build-7");
        assert_eq!(waiting["awaiting_agent"], true);
        assert_eq!(waiting["os_family"], "windows");
        let (_, page) = call(&pool, &admin, "GET", &format!("/api/servers/{}/virtualization", waiting["id"].as_str().unwrap()), None).await;
        assert_eq!(page["guest_of"][0]["object"]["name"], "build");
        assert_eq!(call(&pool, &admin, "POST", &format!("/api/virtual-objects/{build}/install-agent"), None).await.0, StatusCode::CONFLICT);
        let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_log WHERE action LIKE 'virt.%' ORDER BY id")
            .fetch_all(&pool).await.unwrap();
        assert_eq!(actions, ["virt.link", "virt.link", "virt.link", "virt.install_agent"]);
    }
}
