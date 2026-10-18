//! Enrollment tokens and the `Enroll` RPC with PostgreSQL, up to a real
//! mTLS stream with the issued certificate. Without `DATABASE_URL` at build
//! time the tests are reported as ignored (see `build.rs`).

use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use monitorflow_common::config::TlsPaths;
use monitorflow_common::proto::v1::agent_service_client::AgentServiceClient;
use monitorflow_common::proto::v1::enroll_service_client::EnrollServiceClient;
use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::{EnrollRequest, EnrollResponse, Envelope, Hello};
use monitorflow_common::tls::{self, EnrollmentToken, Identity, PeerIdentity, PeerRole};
use monitorflow_common::PROTOCOL_VERSION;
use monitorflow_server::ca::{CaOptions, CertificateAuthority, ServerName};
use monitorflow_server::enroll::token::{self, Creator, TokenError, TokenRequest};
use monitorflow_server::enroll::{self, EnrollError, Enroller, FailureLimiter};
use monitorflow_server::grpc::{
    self, AgentStreamService, DenyRelayedAgents, Registry, StreamSettings,
};
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType};
use rustls::pki_types::CertificateDer;
use sqlx::PgPool;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio_stream::wrappers::ReceiverStream;
use tonic::Code;
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
            Arc::new(DenyRelayedAgents),
            StreamSettings::default(),
        );
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
        };
        EnrollServiceClient::new(channel)
            .enroll(request)
            .await
            .map(tonic::Response::into_inner)
            .map_err(Box::new)
    }
}

db_tests! {
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
