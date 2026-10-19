//! `CertificateService::Renew` with PostgreSQL, directly and over mTLS.
//! Without `DATABASE_URL` at build time the tests are reported as ignored
//! (see `build.rs`).

use std::net::Ipv4Addr;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use monitorflow_common::config::TlsPaths;
use monitorflow_common::proto::v1::certificate_service_client::CertificateServiceClient;
use monitorflow_common::proto::v1::RenewRequest;
use monitorflow_common::tls::{self, Identity, PeerIdentity, PeerRole};
use monitorflow_server::ca::{CaOptions, CertificateAuthority, RevocationReason, ServerName};
use monitorflow_server::enroll::token::{self, Creator, TokenRequest};
use monitorflow_server::enroll::{self, Enroller, FailureLimiter};
use monitorflow_server::grpc::{self, AgentStreamService, DenyRelayedAgents, Registry};
use monitorflow_server::renew::{PresentedCert, RenewError, Renewer};
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType};
use rustls::pki_types::CertificateDer;
use sqlx::PgPool;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tonic::Code;

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

struct Pki {
    _dir: TempDir,
    ca: Arc<CertificateAuthority>,
}

/// An enrolled peer: its certificate as presented on a connection, and the
/// PEM files it stored.
struct Peer {
    presented: PresentedCert,
    cert_pem: String,
    key_pem: String,
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

    /// Enrolls `role`/`peer_id` with a token, as an administrator would.
    async fn enroll(&self, pool: &PgPool, role: PeerRole, peer_id: &str) -> Peer {
        self.enroll_with_names(pool, role, peer_id, Vec::new())
            .await
    }

    async fn enroll_with_names(
        &self,
        pool: &PgPool,
        role: PeerRole,
        peer_id: &str,
        server_names: Vec<String>,
    ) -> Peer {
        let site_id = token::site_id(pool, "paris", true).await.unwrap().unwrap();
        let request = TokenRequest {
            role,
            peer_id: peer_id.into(),
            site_id,
            ttl: token::DEFAULT_TTL,
            creator: Creator::Cli,
            server_names,
        };
        let created = token::create(pool, enroll::ca_fingerprint(&self.ca), &request)
            .await
            .unwrap();
        let key = KeyPair::generate().unwrap();
        let enrolled = Enroller::new(pool.clone(), self.ca.clone(), FailureLimiter::default())
            .enroll(
                created.token.to_secret_string().expose(),
                &csr(&key, peer_id, vec![]),
                None,
            )
            .await
            .unwrap();
        Peer {
            presented: presented(&enrolled.cert.cert_pem),
            cert_pem: enrolled.cert.cert_pem,
            key_pem: key.serialize_pem(),
        }
    }

    fn renewer(&self, pool: &PgPool) -> Renewer {
        Renewer::new(pool.clone(), self.ca.clone()).with_min_interval(Duration::ZERO)
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

fn fresh_csr() -> String {
    csr(&KeyPair::generate().unwrap(), "renewal", vec![])
}

fn presented(cert_pem: &str) -> PresentedCert {
    let der = CertificateDer::from(pem::parse(cert_pem).unwrap().into_contents());
    PresentedCert::from_der(&der).unwrap()
}

async fn count(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

fn refused<T: std::fmt::Debug>(result: Result<T, RenewError>, why: &str) {
    let err = result.unwrap_err();
    assert!(matches!(err, RenewError::Refused(_)), "{err:?}");
    assert!(err.to_string().contains(why), "{err}");
    assert_eq!(tonic::Status::from(err).code(), Code::PermissionDenied);
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

db_tests! {
    #[sqlx::test]
    async fn renewal_records_the_certificate_and_keeps_the_old_one(pool: PgPool) {
        let pki = Pki::new();
        let agent = pki.enroll(&pool, PeerRole::Agent, "web-01").await;
        let renewed = pki.renewer(&pool).renew(&agent.presented, &fresh_csr()).await.unwrap();

        assert_eq!(renewed.identity.uri(), "monitorflow://agent/web-01");
        assert_ne!(renewed.cert.serial, agent.presented.serial);
        assert_eq!(presented(&renewed.cert.cert_pem).serial, renewed.cert.serial);
        let rows: Vec<(String, String, bool)> = sqlx::query_as(
            "SELECT peer_role, peer_id, revoked_at IS NULL FROM certificates WHERE serial = $1 OR serial = $2",
        )
        .bind(renewed.cert.serial.as_bytes())
        .bind(agent.presented.serial.as_bytes())
        .fetch_all(&pool)
        .await
        .unwrap();
        // Both are recorded and the old one is not revoked.
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(rows.iter().all(|r| r == &("agent".into(), "web-01".into(), true)), "{rows:?}");
        assert!(!pki.ca.is_revoked(&agent.presented.serial));

        let audit: (String, String, String, serde_json::Value) = sqlx::query_as(
            "SELECT actor_id, target_kind, target_id, details FROM audit_log WHERE action = 'peer.renew'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((audit.0.as_str(), audit.1.as_str(), audit.2.as_str()), ("monitorflow://agent/web-01", "agent", "web-01"));
        assert_eq!(audit.3["serial"], renewed.cert.serial.to_string());
        assert_eq!(audit.3["previous_serial"], agent.presented.serial.to_string());

        // The renewed certificate can renew in turn.
        let again = presented(&renewed.cert.cert_pem);
        pki.renewer(&pool).renew(&again, &fresh_csr()).await.unwrap();
        assert_eq!(count(&pool, "SELECT count(*) FROM certificates").await, 3);
    }

    #[sqlx::test]
    async fn identity_comes_from_the_presented_certificate(pool: PgPool) {
        let pki = Pki::new();
        let agent = pki.enroll(&pool, PeerRole::Agent, "web-01").await;
        let lying = csr(
            &KeyPair::generate().unwrap(),
            "db-99",
            vec![
                SanType::URI("monitorflow://relay/db-99".try_into().unwrap()),
                SanType::DnsName("evil.example".try_into().unwrap()),
            ],
        );
        let renewed = pki.renewer(&pool).renew(&agent.presented, &lying).await.unwrap();
        let der = CertificateDer::from(pem::parse(&renewed.cert.cert_pem).unwrap().into_contents());
        assert_eq!(PeerIdentity::from_cert(&der).unwrap().uri(), "monitorflow://agent/web-01");
        let (_, cert) = x509_parser::parse_x509_certificate(&der).unwrap();
        let sans = cert.subject_alternative_name().unwrap().unwrap();
        assert_eq!(sans.value.general_names.len(), 1, "{sans:?}");
        assert_eq!(count(&pool, "SELECT count(*) FROM certificates WHERE peer_id = 'db-99'").await, 0);
    }

    #[sqlx::test]
    async fn relays_keep_the_names_of_their_token(pool: PgPool) {
        use monitorflow_server::ca::ServerName;
        let pki = Pki::new();
        let names = vec!["relay-1.paris.example".to_owned(), "10.0.0.7".to_owned()];
        let relay = pki.enroll_with_names(&pool, PeerRole::Relay, "relay-1", names).await;
        let expected = vec![
            ServerName::Dns("relay-1.paris.example".into()),
            ServerName::Ip("10.0.0.7".parse().unwrap()),
        ];
        assert_eq!(relay.presented.server_names, expected);
        let renewed = pki.renewer(&pool).renew(&relay.presented, &fresh_csr()).await.unwrap();
        assert_eq!(presented(&renewed.cert.cert_pem).server_names, expected);
    }

    #[sqlx::test]
    async fn revoked_rows_and_certificates_are_refused(pool: PgPool) {
        let pki = Pki::new();
        let renewer = pki.renewer(&pool);

        let relay = pki.enroll(&pool, PeerRole::Relay, "relay-1").await;
        renewer.renew(&relay.presented, &fresh_csr()).await.unwrap();
        sqlx::query("UPDATE relays SET revoked_at = now() WHERE peer_id = 'relay-1'")
            .execute(&pool).await.unwrap();
        refused(renewer.renew(&relay.presented, &fresh_csr()).await, "relay is revoked");

        let agent = pki.enroll(&pool, PeerRole::Agent, "web-01").await;
        let site: uuid::Uuid = sqlx::query_scalar("SELECT id FROM sites").fetch_one(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family, revoked_at)
             VALUES ('web-01', $1, 'web-01', 'debian', now())",
        )
        .bind(site)
        .execute(&pool)
        .await
        .unwrap();
        refused(renewer.renew(&agent.presented, &fresh_csr()).await, "agent is revoked");

        // Revoked certificate, in the database or in the CA's list.
        let other = pki.enroll(&pool, PeerRole::Agent, "web-02").await;
        sqlx::query("UPDATE certificates SET revoked_at = now() WHERE serial = $1")
            .bind(other.presented.serial.as_bytes())
            .execute(&pool).await.unwrap();
        refused(renewer.renew(&other.presented, &fresh_csr()).await, "revoked");
        let third = pki.enroll(&pool, PeerRole::Agent, "web-03").await;
        pki.ca.revoke(&third.presented.serial, RevocationReason::KeyCompromise).unwrap();
        refused(renewer.renew(&third.presented, &fresh_csr()).await, "revoked");

        // Two certificates from the relay, one from each enrollment.
        assert_eq!(count(&pool, "SELECT count(*) FROM certificates").await, 5);
        assert_eq!(count(&pool, "SELECT count(*) FROM audit_log WHERE action = 'peer.renew'").await, 1);
    }

    #[sqlx::test]
    async fn unknown_expired_and_server_certificates_are_refused(pool: PgPool) {
        let pki = Pki::new();
        let renewer = pki.renewer(&pool);
        let agent = pki.enroll(&pool, PeerRole::Agent, "web-01").await;

        // Issued by the CA but never recorded, as from scripts/dev-certs.sh.
        let id = PeerIdentity::new(PeerRole::Agent, "web-01").unwrap();
        let unrecorded = pki.ca.issue(&fresh_csr(), &id, None, &[]).unwrap();
        refused(renewer.renew(&presented(&unrecorded.cert_pem), &fresh_csr()).await, "not issued by this server");

        // A serial recorded for another peer.
        let mut swapped = agent.presented.clone();
        swapped.identity = PeerIdentity::new(PeerRole::Agent, "web-02").unwrap();
        refused(renewer.renew(&swapped, &fresh_csr()).await, "another peer");

        let mut expired = agent.presented.clone();
        expired.not_after = SystemTime::now() - Duration::from_secs(1);
        refused(renewer.renew(&expired, &fresh_csr()).await, "expired");
        sqlx::query("UPDATE certificates SET not_before = now() - interval '2 days', not_after = now() - interval '1 day'")
            .execute(&pool).await.unwrap();
        refused(renewer.renew(&agent.presented, &fresh_csr()).await, "expired");

        let mut server = agent.presented.clone();
        server.identity = PeerIdentity::new(PeerRole::Server, "central").unwrap();
        refused(renewer.renew(&server, &fresh_csr()).await, "server renews");

        let err = renewer.renew(&agent.presented, "not a csr").await.unwrap_err();
        assert!(matches!(err, RenewError::InvalidCsr), "{err:?}");
        assert_eq!(count(&pool, "SELECT count(*) FROM certificates").await, 1);
    }

    #[sqlx::test]
    async fn renewals_are_rate_limited_per_peer(pool: PgPool) {
        let pki = Pki::new();
        let renewer = Renewer::new(pool.clone(), pki.ca.clone());
        let agent = pki.enroll(&pool, PeerRole::Agent, "web-01").await;

        // The enrollment itself counts.
        let err = renewer.renew(&agent.presented, &fresh_csr()).await.unwrap_err();
        assert!(matches!(err, RenewError::TooSoon(_)), "{err:?}");
        assert_eq!(tonic::Status::from(err).code(), Code::ResourceExhausted);

        // Once the interval has passed, concurrent calls get one certificate.
        sqlx::query("UPDATE certificates SET issued_at = now() - interval '2 hours'")
            .execute(&pool).await.unwrap();
        let attempts = (0..4).map(|_| {
            let (renewer, presented) = (renewer.clone(), agent.presented.clone());
            tokio::spawn(async move { renewer.renew(&presented, &fresh_csr()).await })
        });
        let mut ok = 0;
        for attempt in attempts {
            match attempt.await.unwrap() {
                Ok(_) => ok += 1,
                Err(RenewError::TooSoon(_)) => {}
                Err(other) => panic!("{other:?}"),
            }
        }
        assert_eq!(ok, 1);
        assert_eq!(count(&pool, "SELECT count(*) FROM certificates").await, 2);

        // Another peer is not affected.
        let other = pki.enroll(&pool, PeerRole::Agent, "web-02").await;
        sqlx::query("UPDATE certificates SET issued_at = now() - interval '2 hours' WHERE peer_id = 'web-02'")
            .execute(&pool).await.unwrap();
        renewer.renew(&other.presented, &fresh_csr()).await.unwrap();
    }

    #[sqlx::test]
    async fn renew_over_mtls_uses_the_connection_certificate(pool: PgPool) {
        let pki = Pki::new();
        let dir = TempDir::new().unwrap();
        let key = KeyPair::generate().unwrap();
        let names = [ServerName::Dns("localhost".into()), ServerName::Ip(Ipv4Addr::LOCALHOST.into())];
        let server_id = PeerIdentity::new(PeerRole::Server, "central").unwrap();
        let cert = pki.ca.issue(&csr(&key, "central", vec![]), &server_id, None, &names).unwrap();
        let server = write_identity(dir.path(), "server", pki.ca.ca_cert_pem(), &cert.cert_pem, &key.serialize_pem());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("https://localhost:{}", listener.local_addr().unwrap().port());
        let (_, rx) = watch::channel(server.server_config(vec![pki.ca.crl().unwrap()]).unwrap());
        let service = AgentStreamService::new(Registry::default(), Arc::new(DenyRelayedAgents), Default::default());
        tokio::spawn(grpc::serve_with_renewer(listener, rx, service, Some(pki.renewer(&pool)), std::future::pending()));

        let agent = pki.enroll(&pool, PeerRole::Agent, "web-01").await;
        let identity = write_identity(dir.path(), "agent", pki.ca.ca_cert_pem(), &agent.cert_pem, &agent.key_pem);
        let channel = tls::connect(&url, identity.client_config().unwrap(), TIMEOUT).await.unwrap();
        let mut client = CertificateServiceClient::new(channel);
        let reply = client
            .renew(RenewRequest { csr_pem: csr(&KeyPair::generate().unwrap(), "web-02", vec![]) })
            .await
            .unwrap()
            .into_inner();
        assert_eq!(reply.peer_uri, "monitorflow://agent/web-01");
        assert_eq!(reply.ca_cert_pem, pki.ca.ca_cert_pem());
        assert!(reply.not_after_unix > 0);
        assert_ne!(presented(&reply.cert_pem).serial, agent.presented.serial);

        let status = client.renew(RenewRequest { csr_pem: "x".repeat(32 * 1024) }).await.unwrap_err();
        assert_eq!(status.code(), Code::OutOfRange, "{status:?}");
    }
}
