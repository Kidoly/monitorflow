//! Proxmox VE snapshots of VMs and containers (ADR-0044, ADR-0093)
//! against a fake Proxmox API served over TLS with a test CA: tasks,
//! failures, timeouts, authentication, retention, storage that cannot
//! snapshot, and the remediation loop's hook over them. Never contacts a
//! real Proxmox. Database tests are ignored without `DATABASE_URL`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use monitorflow_common::redact::Secret;
use monitorflow_server::remediation::risk::Tier;
use monitorflow_server::remediation::runner::hook::{Change, ChangeHook};
use monitorflow_server::remediation::snapshot::{
    GuestKind, ProxmoxClient, ProxmoxOptions, ProxmoxSnapshotter, SnapshotError, SnapshotHook,
    SnapshotName, SnapshotRequest, Snapshotter, Unsupported, VmRef,
};
use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair};
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
use uuid::Uuid;

const TOKEN_ID: &str = "monitorflow@pve!snapshots";
const SECRET: &str = "5f1d2a8e-0c4b-4e6a-9d3f-7b2e1c0a9f88";

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

/// How the fake's next tasks end.
#[derive(Debug, Clone)]
enum Outcome {
    /// Running for this many polls, then `OK`.
    Ok(u32),
    Fail(&'static str),
    /// Never stops.
    Hang,
}

#[derive(Debug, Clone)]
struct Task {
    polls_left: u32,
    exit: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
struct Seen {
    method: Method,
    path: String,
    auth: String,
    form: HashMap<String, String>,
}

struct Fake {
    /// What the one guest on node `pve1` is: `qemu` or `lxc`. Paths of the
    /// other kind fail as Proxmox fails them.
    kind: Mutex<&'static str>,
    /// Its answer to `feature?feature=snapshot`.
    feature: Mutex<bool>,
    /// Its configuration, and the node's storage list.
    config: Mutex<Value>,
    storage: Mutex<Value>,
    /// Snapshots of the guest: name, description, snaptime.
    snapshots: Mutex<Vec<(String, String, i64)>>,
    tasks: Mutex<HashMap<String, Task>>,
    outcome: Mutex<Outcome>,
    seen: Mutex<Vec<Seen>>,
    next: Mutex<u64>,
}

impl Fake {
    fn start_task(&self) -> String {
        let mut next = self.next.lock().unwrap();
        *next += 1;
        let upid = format!(
            "UPID:pve1:0000{:04X}:0001:6720A0B0:qmsnapshot:101:monitorflow@pve!snapshots:",
            *next
        );
        let task = match self.outcome.lock().unwrap().clone() {
            Outcome::Ok(polls) => Task {
                polls_left: polls,
                exit: Some("OK".into()),
            },
            Outcome::Fail(m) => Task {
                polls_left: 0,
                exit: Some(m.into()),
            },
            Outcome::Hang => Task {
                polls_left: u32::MAX,
                exit: None,
            },
        };
        self.tasks.lock().unwrap().insert(upid.clone(), task);
        upid
    }

    fn names(&self) -> Vec<String> {
        self.snapshots
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.0.clone())
            .collect()
    }

    fn seen(&self, method: Method) -> Vec<Seen> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.method == method)
            .cloned()
            .collect()
    }
}

type Shared = Arc<Fake>;

/// Records the request; answers 401 unless the token is the expected one.
fn check(
    fake: &Fake,
    method: Method,
    path: String,
    headers: &HeaderMap,
    form: HashMap<String, String>,
) -> Result<(), Box<Response>> {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    fake.seen.lock().unwrap().push(Seen {
        method,
        path,
        auth: auth.clone(),
        form,
    });
    if auth != format!("PVEAPIToken={TOKEN_ID}={SECRET}") {
        return Err(Box::new(
            (StatusCode::UNAUTHORIZED, Json(json!({ "data": null }))).into_response(),
        ));
    }
    Ok(())
}

fn data(value: Value) -> Response {
    Json(json!({ "data": value })).into_response()
}

/// Proxmox's answer to a path of the wrong kind: no such configuration.
fn wrong_kind(f: &Fake, node: &str, kind: &str, vmid: u32) -> Option<Response> {
    let actual = *f.kind.lock().unwrap();
    (kind != actual).then(|| {
        let dir = if kind == "qemu" { "qemu-server" } else { kind };
        let message =
            format!("Configuration file 'nodes/{node}/{dir}/{vmid}.conf' does not exist\n");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "data": null, "message": message })),
        )
            .into_response()
    })
}

async fn list(
    State(f): State<Shared>,
    Path((node, kind, vmid)): Path<(String, String, u32)>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = check(
        &f,
        Method::GET,
        format!("{node}/{kind}/{vmid}/snapshot"),
        &headers,
        HashMap::new(),
    ) {
        return *r;
    }
    if let Some(r) = wrong_kind(&f, &node, &kind, vmid) {
        return r;
    }
    let mut items: Vec<Value> = f.snapshots.lock().unwrap().iter()
        .map(|(name, description, t)| json!({ "name": name, "description": format!("{description}\n"), "snaptime": t }))
        .collect();
    items.push(json!({ "name": "current", "description": "You are here!", "running": 1 }));
    data(Value::Array(items))
}

async fn create(
    State(f): State<Shared>,
    Path((node, kind, vmid)): Path<(String, String, u32)>,
    headers: HeaderMap,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    if let Err(r) = check(
        &f,
        Method::POST,
        format!("{node}/{kind}/{vmid}/snapshot"),
        &headers,
        form.clone(),
    ) {
        return *r;
    }
    if let Some(r) = wrong_kind(&f, &node, &kind, vmid) {
        return r;
    }
    let name = form["snapname"].clone();
    if f.names().contains(&name) {
        let body =
            json!({ "data": null, "message": format!("snapshot name '{name}' already used\n") });
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response();
    }
    let upid = f.start_task();
    if f.tasks.lock().unwrap()[&upid].exit.as_deref() == Some("OK") {
        let mut snapshots = f.snapshots.lock().unwrap();
        let t = 1_760_000_000 + snapshots.len() as i64;
        snapshots.push((
            name,
            form.get("description").cloned().unwrap_or_default(),
            t,
        ));
    }
    data(json!(upid))
}

async fn delete(
    State(f): State<Shared>,
    Path((node, kind, vmid, name)): Path<(String, String, u32, String)>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = check(
        &f,
        Method::DELETE,
        format!("{node}/{kind}/{vmid}/snapshot/{name}"),
        &headers,
        HashMap::new(),
    ) {
        return *r;
    }
    if let Some(r) = wrong_kind(&f, &node, &kind, vmid) {
        return r;
    }
    f.snapshots.lock().unwrap().retain(|s| s.0 != name);
    data(json!(f.start_task()))
}

async fn rollback(
    State(f): State<Shared>,
    Path((node, kind, vmid, name)): Path<(String, String, u32, String)>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = check(
        &f,
        Method::POST,
        format!("{node}/{kind}/{vmid}/snapshot/{name}/rollback"),
        &headers,
        HashMap::new(),
    ) {
        return *r;
    }
    if let Some(r) = wrong_kind(&f, &node, &kind, vmid) {
        return r;
    }
    data(json!(f.start_task()))
}

async fn feature(
    State(f): State<Shared>,
    Path((node, kind, vmid)): Path<(String, String, u32)>,
    axum::extract::Query(query): axum::extract::Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = check(
        &f,
        Method::GET,
        format!("{node}/{kind}/{vmid}/feature"),
        &headers,
        query.clone(),
    ) {
        return *r;
    }
    if let Some(r) = wrong_kind(&f, &node, &kind, vmid) {
        return r;
    }
    assert_eq!(query.get("feature").map(String::as_str), Some("snapshot"));
    let has = *f.feature.lock().unwrap();
    // Containers answer 0 or 1, as Proxmox does.
    data(json!({ "hasFeature": u8::from(has) }))
}

async fn config(
    State(f): State<Shared>,
    Path((node, kind, vmid)): Path<(String, String, u32)>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = check(
        &f,
        Method::GET,
        format!("{node}/{kind}/{vmid}/config"),
        &headers,
        HashMap::new(),
    ) {
        return *r;
    }
    if let Some(r) = wrong_kind(&f, &node, &kind, vmid) {
        return r;
    }
    data(f.config.lock().unwrap().clone())
}

async fn storage(
    State(f): State<Shared>,
    Path(node): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = check(
        &f,
        Method::GET,
        format!("{node}/storage"),
        &headers,
        HashMap::new(),
    ) {
        return *r;
    }
    data(f.storage.lock().unwrap().clone())
}

async fn status(
    State(f): State<Shared>,
    Path((node, upid)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = check(
        &f,
        Method::GET,
        format!("{node}/tasks/{upid}"),
        &headers,
        HashMap::new(),
    ) {
        return *r;
    }
    let mut tasks = f.tasks.lock().unwrap();
    let Some(task) = tasks.get_mut(&upid) else {
        return (StatusCode::NOT_FOUND, Json(json!({ "data": null }))).into_response();
    };
    if task.polls_left > 0 {
        task.polls_left = task.polls_left.saturating_sub(1);
        return data(json!({ "status": "running", "upid": upid }));
    }
    data(json!({ "status": "stopped", "exitstatus": task.exit, "upid": upid }))
}

/// Accepts TLS connections; failed handshakes are dropped.
struct TlsListener {
    tcp: TcpListener,
    acceptor: TlsAcceptor,
}

impl axum::serve::Listener for TlsListener {
    type Io = tokio_rustls::server::TlsStream<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let Ok((tcp, addr)) = self.tcp.accept().await else {
                continue;
            };
            if let Ok(tls) = self.acceptor.accept(tcp).await {
                return (tls, addr);
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.tcp.local_addr()
    }
}

struct Harness {
    fake: Shared,
    url: String,
    ca_pem: String,
}

impl Harness {
    async fn start() -> Self {
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca = CertifiedIssuer::self_signed(ca_params, KeyPair::generate().unwrap()).unwrap();
        let key = KeyPair::generate().unwrap();
        let cert = CertificateParams::new(vec!["127.0.0.1".to_owned()])
            .unwrap()
            .signed_by(&key, &ca)
            .unwrap();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let tls = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
            )
            .unwrap();

        let fake = Arc::new(Fake {
            kind: Mutex::new("qemu"),
            feature: Mutex::new(true),
            config: Mutex::new(json!({ "scsi0": "local-lvm:vm-101-disk-0,size=32G" })),
            storage: Mutex::new(json!([{ "storage": "local-lvm", "type": "lvmthin" }])),
            snapshots: Mutex::default(),
            tasks: Mutex::default(),
            outcome: Mutex::new(Outcome::Ok(2)),
            seen: Mutex::default(),
            next: Mutex::default(),
        });
        let app = Router::new()
            .route(
                "/api2/json/nodes/{node}/{kind}/{vmid}/snapshot",
                get(list).post(create),
            )
            .route(
                "/api2/json/nodes/{node}/{kind}/{vmid}/snapshot/{name}",
                axum::routing::delete(delete),
            )
            .route(
                "/api2/json/nodes/{node}/{kind}/{vmid}/snapshot/{name}/rollback",
                post(rollback),
            )
            .route(
                "/api2/json/nodes/{node}/{kind}/{vmid}/feature",
                get(feature),
            )
            .route("/api2/json/nodes/{node}/{kind}/{vmid}/config", get(config))
            .route("/api2/json/nodes/{node}/storage", get(storage))
            .route("/api2/json/nodes/{node}/tasks/{upid}/status", get(status))
            .with_state(fake.clone());
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("https://{}", tcp.local_addr().unwrap());
        let listener = TlsListener {
            tcp,
            acceptor: TlsAcceptor::from(Arc::new(tls)),
        };
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            fake,
            url,
            ca_pem: ca.pem(),
        }
    }

    fn client_with(&self, secret: &str, task_timeout: Duration) -> ProxmoxClient {
        let options = ProxmoxOptions {
            request_timeout: Duration::from_secs(5),
            task_timeout,
            poll_interval: Duration::from_millis(10),
        };
        ProxmoxClient::new(
            &self.url,
            TOKEN_ID,
            &Secret::new(secret),
            Some(self.ca_pem.as_bytes()),
            options,
        )
        .unwrap()
    }

    fn client(&self) -> ProxmoxClient {
        self.client_with(SECRET, Duration::from_secs(5))
    }

    fn outcome(&self, outcome: Outcome) {
        *self.fake.outcome.lock().unwrap() = outcome;
    }

    fn add_foreign(&self, name: &str, description: &str) {
        self.fake
            .snapshots
            .lock()
            .unwrap()
            .push((name.into(), description.into(), 1_700_000_000));
    }
}

fn vm() -> VmRef {
    VmRef {
        cluster: "pve".into(),
        node: "pve1".into(),
        vmid: 101,
        kind: GuestKind::Qemu,
    }
}

fn container() -> VmRef {
    VmRef {
        vmid: 104,
        kind: GuestKind::Lxc,
        ..vm()
    }
}

#[tokio::test]
async fn a_snapshot_is_created_with_the_token_and_its_task_awaited() {
    let h = Harness::start().await;
    let client = h.client();
    let upid = client
        .create_snapshot(&vm(), "mf-0123abcdef45-1", "before step 1")
        .await
        .unwrap();
    assert!(upid.starts_with("UPID:pve1:"), "{upid}");
    client.wait_task("pve1", &upid).await.unwrap();

    let posts = h.fake.seen(Method::POST);
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].path, "pve1/qemu/101/snapshot");
    assert_eq!(posts[0].auth, format!("PVEAPIToken={TOKEN_ID}={SECRET}"));
    assert_eq!(posts[0].form["snapname"], "mf-0123abcdef45-1");
    assert_eq!(posts[0].form["description"], "before step 1");
    assert!(!posts[0].form.contains_key("vmstate"), "disk only");
    // Two polls while running, one when stopped.
    let polls: Vec<_> = h
        .fake
        .seen(Method::GET)
        .into_iter()
        .filter(|s| s.path.contains("/tasks/"))
        .collect();
    assert_eq!(polls.len(), 3);
    assert_eq!(polls[0].path, format!("pve1/tasks/{upid}"));

    let listed = client.list_snapshots(&vm()).await.unwrap();
    assert_eq!(listed.len(), 1, "without `current`");
    assert_eq!(listed[0].name, "mf-0123abcdef45-1");
    assert_eq!(listed[0].snaptime, Some(1_760_000_000));
}

#[tokio::test]
async fn a_failed_task_reports_its_exit_status() {
    let h = Harness::start().await;
    h.outcome(Outcome::Fail("snapshot feature is not available"));
    let client = h.client();
    let upid = client
        .create_snapshot(&vm(), "mf-0123abcdef45-1", "d")
        .await
        .unwrap();
    match client.wait_task("pve1", &upid).await {
        Err(SnapshotError::TaskFailed(m)) => assert_eq!(m, "snapshot feature is not available"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_task_that_never_stops_times_out() {
    let h = Harness::start().await;
    h.outcome(Outcome::Hang);
    let client = h.client_with(SECRET, Duration::from_millis(100));
    let upid = client
        .create_snapshot(&vm(), "mf-0123abcdef45-1", "d")
        .await
        .unwrap();
    let started = std::time::Instant::now();
    match client.wait_task("pve1", &upid).await {
        Err(SnapshotError::Timeout { upid: u }) => assert_eq!(u, upid),
        other => panic!("{other:?}"),
    }
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn a_refused_token_is_an_auth_error_without_the_secret() {
    let h = Harness::start().await;
    let client = h.client_with("wrong-secret", Duration::from_secs(5));
    let error = client
        .create_snapshot(&vm(), "mf-0123abcdef45-1", "d")
        .await
        .unwrap_err();
    assert!(
        matches!(error, SnapshotError::Auth { status: 401 }),
        "{error:?}"
    );
    assert!(!error.to_string().contains("wrong-secret"));
    assert!(h.fake.names().is_empty());
}

#[tokio::test]
async fn api_errors_carry_the_proxmox_message() {
    let h = Harness::start().await;
    h.add_foreign("mf-0123abcdef45-1", "someone else");
    let error = h
        .client()
        .create_snapshot(&vm(), "mf-0123abcdef45-1", "d")
        .await
        .unwrap_err();
    match error {
        SnapshotError::Api {
            status: 500,
            message,
        } => {
            assert_eq!(message, "snapshot name 'mf-0123abcdef45-1' already used")
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn certificates_are_verified() {
    let h = Harness::start().await;
    // The built-in web roots do not trust the test CA.
    let options = ProxmoxOptions {
        request_timeout: Duration::from_secs(5),
        task_timeout: Duration::from_secs(5),
        poll_interval: Duration::from_millis(10),
    };
    let client = ProxmoxClient::new(
        &h.url,
        TOKEN_ID,
        &Secret::new(SECRET),
        None,
        options.clone(),
    )
    .unwrap();
    let error = client.list_snapshots(&vm()).await.unwrap_err();
    assert!(matches!(error, SnapshotError::Transport(_)), "{error:?}");
    // Neither does another CA.
    let other = Harness::start().await;
    let client = ProxmoxClient::new(
        &h.url,
        TOKEN_ID,
        &Secret::new(SECRET),
        Some(other.ca_pem.as_bytes()),
        options,
    )
    .unwrap();
    assert!(matches!(
        client.list_snapshots(&vm()).await,
        Err(SnapshotError::Transport(_))
    ));
    assert!(
        h.fake.seen.lock().unwrap().is_empty(),
        "no request got through"
    );
}

/// A customer, site and server; the server mapped to VM 101 unless `vm`
/// is `None`.
async fn server(pool: &PgPool, mapped: Option<VmRef>) -> Uuid {
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
    let server: Uuid = sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family) VALUES ('agent-1', $1, 'web-1', 'debian') RETURNING id",
    )
    .bind(site).fetch_one(pool).await.unwrap();
    if let Some(vm) = mapped {
        sqlx::query("INSERT INTO server_proxmox_vms (server_id, cluster, node, vmid, kind) VALUES ($1, $2, $3, $4, $5)")
            .bind(server).bind(&vm.cluster).bind(&vm.node).bind(vm.vmid as i32).bind(vm.kind.as_str())
            .execute(pool).await.unwrap();
    }
    server
}

fn snapshotter(h: &Harness, pool: &PgPool, keep: usize) -> ProxmoxSnapshotter {
    ProxmoxSnapshotter::new(
        pool.clone(),
        HashMap::from([("pve".to_owned(), h.client())]),
        keep,
    )
}

async fn audit_actions(pool: &PgPool) -> Vec<(String, String)> {
    sqlx::query_as(
        "SELECT action, details->>'name' FROM audit_log WHERE action LIKE 'proxmox.%' ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

db_tests! {
    #[sqlx::test]
    async fn retention_keeps_the_newest_and_never_touches_foreign_snapshots(pool: PgPool) {
        let h = Harness::start().await;
        let server = server(&pool, Some(vm())).await;
        let incident = Uuid::from_u128(0x0123_abcd_ef45 << 80);
        // Made by someone else, one even with a MonitorFlow-looking name.
        h.add_foreign("before-upgrade", "manual");
        h.add_foreign(SnapshotName::for_step(Uuid::from_u128(0xfeed << 80), 1).as_str(), "manual");
        let snapshots = snapshotter(&h, &pool, 2);

        for step in 1..=4 {
            let taken = snapshots.take(server, SnapshotRequest { incident, step }).await.unwrap();
            assert_eq!(taken.name, SnapshotName::for_step(incident, step).as_str());
            assert!(taken.managed);
        }
        let mut names = h.fake.names();
        names.sort();
        assert_eq!(names, ["before-upgrade", "mf-00000000feed-1", "mf-0123abcdef45-3", "mf-0123abcdef45-4"]);
        assert_eq!(
            h.fake.seen(Method::DELETE).iter().map(|s| s.path.as_str()).collect::<Vec<_>>(),
            ["pve1/qemu/101/snapshot/mf-0123abcdef45-1", "pve1/qemu/101/snapshot/mf-0123abcdef45-2"]
        );
        let live: Vec<String> = sqlx::query_scalar("SELECT name FROM proxmox_snapshots WHERE deleted_at IS NULL ORDER BY id")
            .fetch_all(&pool).await.unwrap();
        assert_eq!(live, ["mf-0123abcdef45-3", "mf-0123abcdef45-4"]);

        // A retry of a step returns the snapshot it took, without a task.
        let posts = h.fake.seen(Method::POST).len();
        let again = snapshots.take(server, SnapshotRequest { incident, step: 4 }).await.unwrap();
        assert_eq!(again.name, "mf-0123abcdef45-4");
        assert_eq!(h.fake.seen(Method::POST).len(), posts);

        let listed = snapshots.list(server).await.unwrap();
        let managed: Vec<(&str, bool)> = listed.iter().map(|s| (s.name.as_str(), s.managed)).collect();
        assert_eq!(managed, [
            ("before-upgrade", false),
            ("mf-00000000feed-1", false),
            ("mf-0123abcdef45-3", true),
            ("mf-0123abcdef45-4", true),
        ]);

        let audit = audit_actions(&pool).await;
        let count = |a: &str| audit.iter().filter(|(action, _)| action == a).count();
        assert_eq!((count("proxmox.snapshot.create"), count("proxmox.snapshot.delete")), (4, 2));
    }

    #[sqlx::test]
    async fn failures_leave_no_record_and_names_of_others_are_refused(pool: PgPool) {
        let h = Harness::start().await;
        let server = server(&pool, Some(vm())).await;
        let incident = Uuid::from_u128(7 << 80);
        let snapshots = snapshotter(&h, &pool, 3);

        h.outcome(Outcome::Fail("VM is locked (backup)"));
        let error = snapshots.take(server, SnapshotRequest { incident, step: 1 }).await.unwrap_err();
        assert!(matches!(error, SnapshotError::TaskFailed(ref m) if m == "VM is locked (backup)"), "{error:?}");
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM proxmox_snapshots").fetch_one(&pool).await.unwrap();
        assert_eq!(rows, 0);

        // Same name, another description: not ours.
        h.outcome(Outcome::Ok(0));
        h.add_foreign(SnapshotName::for_step(incident, 2).as_str(), "manual");
        let error = snapshots.take(server, SnapshotRequest { incident, step: 2 }).await.unwrap_err();
        assert!(matches!(error, SnapshotError::NameTaken(_)), "{error:?}");

        // Same name and this step's description: a task that outlived its
        // timeout, adopted.
        let request = SnapshotRequest { incident, step: 3 };
        h.add_foreign(request.name().as_str(), &request.description());
        let posts = h.fake.seen(Method::POST).len();
        assert!(snapshots.take(server, request).await.unwrap().managed);
        assert_eq!(h.fake.seen(Method::POST).len(), posts);
    }

    #[sqlx::test]
    async fn rollback_only_to_snapshots_monitorflow_took(pool: PgPool) {
        let h = Harness::start().await;
        let server = server(&pool, Some(vm())).await;
        let snapshots = snapshotter(&h, &pool, 3);
        let request = SnapshotRequest { incident: Uuid::from_u128(9 << 80), step: 1 };
        snapshots.take(server, request).await.unwrap();
        snapshots.rollback(server, &request.name()).await.unwrap();
        let rollbacks: Vec<_> = h.fake.seen(Method::POST).into_iter().filter(|s| s.path.ends_with("/rollback")).collect();
        assert_eq!(rollbacks.len(), 1);
        assert_eq!(rollbacks[0].path, format!("pve1/qemu/101/snapshot/{}/rollback", request.name()));

        let foreign = SnapshotName::for_step(Uuid::from_u128(0xfeed << 80), 1);
        h.add_foreign(foreign.as_str(), "manual");
        let error = snapshots.rollback(server, &foreign).await.unwrap_err();
        assert!(matches!(error, SnapshotError::UnknownSnapshot(_)), "{error:?}");
        assert_eq!(h.fake.seen(Method::POST).iter().filter(|s| s.path.ends_with("/rollback")).count(), 1);
        let audit = audit_actions(&pool).await;
        assert_eq!(audit.last().unwrap().0, "proxmox.snapshot.rollback");
    }

    #[sqlx::test]
    async fn support_needs_a_mapping_and_a_configured_cluster(pool: PgPool) {
        let h = Harness::start().await;
        let unmapped = server(&pool, None).await;
        let snapshots = snapshotter(&h, &pool, 3);
        assert_eq!(snapshots.support(unmapped).await.unwrap().reason(), Some(Unsupported::NotMapped));
        let request = SnapshotRequest { incident: Uuid::from_u128(1), step: 1 };
        assert!(matches!(
            snapshots.take(unmapped, request).await,
            Err(SnapshotError::Unsupported(Unsupported::NotMapped))
        ));
        sqlx::query("INSERT INTO server_proxmox_vms (server_id, cluster, node, vmid) VALUES ($1, 'lab', 'pve1', 101)")
            .bind(unmapped).execute(&pool).await.unwrap();
        assert_eq!(snapshots.support(unmapped).await.unwrap().reason(), Some(Unsupported::UnknownCluster));
        let disabled = ProxmoxSnapshotter::new(pool.clone(), HashMap::new(), 3);
        assert_eq!(disabled.support(unmapped).await.unwrap().reason(), Some(Unsupported::Disabled));
        assert!(h.fake.seen.lock().unwrap().is_empty());
    }

    #[sqlx::test]
    async fn the_change_hook_snapshots_mapped_servers_and_falls_back_for_others(pool: PgPool) {
        let h = Harness::start().await;
        let mapped = server(&pool, Some(vm())).await;
        let hook = SnapshotHook::new(Arc::new(snapshotter(&h, &pool, 3)), "proxmox");
        let incident = Uuid::from_u128(0x0123_abcd_ef45 << 80);
        let change = |server, step| Change {
            incident_id: incident,
            server_id: server,
            step,
            command: "systemctl restart nginx",
            tier: Tier::LowImpact,
        };

        let recorded = hook.before_change(&change(mapped, 2)).await.unwrap();
        assert_eq!(recorded.method, "proxmox");
        assert_eq!(recorded.name.as_deref(), Some("mf-0123abcdef45-2"));
        assert!(recorded.detail.contains("VM 101 on pve/pve1"), "{}", recorded.detail);
        assert_eq!(h.fake.names(), ["mf-0123abcdef45-2"]);
        // A resumed incident asks again for the same step: no second task.
        let posts = h.fake.seen(Method::POST).len();
        assert_eq!(hook.before_change(&change(mapped, 2)).await.unwrap(), recorded);
        assert_eq!(h.fake.seen(Method::POST).len(), posts);

        // A failed task is an error, so the loop escalates.
        h.outcome(Outcome::Fail("VM is locked (backup)"));
        let error = hook.before_change(&change(mapped, 3)).await.unwrap_err();
        assert!(error.contains("VM is locked (backup)"), "{error}");

        // Unmapped servers, and no cluster at all, never reach Proxmox.
        let requests = h.fake.seen.lock().unwrap().len();
        sqlx::query("DELETE FROM server_proxmox_vms").execute(&pool).await.unwrap();
        let recorded = hook.before_change(&change(mapped, 4)).await.unwrap();
        assert_eq!((recorded.method.as_str(), recorded.name), ("none", None));
        assert!(recorded.detail.contains("not mapped"), "{}", recorded.detail);
        let off = SnapshotHook::new(Arc::new(ProxmoxSnapshotter::new(pool.clone(), HashMap::new(), 3)), "proxmox");
        let recorded = off.before_change(&change(mapped, 4)).await.unwrap();
        assert_eq!(recorded.method, "none");
        assert!(recorded.detail.ends_with("rollback is limited"), "{}", recorded.detail);
        assert_eq!(h.fake.seen.lock().unwrap().len(), requests);
    }

    #[sqlx::test]
    async fn containers_are_snapshotted_rolled_back_and_pruned_through_lxc_paths(pool: PgPool) {
        let h = Harness::start().await;
        *h.fake.kind.lock().unwrap() = "lxc";
        let server = server(&pool, Some(container())).await;
        let snapshots = snapshotter(&h, &pool, 2);
        let incident = Uuid::from_u128(0x0123_abcd_ef45 << 80);
        for step in 1..=3 {
            snapshots.take(server, SnapshotRequest { incident, step }).await.unwrap();
        }
        let posts = h.fake.seen(Method::POST);
        assert!(posts.iter().all(|s| s.path == "pve1/lxc/104/snapshot"), "{posts:?}");
        assert!(posts.iter().all(|s| !s.form.contains_key("vmstate")));
        assert_eq!(
            h.fake.seen(Method::DELETE).iter().map(|s| s.path.as_str()).collect::<Vec<_>>(),
            ["pve1/lxc/104/snapshot/mf-0123abcdef45-1"]
        );
        assert_eq!(h.fake.names(), ["mf-0123abcdef45-2", "mf-0123abcdef45-3"]);

        snapshots.rollback(server, &SnapshotName::for_step(incident, 3)).await.unwrap();
        let rollbacks: Vec<_> = h.fake.seen(Method::POST).into_iter().filter(|s| s.path.ends_with("/rollback")).collect();
        assert_eq!(rollbacks.len(), 1);
        assert_eq!(rollbacks[0].path, "pve1/lxc/104/snapshot/mf-0123abcdef45-3/rollback");
        let listed = snapshots.list(server).await.unwrap();
        assert_eq!(listed.iter().filter(|s| s.managed).count(), 2);

        let kinds: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT details->>'kind' FROM audit_log WHERE action LIKE 'proxmox.snapshot.%'",
        )
        .fetch_all(&pool).await.unwrap();
        assert_eq!(kinds, ["lxc"]);

        let hook = SnapshotHook::new(Arc::new(snapshotter(&h, &pool, 2)), "proxmox");
        let change = Change {
            incident_id: incident,
            server_id: server,
            step: 4,
            command: "systemctl restart nginx",
            tier: Tier::LowImpact,
        };
        let recorded = hook.before_change(&change).await.unwrap();
        assert_eq!(recorded.method, "proxmox");
        assert!(recorded.detail.contains("of container 104 on pve/pve1"), "{}", recorded.detail);
    }

    #[sqlx::test]
    async fn a_container_on_storage_without_snapshots_limits_rollback(pool: PgPool) {
        let h = Harness::start().await;
        *h.fake.kind.lock().unwrap() = "lxc";
        *h.fake.feature.lock().unwrap() = false;
        *h.fake.config.lock().unwrap() = json!({
            "hostname": "web-1",
            "rootfs": "local:104/vm-104-disk-0.raw,size=8G",
            "unprivileged": 1,
        });
        *h.fake.storage.lock().unwrap() = json!([
            { "storage": "local", "type": "dir" },
            { "storage": "local-zfs", "type": "zfspool" },
        ]);
        let server = server(&pool, Some(container())).await;
        let snapshots = snapshotter(&h, &pool, 3);
        // Not known before Proxmox is asked: a real attempt would tell.
        assert!(snapshots.support(server).await.unwrap().is_available());

        assert_eq!(snapshots.check_due(Duration::from_secs(3600)).await.unwrap(), 1);
        assert_eq!(
            snapshots.support(server).await.unwrap().reason(),
            Some(Unsupported::CannotSnapshot(GuestKind::Lxc))
        );
        let why: Option<String> = sqlx::query_scalar("SELECT cannot_snapshot FROM server_proxmox_vms")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(why.as_deref(), Some("rootfs is on storage local (dir), which cannot take snapshots"));
        // Checked an hour at most ago: not due again.
        assert_eq!(snapshots.check_due(Duration::from_secs(3600)).await.unwrap(), 0);

        // The change runs, with rollback limited, as the approver was shown.
        let hook = SnapshotHook::new(Arc::new(snapshotter(&h, &pool, 3)), "proxmox");
        let change = Change {
            incident_id: Uuid::from_u128(5 << 80),
            server_id: server,
            step: 1,
            command: "systemctl restart nginx",
            tier: Tier::LowImpact,
        };
        let recorded = hook.before_change(&change).await.unwrap();
        assert_eq!((recorded.method.as_str(), recorded.name), ("none", None));
        assert_eq!(recorded.detail, "the container's storage cannot take snapshots; rollback is limited");
        assert!(h.fake.seen(Method::POST).is_empty());

        // Moved to ZFS, with a bind mount: snapshots again, which leave
        // the bind mount out.
        *h.fake.feature.lock().unwrap() = true;
        *h.fake.config.lock().unwrap() = json!({
            "rootfs": "local-zfs:subvol-104-disk-0,size=8G",
            "mp0": "/srv/shared,mp=/shared",
        });
        let found = snapshots.check(server, &container()).await.unwrap();
        assert_eq!(found.cannot_snapshot, None);
        assert_eq!(found.left_out.as_deref(), Some("mp0 at /shared, a bind mount"));
        assert!(snapshots.support(server).await.unwrap().is_available());
        let left_out: Option<String> = sqlx::query_scalar("SELECT left_out FROM server_proxmox_vms")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(left_out.as_deref(), Some("mp0 at /shared, a bind mount"));
    }

    #[sqlx::test]
    async fn a_failed_snapshot_has_the_guest_checked_again(pool: PgPool) {
        let h = Harness::start().await;
        let server = server(&pool, Some(vm())).await;
        let snapshots = snapshotter(&h, &pool, 3);
        assert_eq!(snapshots.check_due(Duration::from_secs(3600)).await.unwrap(), 1);
        assert_eq!(snapshots.check_due(Duration::from_secs(3600)).await.unwrap(), 0);

        h.outcome(Outcome::Fail("snapshot feature is not available"));
        let request = SnapshotRequest { incident: Uuid::from_u128(3 << 80), step: 1 };
        let error = snapshots.take(server, request).await.unwrap_err();
        assert!(matches!(error, SnapshotError::TaskFailed(_)), "{error:?}");
        assert_eq!(snapshots.check_due(Duration::from_secs(3600)).await.unwrap(), 1, "checked again at once");

        // A mapping of the wrong kind fails as Proxmox fails it.
        sqlx::query("UPDATE server_proxmox_vms SET kind = 'lxc'").execute(&pool).await.unwrap();
        h.outcome(Outcome::Ok(0));
        let error = snapshots.take(server, SnapshotRequest { step: 2, ..request }).await.unwrap_err();
        match error {
            SnapshotError::Api { status: 500, message } => {
                assert_eq!(message, "Configuration file 'nodes/pve1/lxc/101.conf' does not exist")
            }
            other => panic!("{other:?}"),
        }
        let checked: Option<chrono::DateTime<chrono::Utc>> =
            sqlx::query_scalar("SELECT checked_at FROM server_proxmox_vms").fetch_one(&pool).await.unwrap();
        assert_eq!(checked, None);
    }
}
