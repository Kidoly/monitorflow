//! Enrollment tokens and the `Enroll` RPC with PostgreSQL, up to a real
//! mTLS stream with the issued certificate. Without `DATABASE_URL` at build
//! time the tests are reported as ignored (see `build.rs`).

use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use monitorflow_common::config::TlsPaths;
use monitorflow_common::proto::v1::agent_service_client::AgentServiceClient;
use monitorflow_common::proto::v1::enroll_service_client::EnrollServiceClient;
use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::{EnrollRequest, EnrollResponse, Envelope, Hello};
use monitorflow_common::tls::{self, EnrollmentToken, Identity, PeerIdentity, PeerRole};
use monitorflow_common::PROTOCOL_VERSION;
use monitorflow_server::api::{self, auth::CSRF_HEADER, users, ApiSettings, ApiState, Role};
use monitorflow_server::ca::{CaOptions, CertificateAuthority, ServerName};
use monitorflow_server::db;
use monitorflow_server::enroll::install_key::{self, KeyRequest};
use monitorflow_server::enroll::token::{self, Creator, TokenError, TokenRequest};
use monitorflow_server::enroll::{self, EnrollError, Enroller, FailureLimiter};
use monitorflow_server::grpc::{
    self, AcceptedHello, AgentStreamService, DbInventory, DbRelayPolicy, Inventory, InventoryError,
    Registry, StreamSettings,
};
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType};
use rustls::pki_types::CertificateDer;
use sqlx::PgPool;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio_stream::wrappers::ReceiverStream;
use tonic::Code;
use tower::ServiceExt;
use uuid::Uuid;

const TIMEOUT: Duration = Duration::from_secs(5);

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

const CA: [u8; 32] = [9; 32];

fn request(site_id: Uuid, peer_id: &str) -> TokenRequest {
    TokenRequest {
        role: PeerRole::Agent,
        peer_id: peer_id.into(),
        site_id,
        ttl: token::DEFAULT_TTL,
        creator: Creator::Cli,
        server_names: Vec::new(),
        parent_relay: None,
    }
}

async fn site(pool: &PgPool) -> Uuid {
    token::site_id(pool, "paris", true).await.unwrap().unwrap()
}

struct Pki {
    _dir: TempDir,
    ca: Arc<CertificateAuthority>,
}

impl Pki {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let ca = CertificateAuthority::open_or_create(&dir.path().join("ca"), CaOptions::default())
            .unwrap();
        Self {
            _dir: dir,
            ca: Arc::new(ca),
        }
    }

    fn fingerprint(&self) -> [u8; 32] {
        enroll::ca_fingerprint(&self.ca)
    }

    fn enroller(&self, pool: &PgPool) -> Enroller {
        Enroller::new(pool.clone(), self.ca.clone(), FailureLimiter::default())
    }

    async fn token(&self, pool: &PgPool, peer_id: &str) -> String {
        let site_id = site(pool).await;
        let created = token::create(pool, self.fingerprint(), &request(site_id, peer_id))
            .await
            .unwrap();
        created.token.to_secret_string().expose().to_owned()
    }

    /// Server identity `monitorflow://server/central` for localhost, issued by the CA.
    fn server_identity(&self, dir: &Path) -> Identity {
        let key = KeyPair::generate().unwrap();
        let id = PeerIdentity::new(PeerRole::Server, "central").unwrap();
        let names = [
            ServerName::Dns("localhost".into()),
            ServerName::Ip(Ipv4Addr::LOCALHOST.into()),
        ];
        let cert = self
            .ca
            .issue(&csr(&key, "central", vec![]), &id, None, &names)
            .unwrap();
        write_identity(
            dir,
            "server",
            self.ca.ca_cert_pem(),
            &cert.cert_pem,
            &key.serialize_pem(),
        )
    }
}

/// CSR for `key` asking for `cn` and `sans`, which the CA must ignore.
fn csr(key: &KeyPair, cn: &str, sans: Vec<SanType>) -> String {
    let mut params = CertificateParams::default();
    params.distinguished_name = DistinguishedName::new();
    params.distinguished_name.push(DnType::CommonName, cn);
    params.subject_alt_names = sans;
    params.serialize_request(key).unwrap().pem().unwrap()
}

fn write_identity(dir: &Path, name: &str, ca: &str, cert: &str, key: &str) -> Identity {
    let paths = TlsPaths {
        ca_cert: dir.join(format!("{name}-ca.pem")),
        cert: dir.join(format!("{name}.pem")),
        key: dir.join(format!("{name}-key.pem")),
    };
    std::fs::write(&paths.ca_cert, ca).unwrap();
    std::fs::write(&paths.cert, cert).unwrap();
    std::fs::write(&paths.key, key).unwrap();
    Identity::load(&paths).unwrap()
}

fn identity_of(cert_pem: &str) -> PeerIdentity {
    let der = CertificateDer::from(pem::parse(cert_pem).unwrap().into_contents());
    PeerIdentity::from_cert(&der).unwrap()
}

async fn count(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

fn refused<T: std::fmt::Debug>(result: Result<T, EnrollError>) {
    let err = result.unwrap_err();
    assert!(matches!(err, EnrollError::Refused), "{err:?}");
    let status = tonic::Status::from(err);
    assert_eq!(
        (status.code(), status.message()),
        (Code::PermissionDenied, "enrollment refused")
    );
}

/// The two listeners of a server, on 127.0.0.1.
struct Server {
    enroll_url: String,
    grpc_url: String,
    registry: Registry,
}

impl Server {
    async fn start(pki: &Pki, pool: &PgPool, limiter: FailureLimiter, dir: &Path) -> Self {
        let identity = pki.server_identity(dir);
        let enroll_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let grpc_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = |l: &TcpListener| format!("https://localhost:{}", l.local_addr().unwrap().port());
        let (enroll_url, grpc_url) = (url(&enroll_listener), url(&grpc_listener));

        let enroller = Enroller::new(pool.clone(), pki.ca.clone(), limiter);
        let tls = identity.server_config_without_client_auth().unwrap();
        tokio::spawn(enroll::serve(
            enroll_listener,
            tls,
            enroller,
            std::future::pending(),
        ));

        let crl = pki.ca.crl().unwrap();
        // incoming() only borrows the latest value; the sender may go.
        let (_, rx) = watch::channel(identity.server_config(vec![crl]).unwrap());
        let registry = Registry::default();
        let service = AgentStreamService::new(
            registry.clone(),
            Arc::new(DbRelayPolicy::new(pool.clone())),
            StreamSettings::default(),
        )
        .with_inventory(Arc::new(DbInventory::new(pool.clone())));
        tokio::spawn(grpc::serve(
            grpc_listener,
            rx,
            service,
            std::future::pending(),
        ));
        Self {
            enroll_url,
            grpc_url,
            registry,
        }
    }

    // `Status` is boxed to keep the `Err` variant small (clippy::result_large_err).
    async fn enroll(
        &self,
        token: &str,
        csr_pem: &str,
    ) -> Result<EnrollResponse, Box<tonic::Status>> {
        self.enroll_host(token, csr_pem, "").await
    }

    async fn enroll_host(
        &self,
        token: &str,
        csr_pem: &str,
        hostname: &str,
    ) -> Result<EnrollResponse, Box<tonic::Status>> {
        let pin = *EnrollmentToken::parse(token).unwrap().ca_fingerprint();
        let channel = tls::connect(
            &self.enroll_url,
            tls::pinned_client_config(pin).unwrap(),
            TIMEOUT,
        )
        .await
        .unwrap();
        let request = EnrollRequest {
            token: token.into(),
            csr_pem: csr_pem.into(),
            hostname: hostname.into(),
        };
        EnrollServiceClient::new(channel)
            .enroll(request)
            .await
            .map(tonic::Response::into_inner)
            .map_err(Box::new)
    }
}

/// `GET /api/servers` as a new viewer.
async fn list_servers(pool: &PgPool, registry: Registry) -> serde_json::Value {
    let password = monitorflow_common::redact::Secret::new("correct horse battery");
    users::create_user(pool, "viewer", &password, Role::Viewer, "test")
        .await
        .unwrap();
    let app = api::router(ApiState::new(
        pool.clone(),
        registry,
        ApiSettings::default(),
    ));
    let login = axum::http::Request::post("/api/auth/login")
        .header("content-type", "application/json")
        .header(CSRF_HEADER, "1")
        .body(axum::body::Body::from(
            serde_json::json!({ "username": "viewer", "password": password.expose() }).to_string(),
        ))
        .unwrap();
    let response = app.clone().oneshot(login).await.unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let cookie = response.headers()["set-cookie"].to_str().unwrap();
    let cookie = cookie.split(';').next().unwrap().to_owned();
    let list = axum::http::Request::get("/api/servers")
        .header("cookie", cookie)
        .body(axum::body::Body::empty())
        .unwrap();
    let response = app.oneshot(list).await.unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&body).unwrap()
}

async fn enroll_as(
    pki: &Pki,
    pool: &PgPool,
    role: PeerRole,
    peer_id: &str,
    site: &str,
) -> Result<enroll::Enrolled, EnrollError> {
    let site_id = token::site_id(pool, site, true).await.unwrap().unwrap();
    let request = TokenRequest {
        role,
        ..request(site_id, peer_id)
    };
    let created = token::create(pool, pki.fingerprint(), &request)
        .await
        .unwrap();
    let csr = csr(&KeyPair::generate().unwrap(), peer_id, vec![]);
    pki.enroller(pool)
        .enroll(created.token.to_secret_string().expose(), &csr, None)
        .await
}

/// A new install key of the site `paris`.
async fn install_key(
    pki: &Pki,
    pool: &PgPool,
    max_hosts: Option<u32>,
    require_approval: bool,
) -> String {
    let request = KeyRequest {
        site_id: site(pool).await,
        ttl: install_key::DEFAULT_TTL,
        max_hosts,
        require_approval,
        creator: Creator::Cli,
    };
    let created = install_key::create(pool, pki.fingerprint(), &request)
        .await
        .unwrap();
    created.key.to_secret_string().expose().to_owned()
}

/// Enrolls with `key` as the host `hostname`; returns the agent ID.
async fn enroll_host(
    pki: &Pki,
    pool: &PgPool,
    key: &str,
    hostname: &str,
) -> Result<String, EnrollError> {
    let csr = csr(&KeyPair::generate().unwrap(), "monitorflow-agent", vec![]);
    let enrolled = pki
        .enroller(pool)
        .enroll_host(key, &csr, hostname, None)
        .await?;
    assert_eq!(enrolled.identity.role(), PeerRole::Agent);
    Ok(enrolled.identity.id().to_owned())
}

/// The first Hello of the agent `id`, as the stream sends it.
async fn first_hello(pool: &PgPool, id: &str) -> Result<String, InventoryError> {
    let peer = PeerIdentity::new(PeerRole::Agent, id).unwrap();
    let hello = Hello {
        agent_id: id.into(),
        hostname: id.into(),
        os_family: "debian".into(),
        ..Default::default()
    };
    DbInventory::new(pool.clone())
        .hello(AcceptedHello {
            peer: &peer,
            hello: &hello,
            via_relay: None,
            role: PeerRole::Agent,
        })
        .await
}

db_tests! {
    #[sqlx::test]
    async fn an_install_key_enrolls_many_hosts_under_unique_names(pool: PgPool) {
        let pki = Pki::new();
        // web-01 is taken by a single-use token, WEB-02 by a server in use.
        pki.token(&pool, "web-01").await;
        sqlx::query("INSERT INTO servers (peer_id, site_id, hostname, os_family)
                     VALUES ('WEB-02', $1, 'x', 'debian')")
            .bind(site(&pool).await)
            .execute(&pool)
            .await
            .unwrap();
        let key = install_key(&pki, &pool, None, false).await;
        let mut ids = Vec::new();
        for hostname in ["web-01", "Web-01", "web-02", "db 1", "db-1"] {
            ids.push(enroll_host(&pki, &pool, &key, hostname).await.unwrap());
        }
        assert_eq!(ids, ["web-01-2", "web-01-3", "web-02-2", "db-1", "db-1-2"]);

        // Each host lands in the key's site at its first Hello.
        assert_eq!(first_hello(&pool, "db-1").await.unwrap(), "paris");
        let (count, recorded): (i32, i64) = sqlx::query_as(
            "SELECT host_count, (SELECT count(*) FROM enrollment_tokens t
                                 WHERE t.install_key_id = k.id AND t.used_at IS NOT NULL)
             FROM install_keys k",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((count, recorded), (5, 5));
        let audited: serde_json::Value = sqlx::query_scalar(
            "SELECT details FROM audit_log WHERE action = 'peer.enroll' AND target_id = 'db-1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(audited["install_key_id"].is_string(), "{audited}");
    }

    #[sqlx::test]
    async fn install_keys_stop_at_their_limit_expiry_or_revocation(pool: PgPool) {
        let pki = Pki::new();
        let one = install_key(&pki, &pool, Some(1), false).await;
        enroll_host(&pki, &pool, &one, "a").await.unwrap();
        refused(enroll_host(&pki, &pool, &one, "b").await);

        let revoked = install_key(&pki, &pool, None, false).await;
        let expired = install_key(&pki, &pool, None, false).await;
        sqlx::query("UPDATE install_keys SET revoked_at = now() WHERE key_hash = $1")
            .bind(token::hash(&EnrollmentToken::parse(&revoked).unwrap()).as_slice())
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE install_keys SET created_at = now() - interval '2 days',
                                             expires_at = now() - interval '1 day'
                     WHERE key_hash = $1")
            .bind(token::hash(&EnrollmentToken::parse(&expired).unwrap()).as_slice())
            .execute(&pool)
            .await
            .unwrap();
        refused(enroll_host(&pki, &pool, &revoked, "c").await);
        refused(enroll_host(&pki, &pool, &expired, "c").await);

        // Relays and agents too old to send their host name are told why.
        let key = install_key(&pki, &pool, None, false).await;
        let err = enroll_host(&pki, &pool, &key, " ").await.unwrap_err();
        assert!(matches!(err, EnrollError::NoHostname), "{err:?}");
        assert_eq!(tonic::Status::from(err).code(), Code::InvalidArgument);
        assert_eq!(count(&pool, "SELECT count(*) FROM certificates").await, 1);
    }

    #[sqlx::test]
    async fn install_keys_enroll_at_most_a_few_hosts_a_minute(pool: PgPool) {
        let pki = Pki::new();
        let key = install_key(&pki, &pool, None, false).await;
        for i in 0..install_key::MAX_PER_MINUTE {
            enroll_host(&pki, &pool, &key, &format!("h{i}")).await.unwrap();
        }
        let err = enroll_host(&pki, &pool, &key, "one-more").await.unwrap_err();
        assert!(matches!(err, EnrollError::Busy), "{err:?}");
        // Agents retry RESOURCE_EXHAUSTED.
        assert_eq!(tonic::Status::from(err).code(), Code::ResourceExhausted);
    }

    #[sqlx::test]
    async fn hosts_of_an_approval_key_wait_until_approved(pool: PgPool) {
        let pki = Pki::new();
        let key = install_key(&pki, &pool, None, true).await;
        let id = enroll_host(&pki, &pool, &key, "web-01").await.unwrap();
        let err = first_hello(&pool, &id).await.unwrap_err();
        assert!(matches!(err, InventoryError::AwaitingApproval), "{err:?}");
        assert_eq!(count(&pool, "SELECT count(*) FROM servers").await, 0);
        sqlx::query("DELETE FROM pending_hosts").execute(&pool).await.unwrap();
        assert_eq!(first_hello(&pool, &id).await.unwrap(), "paris");
        assert_eq!(count(&pool, "SELECT count(*) FROM servers").await, 1);
    }

    #[sqlx::test]
    async fn concurrent_hosts_with_one_name_get_distinct_ids(pool: PgPool) {
        let pki = Pki::new();
        let key = install_key(&pki, &pool, Some(3), false).await;
        let tasks: Vec<_> = (0..4)
            .map(|_| {
                let (enroller, key) = (pki.enroller(&pool), key.clone());
                tokio::spawn(async move {
                    let csr = csr(&KeyPair::generate().unwrap(), "x", vec![]);
                    enroller.enroll_host(&key, &csr, "web", None).await.map(|e| e.identity.id().to_owned())
                })
            })
            .collect();
        let mut ids = Vec::new();
        for task in tasks {
            if let Ok(id) = task.await.unwrap() {
                ids.push(id);
            }
        }
        ids.sort();
        assert_eq!(ids, ["web", "web-2", "web-3"]);
    }

    #[sqlx::test]
    async fn an_install_key_enrolls_over_the_wire(pool: PgPool) {
        let pki = Pki::new();
        let dir = TempDir::new().unwrap();
        let server = Server::start(&pki, &pool, FailureLimiter::default(), dir.path()).await;
        let key = install_key(&pki, &pool, None, false).await;
        let csr = csr(&KeyPair::generate().unwrap(), "x", vec![]);
        let reply = server.enroll_host(&key, &csr, "Web-02.example").await.unwrap();
        assert_eq!(reply.peer_uri, "monitorflow://agent/web-02.example");
        let again = server.enroll_host(&key, &csr, "Web-02.example").await.unwrap();
        assert_eq!(again.peer_uri, "monitorflow://agent/web-02.example-2");
        let old = server.enroll(&key, &csr).await.unwrap_err();
        assert_eq!(old.code(), Code::InvalidArgument);
    }

    #[sqlx::test]
    async fn relay_enrollment_creates_its_row_in_the_token_site(pool: PgPool) {
        let pki = Pki::new();
        enroll_as(&pki, &pool, PeerRole::Relay, "relay-1", "paris").await.unwrap();
        let (site, revoked): (String, bool) = sqlx::query_as(
            "SELECT si.name, r.revoked_at IS NOT NULL FROM relays r JOIN sites si ON si.id = r.site_id
             WHERE r.peer_id = 'relay-1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((site.as_str(), revoked), ("paris", false));
        assert_eq!(count(&pool, "SELECT count(*) FROM audit_log WHERE action = 'relay.create' AND target_id = 'relay-1'").await, 1);

        // An enrolled agent of its site may be relayed before its first Hello.
        enroll_as(&pki, &pool, PeerRole::Agent, "web-01", "paris").await.unwrap();
        enroll_as(&pki, &pool, PeerRole::Agent, "web-02", "lyon").await.unwrap();
        assert!(db::relay_may_speak_for(&pool, "relay-1", "web-01").await.unwrap());
        assert!(!db::relay_may_speak_for(&pool, "relay-1", "web-02").await.unwrap());

        // Enrolling again in the same site keeps the row and does not audit
        // a second creation.
        enroll_as(&pki, &pool, PeerRole::Relay, "relay-1", "paris").await.unwrap();
        assert_eq!(count(&pool, "SELECT count(*) FROM relays").await, 1);
        assert_eq!(count(&pool, "SELECT count(*) FROM audit_log WHERE action = 'relay.create'").await, 1);
    }

    #[sqlx::test]
    async fn enrollment_cannot_move_a_peer_and_brings_a_removed_one_back(pool: PgPool) {
        let pki = Pki::new();
        enroll_as(&pki, &pool, PeerRole::Relay, "relay-1", "paris").await.unwrap();
        refused(enroll_as(&pki, &pool, PeerRole::Relay, "relay-1", "lyon").await);

        let paris = token::site_id(&pool, "paris", false).await.unwrap().unwrap();
        sqlx::query("INSERT INTO servers (peer_id, site_id, hostname, os_family) VALUES ('web-01', $1, 'web-01', 'debian')")
            .bind(paris)
            .execute(&pool)
            .await
            .unwrap();
        refused(enroll_as(&pki, &pool, PeerRole::Agent, "web-01", "lyon").await);
        enroll_as(&pki, &pool, PeerRole::Agent, "web-01", "paris").await.unwrap();

        sqlx::query("UPDATE servers SET revoked_at = now()").execute(&pool).await.unwrap();
        sqlx::query("UPDATE relays SET revoked_at = now()").execute(&pool).await.unwrap();
        // Removed from paris: another site still cannot take them.
        refused(enroll_as(&pki, &pool, PeerRole::Agent, "web-01", "lyon").await);
        refused(enroll_as(&pki, &pool, PeerRole::Relay, "relay-1", "lyon").await);
        // Refusals roll back: only the two accepted enrollments issued one.
        assert_eq!(count(&pool, "SELECT count(*) FROM certificates").await, 2);

        enroll_as(&pki, &pool, PeerRole::Agent, "web-01", "paris").await.unwrap();
        enroll_as(&pki, &pool, PeerRole::Relay, "relay-1", "paris").await.unwrap();
        for table in ["servers", "relays"] {
            let live = count(&pool, &format!("SELECT count(*) FROM {table} WHERE revoked_at IS NULL")).await;
            assert_eq!(live, 1, "{table}");
        }
        assert_eq!(count(&pool, "SELECT count(*) FROM audit_log WHERE action IN ('server.restore', 'relay.restore')").await, 2);
    }

    #[sqlx::test]
    async fn an_install_key_brings_a_removed_host_back_under_its_id(pool: PgPool) {
        let pki = Pki::new();
        let paris = site(&pool).await;
        // Enrolled with a token under another ID than its host name.
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family, revoked_at, last_seen_at)
             VALUES ('agent-7', $1, 'web-07', 'debian', now(), '2026-01-01Z'),
                    ('web-08', $1, 'web-08', 'debian', NULL, NULL)
             RETURNING id",
        )
        .bind(paris)
        .fetch_one(&pool)
        .await
        .unwrap();
        let key = install_key(&pki, &pool, None, false).await;
        assert_eq!(enroll_host(&pki, &pool, &key, "WEB-07").await.unwrap(), "agent-7");
        let (back, seen): (Uuid, DateTime<Utc>) = sqlx::query_as(
            "SELECT id, last_seen_at FROM servers WHERE peer_id = 'agent-7' AND revoked_at IS NULL",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(back, id);
        // Just back: no offline incident before its first Hello.
        assert!(Utc::now() - seen < chrono::Duration::minutes(1));
        assert_eq!(count(&pool, "SELECT count(*) FROM servers").await, 2);
        first_hello(&pool, "agent-7").await.unwrap();
        // A server in use still gets a new ID.
        assert_eq!(enroll_host(&pki, &pool, &key, "web-08").await.unwrap(), "web-08-2");
    }

    #[sqlx::test]
    async fn a_removed_host_in_another_site_gets_a_new_id(pool: PgPool) {
        let pki = Pki::new();
        let lyon = token::site_id(&pool, "lyon", true).await.unwrap().unwrap();
        sqlx::query(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family, revoked_at)
             VALUES ('web-01', $1, 'web-01', 'debian', now())",
        )
        .bind(lyon)
        .execute(&pool)
        .await
        .unwrap();
        let key = install_key(&pki, &pool, None, false).await;
        assert_eq!(enroll_host(&pki, &pool, &key, "web-01").await.unwrap(), "web-01-2");
    }

    #[sqlx::test]
    async fn a_removed_host_waits_for_approval_before_it_comes_back(pool: PgPool) {
        let pki = Pki::new();
        sqlx::query(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family, revoked_at)
             VALUES ('web-01', $1, 'web-01', 'debian', now())",
        )
        .bind(site(&pool).await)
        .execute(&pool)
        .await
        .unwrap();
        let key = install_key(&pki, &pool, None, true).await;
        assert_eq!(enroll_host(&pki, &pool, &key, "web-01").await.unwrap(), "web-01");
        assert_eq!(count(&pool, "SELECT count(*) FROM servers WHERE revoked_at IS NULL").await, 0);
        assert!(matches!(first_hello(&pool, "web-01").await, Err(InventoryError::AwaitingApproval)));
        // A second try does not collide with the waiting row.
        assert_eq!(enroll_host(&pki, &pool, &key, "web-01").await.unwrap(), "web-01-2");
    }

    #[sqlx::test]
    async fn sites_are_created_only_on_request(pool: PgPool) {
        assert_eq!(token::site_id(&pool, "paris", false).await.unwrap(), None);
        let created = site(&pool).await;
        assert_eq!(token::site_id(&pool, "paris", false).await.unwrap(), Some(created));
        assert_eq!(site(&pool).await, created);
    }

    #[sqlx::test]
    async fn create_stores_only_the_hash_and_audits(pool: PgPool) {
        let site_id = site(&pool).await;
        let created = token::create(&pool, CA, &request(site_id, "web-01")).await.unwrap();
        assert_eq!(created.token.ca_fingerprint(), &CA);

        let (hash, role, peer_id, created_by, ttl): (Vec<u8>, String, String, Option<Uuid>, f64) =
            sqlx::query_as(
                "SELECT token_hash, role, peer_id, created_by,
                        extract(epoch FROM expires_at - created_at)::float8
                 FROM enrollment_tokens WHERE id = $1",
            )
            .bind(created.id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(hash, token::hash(&created.token));
        assert_ne!(hash.as_slice(), created.token.secret());
        assert_eq!((role.as_str(), peer_id.as_str(), created_by), ("agent", "web-01", None));
        assert!((ttl - 3600.0).abs() < 1.0, "{ttl}");

        let (actor_kind, actor_id, action, target, details): (String, String, String, String, String) =
            sqlx::query_as("SELECT actor_kind, actor_id, action, target_kind || '/' || target_id, details::text FROM audit_log")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            (actor_kind.as_str(), actor_id.as_str(), action.as_str(), target.as_str()),
            ("system", "cli", "enrollment_token.create", "agent/web-01")
        );
        assert!(details.contains(&created.id.to_string()), "{details}");
        let secret = created.token.to_secret_string();
        assert!(!details.contains(secret.expose()), "{details}");
    }

    #[sqlx::test]
    async fn tokens_record_their_user(pool: PgPool) {
        let site_id = site(&pool).await;
        let user: Uuid = sqlx::query_scalar(
            "INSERT INTO users (username, password_hash) VALUES ('alice', '$argon2id$x') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let mut req = request(site_id, "relay-1");
        req.role = PeerRole::Relay;
        req.creator = Creator::User(user);
        req.ttl = Duration::from_secs(120);
        let created = token::create(&pool, CA, &req).await.unwrap();
        let row: (String, Option<Uuid>) =
            sqlx::query_as("SELECT role, created_by FROM enrollment_tokens WHERE id = $1")
                .bind(created.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(row, ("relay".into(), Some(user)));
        let actor: String = sqlx::query_scalar("SELECT actor_kind || '/' || actor_id FROM audit_log")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(actor, format!("user/{user}"));
    }

    #[sqlx::test]
    async fn invalid_requests_store_nothing(pool: PgPool) {
        let site_id = site(&pool).await;
        let mut req = request(site_id, "central");
        req.role = PeerRole::Server;
        assert!(matches!(token::create(&pool, CA, &req).await, Err(TokenError::Invalid(_))));
        // Unknown site: the foreign key rejects it and nothing is audited.
        assert!(matches!(
            token::create(&pool, CA, &request(Uuid::new_v4(), "web-01")).await,
            Err(TokenError::Db(_))
        ));
        let rows: i64 = sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM enrollment_tokens) + (SELECT count(*) FROM audit_log)",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(rows, 0);
    }

    #[sqlx::test]
    async fn enroll_imposes_the_token_identity(pool: PgPool) {
        let pki = Pki::new();
        let token = pki.token(&pool, "web-01").await;
        let key = KeyPair::generate().unwrap();
        // The CSR asks to be the server; the token says agent web-01.
        let csr = csr(&key, "evil", vec![SanType::URI("monitorflow://server/central".try_into().unwrap())]);
        let source = Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)));
        let enrolled = pki.enroller(&pool).enroll(&token, &csr, source).await.unwrap();
        assert_eq!(enrolled.identity.uri(), "monitorflow://agent/web-01");
        assert_eq!(identity_of(&enrolled.cert.cert_pem), enrolled.identity);

        let (serial, role, peer_id): (Vec<u8>, String, String) =
            sqlx::query_as("SELECT serial, peer_role, peer_id FROM certificates").fetch_one(&pool).await.unwrap();
        assert_eq!((serial.as_slice(), role.as_str(), peer_id.as_str()), (enrolled.cert.serial.as_bytes(), "agent", "web-01"));
        let used_by: Option<String> = sqlx::query_scalar("SELECT used_by_peer_id FROM enrollment_tokens").fetch_one(&pool).await.unwrap();
        assert_eq!(used_by.as_deref(), Some("web-01"));
        let (actor, details): (String, String) = sqlx::query_as(
            "SELECT actor_kind || '/' || actor_id, details::text FROM audit_log WHERE action = 'peer.enroll'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(actor, "peer/monitorflow://agent/web-01");
        assert!(details.contains(&enrolled.cert.serial.to_string()) && details.contains("192.0.2.1"), "{details}");
        assert!(!details.contains(&token), "{details}");
    }

    #[sqlx::test]
    async fn token_is_single_use_under_concurrency(pool: PgPool) {
        let pki = Pki::new();
        let token = pki.token(&pool, "web-01").await;
        let enroller = pki.enroller(&pool);
        let csr = csr(&KeyPair::generate().unwrap(), "web-01", vec![]);
        let attempts: Vec<_> = (0..16)
            .map(|_| {
                let (enroller, token, csr) = (enroller.clone(), token.clone(), csr.clone());
                tokio::spawn(async move { enroller.enroll(&token, &csr, None).await })
            })
            .collect();
        let mut ok = 0;
        for attempt in attempts {
            match attempt.await.unwrap() {
                Ok(_) => ok += 1,
                Err(err) => assert!(matches!(err, EnrollError::Refused), "{err:?}"),
            }
        }
        assert_eq!(ok, 1);
        assert_eq!(count(&pool, "SELECT count(*) FROM certificates").await, 1);
        assert_eq!(count(&pool, "SELECT count(*) FROM audit_log WHERE action = 'peer.enroll'").await, 1);
        refused(enroller.enroll(&token, &csr, None).await);
    }

    #[sqlx::test]
    async fn expired_unknown_and_foreign_tokens_get_the_same_answer(pool: PgPool) {
        let pki = Pki::new();
        let enroller = pki.enroller(&pool);
        let csr = csr(&KeyPair::generate().unwrap(), "x", vec![]);

        let expired = pki.token(&pool, "web-01").await;
        sqlx::query("UPDATE enrollment_tokens SET created_at = now() - interval '2 hours', expires_at = now() - interval '1 second'")
            .execute(&pool)
            .await
            .unwrap();
        refused(enroller.enroll(&expired, &csr, None).await);

        let unknown = token::generate(pki.fingerprint()).unwrap().to_secret_string();
        refused(enroller.enroll(unknown.expose(), &csr, None).await);
        refused(enroller.enroll("not a token", &csr, None).await);

        // A valid token, but pinned to another CA.
        let valid = pki.token(&pool, "web-02").await;
        let secret = *EnrollmentToken::parse(&valid).unwrap().secret();
        let foreign = EnrollmentToken::new(secret, [0; 32]).to_secret_string();
        refused(enroller.enroll(foreign.expose(), &csr, None).await);
        // None of this used the valid token.
        enroller.enroll(&valid, &csr, None).await.unwrap();
        assert_eq!(count(&pool, "SELECT count(*) FROM certificates").await, 1);
    }

    #[sqlx::test]
    async fn invalid_csr_leaves_the_token_usable(pool: PgPool) {
        let pki = Pki::new();
        let token = pki.token(&pool, "web-01").await;
        let enroller = pki.enroller(&pool);
        let err = enroller.enroll(&token, "-----BEGIN CERTIFICATE REQUEST-----\nAAAA\n-----END CERTIFICATE REQUEST-----\n", None).await.unwrap_err();
        assert!(matches!(err, EnrollError::InvalidCsr), "{err:?}");
        let csr = csr(&KeyPair::generate().unwrap(), "web-01", vec![]);
        enroller.enroll(&token, &csr, None).await.unwrap();
    }

    #[sqlx::test]
    async fn enrolled_agent_opens_the_mtls_stream(pool: PgPool) {
        let pki = Pki::new();
        let dir = TempDir::new().unwrap();
        let server = Server::start(&pki, &pool, FailureLimiter::default(), dir.path()).await;
        let token = pki.token(&pool, "web-01").await;

        let key = KeyPair::generate().unwrap();
        let reply = server.enroll(&token, &csr(&key, "web-01", vec![])).await.unwrap();
        assert_eq!(reply.peer_uri, "monitorflow://agent/web-01");
        assert!(reply.not_after_unix > 0);
        // The agent checks the returned CA against its pin before storing it.
        let ca_der = CertificateDer::from(pem::parse(&reply.ca_cert_pem).unwrap().into_contents());
        assert_eq!(&tls::fingerprint(&ca_der), EnrollmentToken::parse(&token).unwrap().ca_fingerprint());

        // The token is spent, over the wire too.
        let again = server.enroll(&token, &csr(&key, "web-01", vec![])).await.unwrap_err();
        assert_eq!((again.code(), again.message()), (Code::PermissionDenied, "enrollment refused"));

        let agent = write_identity(dir.path(), "agent", &reply.ca_cert_pem, &reply.cert_pem, &key.serialize_pem());
        let channel = tls::connect(&server.grpc_url, agent.client_config().unwrap(), TIMEOUT).await.unwrap();
        let (tx, rx) = mpsc::channel(4);
        let hello = Hello {
            agent_id: "web-01".into(),
            protocol_version: PROTOCOL_VERSION.into(),
            agent_version: "0.1.0".into(),
            hostname: "web-01.example".into(),
            os_family: "debian".into(),
            ..Default::default()
        };
        tx.send(Envelope { sequence: 1, payload: Some(Payload::Hello(hello)) }).await.unwrap();
        let _stream = tokio::time::timeout(TIMEOUT, AgentServiceClient::new(channel).stream(ReceiverStream::new(rx)))
            .await
            .unwrap()
            .unwrap();
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        while server.registry.get("web-01").is_none() {
            assert!(tokio::time::Instant::now() < deadline, "web-01 never registered");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(server.registry.get("web-01").unwrap().role, PeerRole::Agent);

        // The Hello created the inventory row: the API lists it, online.
        let servers = list_servers(&pool, server.registry.clone()).await;
        let servers = servers["servers"].as_array().unwrap();
        assert_eq!(servers.len(), 1, "{servers:?}");
        assert_eq!(servers[0]["peer_id"], "web-01");
        assert_eq!(servers[0]["hostname"], "web-01.example");
        assert_eq!(servers[0]["site_name"], "paris");
        assert_eq!(servers[0]["os_family"], "debian");
        assert_eq!(servers[0]["status"], "online");
    }

    #[sqlx::test]
    async fn failed_attempts_are_bounded(pool: PgPool) {
        let pki = Pki::new();
        let dir = TempDir::new().unwrap();
        let limiter = FailureLimiter::new(2, Duration::from_secs(600), 100);
        let server = Server::start(&pki, &pool, limiter, dir.path()).await;
        let token = pki.token(&pool, "web-01").await;
        let csr = csr(&KeyPair::generate().unwrap(), "web-01", vec![]);
        let wrong = token::generate(pki.fingerprint()).unwrap().to_secret_string();
        for _ in 0..2 {
            let status = server.enroll(wrong.expose(), &csr).await.unwrap_err();
            assert_eq!(status.code(), Code::PermissionDenied);
        }
        // Even the right token is turned away until the window ends.
        let status = server.enroll(&token, &csr).await.unwrap_err();
        assert_eq!(status.code(), Code::ResourceExhausted);
        assert_eq!(count(&pool, "SELECT count(*) FROM enrollment_tokens WHERE used_at IS NULL").await, 1);
    }
}
