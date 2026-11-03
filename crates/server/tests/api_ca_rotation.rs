//! The certificate authority page and its buttons (ADR-0088), and how the
//! server renews peers of every version during a change. Ignored without
//! `DATABASE_URL`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use monitorflow_common::config::TlsPaths;
use monitorflow_common::redact::Secret;
use monitorflow_common::tls::{Identity, PeerIdentity, PeerRole};
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::ca::{CaOptions, CertificateAuthority, ServerName};
use monitorflow_server::ca_rotation::{self, Rotator};
use monitorflow_server::crl::CrlPublisher;
use monitorflow_server::enroll::server_cert;
use monitorflow_server::grpc::Registry;
use monitorflow_server::renew::{hex, Ask, PresentedCert, RenewError, Renewer};
use rustls::pki_types::CertificateDer;
use serde_json::{json, Value};
use sqlx::PgPool;
use tempfile::TempDir;
use tower::ServiceExt;

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
    ca: Arc<CertificateAuthority>,
    rotator: Rotator,
    _dir: TempDir,
}

impl Harness {
    async fn new(pool: &PgPool) -> Self {
        let dir = TempDir::new().unwrap();
        let ca = Arc::new(
            CertificateAuthority::open_or_create(&dir.path().join("ca"), CaOptions::default())
                .unwrap(),
        );
        let paths = TlsPaths {
            ca_cert: dir.path().join("ca.pem"),
            cert: dir.path().join("cert.pem"),
            key: dir.path().join("key.pem"),
        };
        let names = vec![ServerName::Dns("localhost".into())];
        server_cert::ensure(&ca, &paths, &names).unwrap();
        let identity = Identity::load(&paths).unwrap();
        let (crl, _rx) = CrlPublisher::start(ca.clone(), identity).await.unwrap();
        let rotator = Rotator::new(pool.clone(), ca.clone(), Arc::new(crl), paths, names);
        Self {
            pool: pool.clone(),
            ca,
            rotator,
            _dir: dir,
        }
    }

    /// A certificate for `id`, recorded as a peer of that version gets it.
    async fn peer(&self, id: &str, takes_bundles: bool) -> (PresentedCert, String) {
        let key = rcgen::KeyPair::generate().unwrap();
        let csr = rcgen::CertificateParams::default()
            .serialize_request(&key)
            .unwrap()
            .pem()
            .unwrap();
        let identity = PeerIdentity::new(PeerRole::Agent, id).unwrap();
        let issued = self.ca.issue(&csr, &identity, None, &[]).unwrap();
        let trusted = takes_bundles.then(|| vec![hex(&issued.issuer)]);
        sqlx::query(
            "INSERT INTO certificates (serial, peer_role, peer_id, not_before, not_after, issuer_ca, trusted_cas)
             VALUES ($1, 'agent', $2, now(), now() + interval '7 days', NULL, $3)",
        )
        .bind(issued.serial.as_bytes())
        .bind(id)
        .bind(trusted)
        .execute(&self.pool)
        .await
        .unwrap();
        let der = CertificateDer::from(pem::parse(&issued.cert_pem).unwrap().into_contents());
        (PresentedCert::from_der(&der).unwrap(), csr)
    }

    fn state(&self) -> ApiState {
        ApiState::new(
            self.pool.clone(),
            Registry::default(),
            ApiSettings::default(),
        )
        .with_ca_rotation(self.rotator.clone())
    }

    async fn send(&self, request: Request<Body>) -> (StatusCode, Value) {
        let response = api::router(self.state()).oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn get(&self, cookie: &str) -> (StatusCode, Value) {
        let request = Request::get("/api/certificate-authority")
            .header("cookie", cookie)
            .body(Body::empty())
            .unwrap();
        self.send(request).await
    }

    async fn post(&self, uri: &str, cookie: &str, body: Value) -> (StatusCode, Value) {
        let request = Request::post(uri)
            .header(CSRF_HEADER, "1")
            .header("content-type", "application/json")
            .header("cookie", cookie)
            .body(Body::from(body.to_string()))
            .unwrap();
        self.send(request).await
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
        let body = json!({ "username": name, "password": PASSWORD }).to_string();
        let request = Request::post("/api/auth/login")
            .header(CSRF_HEADER, "1")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let response = api::router(self.state()).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned()
    }
}

db_tests! {
    #[sqlx::test(migrations = "./migrations")]
    async fn administrators_see_the_ca_and_change_it(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let (_current, _) = h.peer("web-01", true).await;
        let (_old, _) = h.peer("web-02", false).await;
        h.rotator.backfill().await.unwrap();
        let customer: uuid::Uuid = sqlx::query_scalar("INSERT INTO customers (name) VALUES ('acme') RETURNING id").fetch_one(&pool).await.unwrap();
        let boss = h.login("boss", Role::Administrator, GrantScope::Customer(customer)).await;
        let root = h.login("root", Role::Administrator, GrantScope::Global).await;

        // Only global administrators: the CA serves every customer.
        assert_eq!(h.get(&boss).await.0, StatusCode::FORBIDDEN);
        assert_eq!(h.post("/api/certificate-authority/rotation", &boss, json!({})).await.0, StatusCode::FORBIDDEN);

        let (status, view) = h.get(&root).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(view["fingerprint"], hex(&h.ca.fingerprint()));
        assert_eq!(view["expires_soon"], false);
        assert_eq!(view["rotation"], Value::Null);
        assert_eq!(view["cut_off"], json!([]));

        // One click starts a planned change; it is audited.
        let (status, view) = h.post("/api/certificate-authority/rotation", &root, json!({})).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{view}");
        let rotation = &view["rotation"];
        assert_eq!(rotation["stage"], "introduced");
        assert_eq!(rotation["emergency"], false);
        assert_eq!(rotation["total"], 2);
        assert_eq!(rotation["done"], 0);
        let too_old: Vec<_> = rotation["waiting"].as_array().unwrap().iter()
            .filter(|p| p["too_old"] == true)
            .map(|p| p["peer_id"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(too_old, ["web-02"]);
        let (status, body) = h.post("/api/certificate-authority/rotation", &root, json!({})).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        let actor: String = sqlx::query_scalar("SELECT actor_id FROM audit_log WHERE action = 'ca.rotation.start'")
            .fetch_one(&pool).await.unwrap();
        assert_ne!(actor, "ca-rotation", "the administrator, not the server");

        // "Switch now", then "Retire now", whoever has not moved.
        let (status, view) = h.post("/api/certificate-authority/rotation/next", &root, json!({})).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(view["rotation"]["stage"], "switched");
        let (status, view) = h.post("/api/certificate-authority/rotation/next", &root, json!({})).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(view["rotation"], Value::Null);
        let (status, _) = h.post("/api/certificate-authority/rotation/next", &root, json!({})).await;
        assert_eq!(status, StatusCode::CONFLICT);
        // Nobody moved: both are cut off now.
        assert_eq!(view["cut_off"].as_array().unwrap().len(), 2);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn peers_of_every_version_renew_during_a_change(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let renewer = Renewer::new(pool.clone(), h.ca.clone());
        let (current, csr) = h.peer("web-01", true).await;
        let (old, old_csr) = h.peer("web-02", false).await;
        h.rotator.backfill().await.unwrap();
        let rotation_only = Ask { rotation_only: true, ..Ask::RENEWAL };

        // Nothing to do outside a change, and the hourly limit applies.
        let err = renewer.renew_for(&current, &csr, rotation_only).await.unwrap_err();
        assert!(matches!(err, RenewError::NoRotation), "{err}");
        assert!(matches!(renewer.renew(&current, &csr).await.unwrap_err(), RenewError::TooSoon(_)));

        h.rotator.start(false, ca_rotation::Actor::System).await.unwrap();
        let bundle = h.ca.trust().bundle;
        assert_eq!(bundle.len(), 2);
        // A current peer gets both CAs at once, whatever the limit.
        let renewed = renewer.renew_for(&current, &csr, rotation_only).await.unwrap();
        assert_eq!(renewed.trust.bundle, bundle);
        let trusted: Vec<String> = sqlx::query_scalar(
            "SELECT trusted_cas FROM certificates WHERE serial = $1",
        )
        .bind(renewed.cert.serial.as_bytes())
        .fetch_one(&pool).await.unwrap();
        assert_eq!(trusted, bundle.iter().map(hex).collect::<Vec<_>>());

        // A peer too old for bundles renews as before, and is shown so.
        let ask = Ask { accepts_ca_bundle: false, ..Ask::RENEWAL };
        assert!(matches!(renewer.renew_for(&old, &old_csr, ask).await.unwrap_err(), RenewError::TooSoon(_)));
        sqlx::query("UPDATE certificates SET issued_at = now() - interval '2 hours' WHERE peer_id = 'web-02'").execute(&pool).await.unwrap();
        renewer.renew_for(&old, &old_csr, ask).await.unwrap();
        let fleet = ca_rotation::fleet(&pool, &h.ca).await.unwrap();
        let web02 = fleet.iter().find(|p| p.peer_id == "web-02").unwrap();
        assert!(web02.too_old && !web02.trusts_all);
        let web01 = fleet.iter().find(|p| p.peer_id == "web-01").unwrap();
        assert!(!web01.too_old && web01.trusts_all && web01.moved);
    }
}
