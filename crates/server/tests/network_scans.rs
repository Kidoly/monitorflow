//! Network scans through the API and the scan loop against a real
//! PostgreSQL, with a probe that answers from a table (ADR-0154). Ignored
//! without `DATABASE_URL`. The real probe is tested against real hosts by
//! `deploy/discovery-test/run-e2e.sh`.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use monitorflow_common::proto::v1::Sample;
use monitorflow_common::redact::Secret;
use monitorflow_probe::Check;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users;
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use monitorflow_server::netscan::{self, Tester};
use monitorflow_server::probes::{ProbeStore, TestError};
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

async fn send(pool: &PgPool, request: Request<Body>) -> (StatusCode, Value, Option<String>) {
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

/// A probe that answers from a table of addresses and their labels, and
/// keeps what it was asked.
#[derive(Default)]
struct Fake {
    answers: Mutex<HashMap<String, BTreeMap<String, String>>>,
    asked: Mutex<Vec<(String, BTreeMap<String, String>)>>,
    reachable: Mutex<Vec<String>>,
}

impl Fake {
    fn answer(&self, address: &str, labels: &[(&str, &str)]) {
        self.answers.lock().unwrap().insert(
            address.into(),
            labels
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        );
    }

    fn silence(&self, address: &str) {
        self.answers.lock().unwrap().remove(address);
    }
}

#[async_trait::async_trait]
impl Tester for Fake {
    fn reaches(&self, probe: &str) -> bool {
        probe == "central" || self.reachable.lock().unwrap().iter().any(|p| p == probe)
    }

    async fn test(&self, probe: &str, check: Check) -> Result<Vec<Sample>, TestError> {
        let params = check.config.params.clone();
        self.asked
            .lock()
            .unwrap()
            .push((probe.into(), params.clone()));
        let answers = self.answers.lock().unwrap().clone();
        let mut samples: Vec<Sample> = params["targets"]
            .split(',')
            .filter_map(|t| {
                answers.get(t).map(|labels| Sample {
                    sensor: "discover_host".into(),
                    instance: t.into(),
                    timestamp_unix_ms: 0,
                    value: 1.0,
                    labels: labels.clone().into_iter().collect(),
                })
            })
            .collect();
        // An address it was not asked about is not believed.
        samples.push(Sample {
            sensor: "discover_host".into(),
            instance: "203.0.113.99".into(),
            timestamp_unix_ms: 0,
            value: 1.0,
            labels: [("ping".to_owned(), "1".to_owned())].into_iter().collect(),
        });
        Ok(samples)
    }
}

/// Runs the scan loop until nothing is queued or running.
async fn settle(pool: &PgPool, tester: &Arc<dyn Tester>) {
    let mut running = tokio::task::JoinSet::new();
    for _ in 0..50 {
        netscan::tick(pool, tester, &mut running).await.unwrap();
        while running.join_next().await.is_some() {}
        let active: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM network_scan_runs WHERE state IN ('queued', 'running')",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        if active == 0 {
            return;
        }
        let reachable: i64 =
            sqlx::query_scalar("SELECT count(*) FROM network_scan_runs WHERE state = 'running'")
                .fetch_one(pool)
                .await
                .unwrap();
        if reachable == 0 {
            return;
        }
    }
    panic!("the scans did not settle");
}

fn action<'a>(results: &'a Value, address: &str) -> &'a Value {
    results["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["address"] == address)
        .unwrap_or_else(|| panic!("{address} not in {results}"))
}

db_tests! {
    #[sqlx::test]
    async fn a_scan_proposes_adds_and_says_what_changed(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let paris = site(&pool, "paris").await;
        let lyon = site(&pool, "lyon").await;
        let operator = session(&pool, "op", Role::Operator, Some(paris)).await;
        let viewer = session(&pool, "viewer", Role::Viewer, Some(paris)).await;

        // The site already has an agent and a device with SNMP credentials.
        sqlx::query("INSERT INTO servers (peer_id, site_id, hostname, os_family, mac_addresses)
                     VALUES ('web-01', $1, 'web-01', 'debian', '{02:00:00:00:00:0a}')")
            .bind(paris).execute(&pool).await.unwrap();
        let (status, switch) = call(&pool, &operator, "POST", "/api/devices", Some(json!({
            "site_id": paris, "name": "Old switch", "address": "192.0.2.1", "template": "switch",
            "snmp": {"version": "v2c", "community": "s1te-community"}}))).await;
        assert_eq!(status, StatusCode::CREATED, "{switch}");

        // Viewers read, other sites are not found, bad ranges are refused.
        let body = json!({"site_id": paris, "name": "Office", "targets": ["192.0.2.0/28"]});
        assert_eq!(call(&pool, &viewer, "POST", "/api/network-scans", Some(body.clone())).await.0,
                   StatusCode::FORBIDDEN);
        let mut other = body.clone();
        other["site_id"] = json!(lyon);
        assert_eq!(call(&pool, &operator, "POST", "/api/network-scans", Some(other)).await.0,
                   StatusCode::NOT_FOUND);
        for targets in [json!(["printer.lan"]), json!(["10.0.0.0/8"]), json!(["127.0.0.1"]), json!([])] {
            let mut bad = body.clone();
            bad["targets"] = targets;
            let (status, error) = call(&pool, &operator, "POST", "/api/network-scans", Some(bad)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
        }

        let (status, scan) = call(&pool, &operator, "POST", "/api/network-scans", Some(json!({
            "site_id": paris, "name": "Office", "targets": ["192.0.2.0/28"], "ports": [22, 80, 3389],
            "notify": true, "snmp": {"version": "v2c", "community": "sc4n-community"}}))).await;
        assert_eq!(status, StatusCode::CREATED, "{scan}");
        assert_eq!(scan["addresses"], 14);
        assert_eq!(scan["probe"], "central");
        assert_eq!(scan["snmp"]["version"], "v2c");
        assert!(!scan.to_string().contains("sc4n-community"), "no secret is returned");
        assert_eq!(scan["last_run"]["state"], "queued");
        let id = scan["id"].as_str().unwrap().to_owned();
        let (status, _) = call(&pool, &operator, "POST", &format!("/api/network-scans/{id}/run"), None).await;
        assert_eq!(status, StatusCode::CONFLICT, "one run at a time");

        let fake = Arc::new(Fake::default());
        fake.answer("192.0.2.1", &[("ping", "1"), ("snmp", "2"), ("groups", "interfaces")]);
        fake.answer("192.0.2.2", &[("ping", "1"), ("ports", "22,80"), ("mac", "02:00:00:00:00:0A")]);
        fake.answer("192.0.2.3", &[("ping", "1"), ("ports", "22,3389"), ("name", "desk-7.lan")]);
        fake.answer("192.0.2.4", &[("ping", "1"), ("rtt_seconds", "0.001"), ("snmp", "1"),
            ("descr", "UPS card"), ("sysname", "ups-1"), ("object_id", "1.3.6.1.4.1.8072"),
            ("groups", "ups"), ("ports", "80")]);
        fake.answer("192.0.2.5", &[("ping", "0"), ("ports", "80,443,9999"), ("name", "shop.lan")]);
        let tester: Arc<dyn Tester> = fake.clone();
        settle(&pool, &tester).await;

        // Chunks of 16: one test, with the scan's credentials first, then
        // the site's.
        let asked = fake.asked.lock().unwrap().clone();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].0, "central");
        assert_eq!(asked[0].1["snmp1_community"], "sc4n-community");
        assert_eq!(asked[0].1["snmp2_community"], "s1te-community");
        assert_eq!(asked[0].1["ports"], "22,80,3389");
        assert_eq!(asked[0].1["targets"].split(',').count(), 14);

        let (status, results) = call(&pool, &viewer, "GET", &format!("/api/network-scans/{id}/results"), None).await;
        assert_eq!(status, StatusCode::OK, "{results}");
        assert_eq!(results["run"]["state"], "done");
        assert_eq!(results["run"]["answered"], 5);
        assert_eq!(results["run"]["compared"], false);
        assert_eq!(results["run"]["dropped"], 3, "{results}");
        assert_eq!(action(&results, "192.0.2.1")["proposal"]["monitored_by"]["matched_by"], "address");
        let web = &action(&results, "192.0.2.2")["proposal"];
        assert_eq!((web["action"].as_str(), web["monitored_by"]["name"].as_str()), (Some("monitored"), Some("web-01")));
        let desk = &action(&results, "192.0.2.3")["proposal"];
        assert_eq!((desk["action"].as_str(), desk["os"].as_str()), (Some("agent"), Some("windows")));
        let ups = &action(&results, "192.0.2.4");
        assert_eq!(ups["snmp_description"], "UPS card");
        assert_eq!((ups["proposal"]["action"].as_str(), ups["proposal"]["template"].as_str()),
                   (Some("snmp_device"), Some("ups")));
        let shop = &action(&results, "192.0.2.5")["proposal"];
        assert_eq!(shop["action"], "device");
        assert_eq!(shop["checks"].as_array().unwrap().len(), 1, "port 80 only: no ping answered");

        // Dismiss the shop, then add everything proposed as a device.
        let dismiss = |a: &str| Some(json!({"address": a}));
        assert_eq!(call(&pool, &operator, "POST", &format!("/api/network-scans/{id}/dismiss"), dismiss("192.0.2.5")).await.0,
                   StatusCode::NO_CONTENT);
        assert_eq!(call(&pool, &operator, "POST", &format!("/api/network-scans/{id}/dismiss"), dismiss("192.0.2.9")).await.0,
                   StatusCode::NOT_FOUND, "never found");
        assert_eq!(call(&pool, &viewer, "POST", &format!("/api/network-scans/{id}/add"), Some(json!({"all": true}))).await.0,
                   StatusCode::FORBIDDEN);
        let (status, added) = call(&pool, &operator, "POST", &format!("/api/network-scans/{id}/add"),
            Some(json!({"all": true}))).await;
        assert_eq!(status, StatusCode::OK, "{added}");
        assert_eq!(added["added"].as_array().unwrap().len(), 1, "{added}");
        assert_eq!(added["added"][0]["name"], "ups-1");
        let ups_id = added["added"][0]["server_id"].as_str().unwrap().to_owned();
        let (_, checks) = call(&pool, &viewer, "GET", &format!("/api/servers/{ups_id}/sensor_configs"), None).await;
        let names: Vec<&str> = checks["entries"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap()).collect();
        assert_eq!(names.len(), 3, "{names:?}");
        assert!(names.contains(&"snmp_ups") && names.contains(&"ping"), "{names:?}");
        let (_, credentials) = call(&pool, &viewer, "GET", &format!("/api/devices/{ups_id}/snmp"), None).await;
        assert_eq!(credentials["community_set"], true);
        let sets = ProbeStore::new(pool.clone()).load().await.unwrap();
        let ups_check = sets["central"].checks.iter().find(|c| c.config.name == "snmp_ups").unwrap();
        assert_eq!(ups_check.config.params["community"], "sc4n-community", "the set that answered");

        // The Windows desk is added as a plain device when listed.
        let (_, added) = call(&pool, &operator, "POST", &format!("/api/network-scans/{id}/add"),
            Some(json!({"devices": [{"address": "192.0.2.3", "name": "Desk 7"}, {"address": "192.0.2.4"},
                                    {"address": "192.0.2.12"}]}))).await;
        assert_eq!(added["added"][0]["name"], "Desk 7", "{added}");
        let reasons: Vec<&str> = added["skipped"].as_array().unwrap().iter().map(|s| s["reason"].as_str().unwrap()).collect();
        assert_eq!(reasons, ["not_found", "monitored"]);
        let (_, results) = call(&pool, &viewer, "GET", &format!("/api/network-scans/{id}/results"), None).await;
        assert_eq!(action(&results, "192.0.2.4")["proposal"]["action"], "monitored");
        assert_eq!(action(&results, "192.0.2.5")["proposal"]["action"], "dismissed");
        assert_eq!(call(&pool, &operator, "POST", &format!("/api/network-scans/{id}/restore"), dismiss("192.0.2.5")).await.0,
                   StatusCode::NO_CONTENT);

        // A second run reports only what changed, and notifies.
        fake.silence("192.0.2.5");
        fake.answer("192.0.2.6", &[("ping", "1")]);
        assert_eq!(call(&pool, &operator, "POST", &format!("/api/network-scans/{id}/run"), None).await.0,
                   StatusCode::ACCEPTED);
        settle(&pool, &tester).await;
        let (_, results) = call(&pool, &viewer, "GET", &format!("/api/network-scans/{id}/results"), None).await;
        assert_eq!(results["run"]["compared"], true);
        assert_eq!(results["run"]["new"], 1);
        assert_eq!(results["run"]["gone"], json!(["192.0.2.5"]));
        assert_eq!(action(&results, "192.0.2.6")["new"], true);
        assert_eq!(action(&results, "192.0.2.4")["new"], false);
        let details: Value = sqlx::query_scalar(
            "SELECT details FROM notification_outbox WHERE event = 'network_scan.changed'")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(details["network_scan"]["new"], json!(["192.0.2.6"]));
        assert_eq!(details["site"]["name"], "paris");
        let (_, runs) = call(&pool, &viewer, "GET", &format!("/api/network-scans/{id}/runs"), None).await;
        assert_eq!(runs["runs"].as_array().unwrap().len(), 2);

        // Audited, without secrets.
        let actions: Vec<String> = sqlx::query_scalar(
            "SELECT action FROM audit_log WHERE action LIKE 'network_scan.%' ORDER BY id")
            .fetch_all(&pool).await.unwrap();
        assert_eq!(actions, ["network_scan.create", "network_scan.run", "network_scan.dismiss",
                             "network_scan.add", "network_scan.add", "network_scan.restore",
                             "network_scan.run"]);
        let audit: String = sqlx::query_scalar("SELECT string_agg(details::text, ' ') FROM audit_log")
            .fetch_one(&pool).await.unwrap();
        assert!(!audit.contains("sc4n-community") && !audit.contains("s1te-community"));

        // Changing it keeps the credentials; removing it removes its runs.
        let (status, changed) = call(&pool, &operator, "PUT", &format!("/api/network-scans/{id}"),
            Some(json!({"name": "Office", "targets": ["192.0.2.0/29"], "every": "day"}))).await;
        assert_eq!(status, StatusCode::OK, "{changed}");
        assert_eq!((changed["every"].as_str(), changed["addresses"].as_u64()), (Some("day"), Some(6)));
        assert_eq!(changed["snmp"]["version"], "v2c");
        assert_eq!(call(&pool, &operator, "DELETE", &format!("/api/network-scans/{id}"), None).await.0,
                   StatusCode::NO_CONTENT);
        let left: i64 = sqlx::query_scalar("SELECT count(*) FROM network_scan_runs").fetch_one(&pool).await.unwrap();
        assert_eq!(left, 0);
    }

    #[sqlx::test]
    async fn runs_wait_for_their_relay_and_the_schedule_queues_them(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let paris = site(&pool, "paris").await;
        let relay: Uuid = sqlx::query_scalar(
            "INSERT INTO relays (peer_id, site_id, name) VALUES ('relay-paris', $1, 'relay-paris') RETURNING id")
            .bind(paris).fetch_one(&pool).await.unwrap();
        let admin = session(&pool, "admin", Role::Administrator, None).await;
        let (status, scan) = call(&pool, &admin, "POST", "/api/network-scans", Some(json!({
            "site_id": paris, "name": "Branch", "targets": ["198.51.100.10-12"], "every": "hour"}))).await;
        assert_eq!(status, StatusCode::CREATED, "{scan}");
        assert_eq!((scan["probe"].as_str(), scan["probe_key"].as_str()), (Some("relay"), Some("relay-paris")));
        assert_eq!(scan["probe_relay_id"], json!(relay));
        let id: Uuid = scan["id"].as_str().unwrap().parse().unwrap();

        // Not held by this instance: it waits.
        let fake = Arc::new(Fake::default());
        let tester: Arc<dyn Tester> = fake.clone();
        let mut running = tokio::task::JoinSet::new();
        netscan::tick(&pool, &tester, &mut running).await.unwrap();
        let state: String = sqlx::query_scalar("SELECT state FROM network_scan_runs").fetch_one(&pool).await.unwrap();
        assert_eq!(state, "queued");
        fake.reachable.lock().unwrap().push("relay-paris".into());
        fake.answer("198.51.100.11", &[("ping", "1")]);
        settle(&pool, &tester).await;
        assert_eq!(fake.asked.lock().unwrap()[0].0, "relay-paris");

        // What a relay's scan adds is checked from the relay, so it is
        // behind it (ADR-0152) without anything set.
        let (status, added) = call(&pool, &admin, "POST", &format!("/api/network-scans/{id}/add"),
            Some(json!({"all": true}))).await;
        assert_eq!(status, StatusCode::OK, "{added}");
        let device = added["added"][0]["server_id"].as_str().unwrap().to_owned();
        let (status, page) = call(&pool, &admin, "GET", &format!("/api/servers/{device}/dependencies"), None).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        let parents = page["parents"].as_array().unwrap();
        assert!(
            parents.iter().any(|p| p["automatic"] == true && p.to_string().contains("relay-paris")),
            "{page}"
        );

        // The schedule queues the next one an hour after the last.
        assert_eq!(netscan::schedule(&pool).await.unwrap(), 0);
        sqlx::query("UPDATE network_scan_runs SET queued_at = now() - interval '61 minutes'")
            .execute(&pool).await.unwrap();
        assert_eq!(netscan::schedule(&pool).await.unwrap(), 1);
        assert_eq!(netscan::schedule(&pool).await.unwrap(), 0, "one at a time");
        let trigger: String = sqlx::query_scalar(
            "SELECT trigger FROM network_scan_runs WHERE state = 'queued'").fetch_one(&pool).await.unwrap();
        assert_eq!(trigger, "schedule");

        // A run left running by a stopped instance fails; a removed
        // relay's runs fail.
        sqlx::query("UPDATE network_scan_runs SET state = 'running', progress_at = now() - interval '5 minutes'
                     WHERE state = 'queued'").execute(&pool).await.unwrap();
        assert_eq!(netscan::fail_stale(&pool).await.unwrap(), 1);
        sqlx::query("INSERT INTO network_scan_runs (scan_id, trigger) VALUES ($1, 'person')")
            .bind(id).execute(&pool).await.unwrap();
        sqlx::query("UPDATE relays SET revoked_at = now()").execute(&pool).await.unwrap();
        netscan::tick(&pool, &tester, &mut running).await.unwrap();
        let errors: Vec<String> = sqlx::query_scalar(
            "SELECT error FROM network_scan_runs WHERE state = 'failed' ORDER BY queued_at")
            .fetch_all(&pool).await.unwrap();
        assert_eq!(errors, ["interrupted", "relay_removed"]);
    }
}
