//! Signing in through an OpenID Connect provider (ADR-0145) against a fake
//! provider served over TLS with a test certificate authority, signing ES256
//! with keys made at run time: the
//! sign-in, accounts and roles from groups, refusals, key rotation, the
//! settings API and its test, and the local administrator that always
//! stays. Real providers and directories are in `deploy/sso-test.sh`.
//! Database tests are ignored without `DATABASE_URL`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::{users, v1, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair};
use ring::rand::SystemRandom;
use ring::signature::{self, EcdsaKeyPair, KeyPair as _};
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
use tower::ServiceExt;
use uuid::Uuid;

const CLIENT_ID: &str = "monitorflow";
const CLIENT_SECRET: &str = "client-secret-1";
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

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// A code the fake will trade, as the provider would after a sign-in.
#[derive(Clone)]
struct Grant {
    challenge: String,
    nonce: String,
    claims: Value,
}

#[derive(Default)]
struct Fake {
    issuer: Mutex<String>,
    kid: Mutex<String>,
    /// The signing key of `kid`: a new one at each rotation.
    key: Mutex<Vec<u8>>,
    codes: Mutex<HashMap<String, Grant>>,
    /// Signs with another key than the one published.
    forge: Mutex<bool>,
    /// Groups only at userinfo.
    userinfo_groups: Mutex<Option<Value>>,
    token_requests: Mutex<u32>,
    jwks_requests: Mutex<u32>,
}

type Shared = Arc<Fake>;

const ES256: &signature::EcdsaSigningAlgorithm = &signature::ECDSA_P256_SHA256_FIXED_SIGNING;

impl Fake {
    /// Signs with a new key from now on, published as `kid`.
    fn rotate(&self, kid: &str) {
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(ES256, &SystemRandom::new()).unwrap();
        *self.key.lock().unwrap() = pkcs8.as_ref().to_vec();
        *self.kid.lock().unwrap() = kid.into();
    }

    fn signing_key(&self) -> EcdsaKeyPair {
        EcdsaKeyPair::from_pkcs8(ES256, &self.key.lock().unwrap(), &SystemRandom::new()).unwrap()
    }
}

async fn discovery(State(f): State<Shared>) -> Json<Value> {
    let issuer = f.issuer.lock().unwrap().clone();
    Json(json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/authorize"),
        "token_endpoint": format!("{issuer}/token"),
        "jwks_uri": format!("{issuer}/jwks"),
        "userinfo_endpoint": format!("{issuer}/userinfo"),
        "code_challenge_methods_supported": ["S256"],
        "id_token_signing_alg_values_supported": ["ES256"],
        "scopes_supported": ["openid", "profile", "email", "groups"],
    }))
}

async fn jwks(State(f): State<Shared>) -> Json<Value> {
    *f.jwks_requests.lock().unwrap() += 1;
    let key = f.signing_key();
    let point = key.public_key().as_ref();
    Json(json!({ "keys": [
        { "kty": "EC", "crv": "P-256", "kid": *f.kid.lock().unwrap(), "use": "sig", "alg": "ES256",
          "x": b64(&point[1..33]), "y": b64(&point[33..]) },
        { "kty": "oct", "k": "c2VjcmV0" },
    ]}))
}

fn sign(f: &Fake, claims: &Value) -> String {
    let header = json!({ "alg": "ES256", "typ": "JWT", "kid": *f.kid.lock().unwrap() });
    let input = format!(
        "{}.{}",
        b64(header.to_string().as_bytes()),
        b64(claims.to_string().as_bytes())
    );
    let mut sig = f
        .signing_key()
        .sign(&SystemRandom::new(), input.as_bytes())
        .unwrap()
        .as_ref()
        .to_vec();
    if *f.forge.lock().unwrap() {
        sig[10] ^= 0xff;
    }
    format!("{input}.{}", b64(&sig))
}

fn form(body: &str) -> HashMap<String, String> {
    body.split('&')
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| {
            let decode = |s: &str| {
                percent_encoding::percent_decode_str(&s.replace('+', " "))
                    .decode_utf8()
                    .unwrap()
                    .into_owned()
            };
            (decode(k), decode(v))
        })
        .collect()
}

async fn token(State(f): State<Shared>, headers: HeaderMap, body: String) -> Response {
    *f.token_requests.lock().unwrap() += 1;
    let expected = format!(
        "Basic {}",
        STANDARD.encode(format!("{CLIENT_ID}:{CLIENT_SECRET}"))
    );
    if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some(expected.as_str()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "invalid_client" })),
        )
            .into_response();
    }
    let form = form(&body);
    let Some(grant) = f.codes.lock().unwrap().remove(&form["code"]) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid_grant" })),
        )
            .into_response();
    };
    let challenge =
        b64(ring::digest::digest(&ring::digest::SHA256, form["code_verifier"].as_bytes()).as_ref());
    if challenge != grant.challenge || form["grant_type"] != "authorization_code" {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid_grant" })),
        )
            .into_response();
    }
    let issuer = f.issuer.lock().unwrap().clone();
    let now = chrono::Utc::now().timestamp();
    let mut claims = json!({
        "iss": issuer, "aud": CLIENT_ID, "iat": now, "exp": now + 300, "nonce": grant.nonce,
    });
    for (k, v) in grant.claims.as_object().unwrap() {
        claims[k] = v.clone();
    }
    Json(json!({ "access_token": "at-1", "token_type": "Bearer", "id_token": sign(&f, &claims) }))
        .into_response()
}

async fn userinfo(State(f): State<Shared>, headers: HeaderMap) -> Response {
    if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some("Bearer at-1") {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let groups = f.userinfo_groups.lock().unwrap().clone();
    Json(json!({ "sub": "u-alice", "groups": groups })).into_response()
}

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

struct Provider {
    fake: Shared,
    issuer: String,
    ca_pem: String,
}

async fn start_provider() -> Provider {
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
    let fake = Arc::new(Fake::default());
    fake.rotate("k1");
    let app = Router::new()
        .route("/realm/.well-known/openid-configuration", get(discovery))
        .route("/realm/jwks", get(jwks))
        .route("/realm/token", post(token))
        .route("/realm/userinfo", get(userinfo))
        .with_state(fake.clone());
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("https://{}/realm", tcp.local_addr().unwrap());
    *fake.issuer.lock().unwrap() = issuer.clone();
    let listener = TlsListener {
        tcp,
        acceptor: TlsAcceptor::from(Arc::new(tls)),
    };
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Provider {
        fake,
        issuer,
        ca_pem: ca.pem(),
    }
}

fn app(pool: &PgPool) -> Router {
    monitorflow_server::secrets::install_for_tests();
    v1::app(ApiState::new(
        pool.clone(),
        Registry::default(),
        ApiSettings::default(),
    ))
}

struct Answer {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}

impl Answer {
    fn cookies(&self) -> Vec<String> {
        self.headers
            .get_all("set-cookie")
            .iter()
            .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned())
            .collect()
    }

    fn location(&self) -> &str {
        self.headers.get("location").unwrap().to_str().unwrap()
    }
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    cookie: Option<&str>,
) -> Answer {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(CSRF_HEADER, "1");
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    let request = match body {
        Some(body) => request
            .header("content-type", "application/json")
            .body(Body::from(body.to_string())),
        None => request.body(Body::empty()),
    }
    .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    Answer {
        status,
        headers,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    }
}

/// The local administrator, signed in: `(id, cookie)`.
async fn admin(app: &Router, pool: &PgPool) -> (Uuid, String) {
    let id = users::create_user(
        pool,
        "root",
        &Secret::new(PASSWORD),
        Role::Administrator,
        "test",
    )
    .await
    .unwrap();
    let answer = send(
        app,
        "POST",
        "/api/auth/login",
        Some(json!({ "username": "root", "password": PASSWORD })),
        None,
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK);
    (id, answer.cookies()[0].clone())
}

fn settings(p: &Provider) -> Value {
    json!({ "kind": "oidc", "issuer": p.issuer, "client_id": CLIENT_ID, "ca_pem": p.ca_pem })
}

/// Adds the provider, turned on, with `mappings`: its ID.
async fn add(app: &Router, cookie: &str, p: &Provider, mappings: Value) -> Uuid {
    let answer = send(
        app,
        "POST",
        "/api/identity-providers",
        Some(json!({ "name": "Fake", "enabled": true, "secret": CLIENT_SECRET, "settings": settings(p) })),
        Some(cookie),
    )
    .await;
    assert_eq!(answer.status, StatusCode::CREATED, "{}", answer.body);
    let id: Uuid = answer.body["id"].as_str().unwrap().parse().unwrap();
    let answer = send(
        app,
        "PUT",
        &format!("/api/identity-providers/{id}/groups"),
        Some(json!({ "mappings": mappings })),
        Some(cookie),
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    id
}

/// What the browser does: start, the provider's sign-in, the return.
/// Returns the callback's answer.
async fn sign_in(app: &Router, p: &Provider, id: Uuid, claims: Value) -> Answer {
    let start = send(
        app,
        "GET",
        &format!("/api/auth/oidc/{id}/start"),
        None,
        None,
    )
    .await;
    assert_eq!(start.status, StatusCode::SEE_OTHER);
    let pending = start.cookies()[0].clone();
    assert!(pending.starts_with("__Host-mf_sso="), "{pending}");
    let set = start.headers.get("set-cookie").unwrap().to_str().unwrap();
    assert!(set.contains("SameSite=Lax") && set.contains("HttpOnly") && set.contains("Secure"));
    let url = reqwest::Url::parse(start.location()).unwrap();
    assert!(url
        .as_str()
        .starts_with(&format!("{}/authorize?", p.issuer)));
    let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(
        query["redirect_uri"],
        "https://localhost/api/auth/oidc/callback"
    );
    let code = Uuid::new_v4().to_string();
    p.fake.codes.lock().unwrap().insert(
        code.clone(),
        Grant {
            challenge: query["code_challenge"].clone(),
            nonce: query["nonce"].clone(),
            claims,
        },
    );
    send(
        app,
        "GET",
        &format!(
            "/api/auth/oidc/callback?code={code}&state={}",
            query["state"]
        ),
        None,
        Some(&pending),
    )
    .await
}

fn alice(groups: &[&str]) -> Value {
    json!({ "sub": "u-alice", "preferred_username": "alice", "groups": groups })
}

async fn roles(pool: &PgPool, username: &str) -> Vec<(String, Option<Uuid>, Option<Uuid>)> {
    sqlx::query_as(
        "SELECT r.role, r.customer_id, r.site_id FROM user_roles r JOIN users u ON u.id = r.user_id
         WHERE u.username = $1 ORDER BY r.role",
    )
    .bind(username)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn audited(pool: &PgPool, action: &str) -> Vec<Value> {
    sqlx::query_scalar("SELECT details FROM audit_log WHERE action = $1 ORDER BY id")
        .bind(action)
        .fetch_all(pool)
        .await
        .unwrap()
}

fn session_cookie(answer: &Answer) -> String {
    answer
        .cookies()
        .into_iter()
        .find(|c| c.starts_with("__Host-mf_session="))
        .expect("a session cookie")
}

db_tests! {
    #[sqlx::test]
    async fn the_first_sign_in_creates_the_account_with_its_role(pool: PgPool) {
        let app = app(&pool);
        let (_, cookie) = admin(&app, &pool).await;
        let p = start_provider().await;
        let id = add(&app, &cookie, &p, json!([
            { "group": "MF-Admins", "role": "administrator" },
            { "group": "helpdesk", "role": "viewer" },
        ])).await;

        let answer = sign_in(&app, &p, id, alice(&["helpdesk"])).await;
        assert_eq!(answer.status, StatusCode::SEE_OTHER);
        assert_eq!(answer.location(), "/");
        let session = session_cookie(&answer);
        assert!(answer.cookies().iter().any(|c| c == "__Host-mf_sso="), "the pending cookie is cleared");
        assert_eq!(roles(&pool, "alice").await, [("viewer".to_owned(), None, None)]);

        let me = send(&app, "GET", "/api/auth/me", None, Some(&session)).await;
        assert_eq!(me.status, StatusCode::OK);
        assert_eq!(me.body["username"], "alice");
        assert_eq!(me.body["provider"]["name"], "Fake");
        assert_eq!(me.body["provider"]["kind"], "oidc");

        let created = audited(&pool, "user.create").await;
        assert_eq!(created.last().unwrap()["via"], "oidc");
        let login = audited(&pool, "auth.login").await;
        assert_eq!(login.last().unwrap()["via"], "oidc");

        // A provider's account has no password here.
        let answer = send(&app, "POST", "/api/me/password", Some(json!({
            "current_password": "x", "new_password": "a long enough password",
        })), Some(&session)).await;
        assert_eq!(answer.status, StatusCode::CONFLICT);
        let answer = send(&app, "POST", "/api/auth/login",
            Some(json!({ "username": "alice", "password": "" })), None).await;
        assert_eq!(answer.status, StatusCode::UNAUTHORIZED);
    }

    #[sqlx::test]
    async fn the_role_follows_the_groups_and_no_group_refuses(pool: PgPool) {
        let app = app(&pool);
        let (_, cookie) = admin(&app, &pool).await;
        let p = start_provider().await;
        let id = add(&app, &cookie, &p, json!([
            { "group": "mf-admins", "role": "administrator" },
            { "group": "helpdesk", "role": "viewer" },
        ])).await;

        let first = sign_in(&app, &p, id, alice(&["helpdesk"])).await;
        let old_session = session_cookie(&first);
        let answer = sign_in(&app, &p, id, alice(&["helpdesk", "MF-ADMINS"])).await;
        assert_eq!(answer.location(), "/");
        let mut got = roles(&pool, "alice").await;
        got.sort();
        assert_eq!(got, [("administrator".to_owned(), None, None), ("viewer".to_owned(), None, None)]);
        let accounts: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE username = 'alice'")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(accounts, 1, "the same account each time");

        let answer = sign_in(&app, &p, id, alice(&["staff"])).await;
        assert_eq!(answer.location(), "/login/?sso=no_group");
        assert!(roles(&pool, "alice").await.is_empty());
        let me = send(&app, "GET", "/api/auth/me", None, Some(&old_session)).await;
        assert_eq!(me.status, StatusCode::UNAUTHORIZED, "sessions end with the last group");
        let refused = audited(&pool, "auth.login_failed").await;
        assert_eq!(refused.last().unwrap()["reason"], "no_group");
    }

    #[sqlx::test]
    async fn a_replayed_return_is_limited_per_source(pool: PgPool) {
        let app = app(&pool);
        let (_, cookie) = admin(&app, &pool).await;
        let p = start_provider().await;
        let id = add(&app, &cookie, &p, json!([{ "group": "ops", "role": "operator" }])).await;
        let start = send(&app, "GET", &format!("/api/auth/oidc/{id}/start"), None, None).await;
        let pending = start.cookies()[0].clone();
        let url = reqwest::Url::parse(start.location()).unwrap();
        let state: String = url.query_pairs().find(|(k, _)| k == "state").unwrap().1.into_owned();

        let before = *p.fake.token_requests.lock().unwrap();
        let audited_before = audited(&pool, "auth.login_failed").await.len();
        // The same cookie and state again and again, with codes the
        // provider refuses.
        for n in 0..25 {
            let answer = send(&app, "GET", &format!("/api/auth/oidc/callback?code=junk-{n}&state={state}"),
                None, Some(&pending)).await;
            assert_eq!(answer.location(), "/login/?sso=provider_error");
        }
        // Only the source's allowance reached the provider and the audit log.
        let limit = 10;
        assert_eq!(*p.fake.token_requests.lock().unwrap() - before, limit);
        let refused = audited(&pool, "auth.login_failed").await.len() - audited_before;
        assert_eq!(refused as u32, limit);
    }

    #[sqlx::test]
    async fn mappings_alike_but_for_case_are_one(pool: PgPool) {
        let app = app(&pool);
        let (_, cookie) = admin(&app, &pool).await;
        let p = start_provider().await;
        let id = add(&app, &cookie, &p, json!([
            { "group": "Équipe", "role": "operator" },
            { "group": "équipe", "role": "operator" },
        ])).await;
        let mappings: i64 = sqlx::query_scalar("SELECT count(*) FROM identity_provider_groups WHERE provider_id = $1")
            .bind(id).fetch_one(&pool).await.unwrap();
        assert_eq!(mappings, 1);
    }

    #[sqlx::test]
    async fn a_mapping_change_applies_at_once(pool: PgPool) {
        let app = app(&pool);
        let (_, cookie) = admin(&app, &pool).await;
        let p = start_provider().await;
        let id = add(&app, &cookie, &p, json!([{ "group": "ops", "role": "operator" }])).await;
        let session = session_cookie(&sign_in(&app, &p, id, alice(&["ops"])).await);

        let answer = send(&app, "PUT", &format!("/api/identity-providers/{id}/groups"),
            Some(json!({ "mappings": [{ "group": "ops", "role": "approver" }] })), Some(&cookie)).await;
        assert_eq!(answer.status, StatusCode::OK);
        assert_eq!(roles(&pool, "alice").await, [("approver".to_owned(), None, None)]);
        assert_eq!(audited(&pool, "identity_provider.groups").await.last().unwrap()["accounts_changed"], 1);

        let answer = send(&app, "PUT", &format!("/api/identity-providers/{id}/groups"),
            Some(json!({ "mappings": [] })), Some(&cookie)).await;
        assert_eq!(answer.status, StatusCode::OK);
        assert!(roles(&pool, "alice").await.is_empty());
        let me = send(&app, "GET", "/api/auth/me", None, Some(&session)).await;
        assert_eq!(me.status, StatusCode::UNAUTHORIZED);
    }

    #[sqlx::test]
    async fn groups_come_from_userinfo_when_the_token_has_none(pool: PgPool) {
        let app = app(&pool);
        let (_, cookie) = admin(&app, &pool).await;
        let p = start_provider().await;
        let id = add(&app, &cookie, &p, json!([{ "group": "ops", "role": "operator" }])).await;
        *p.fake.userinfo_groups.lock().unwrap() = Some(json!(["ops"]));
        let answer = sign_in(&app, &p, id, json!({ "sub": "u-alice", "preferred_username": "alice" })).await;
        assert_eq!(answer.location(), "/");
        assert_eq!(roles(&pool, "alice").await, [("operator".to_owned(), None, None)]);
    }

    #[sqlx::test]
    async fn forged_and_replayed_returns_are_refused(pool: PgPool) {
        let app = app(&pool);
        let (_, cookie) = admin(&app, &pool).await;
        let p = start_provider().await;
        let id = add(&app, &cookie, &p, json!([{ "group": "ops", "role": "operator" }])).await;

        // A return without the browser's cookie: login CSRF.
        let answer = send(&app, "GET", "/api/auth/oidc/callback?code=x&state=y", None, None).await;
        assert_eq!(answer.location(), "/login/?sso=expired");

        // Another state than the one this browser started with.
        let start = send(&app, "GET", &format!("/api/auth/oidc/{id}/start"), None, None).await;
        let pending = start.cookies()[0].clone();
        let answer = send(&app, "GET", "/api/auth/oidc/callback?code=x&state=other", None, Some(&pending)).await;
        assert_eq!(answer.location(), "/login/?sso=expired");

        // A cookie that does not open.
        let answer = send(&app, "GET", "/api/auth/oidc/callback?code=x&state=y", None,
            Some("__Host-mf_sso=mfs1:00000000:AAAA")).await;
        assert_eq!(answer.location(), "/login/?sso=expired");

        // A token whose signature does not verify.
        *p.fake.forge.lock().unwrap() = true;
        let answer = sign_in(&app, &p, id, alice(&["ops"])).await;
        assert_eq!(answer.location(), "/login/?sso=provider_error");
        *p.fake.forge.lock().unwrap() = false;
        assert!(roles(&pool, "alice").await.is_empty());

        // The provider says the person cancelled.
        let start = send(&app, "GET", &format!("/api/auth/oidc/{id}/start"), None, None).await;
        let pending = start.cookies()[0].clone();
        let url = reqwest::Url::parse(start.location()).unwrap();
        let state: String = url.query_pairs().find(|(k, _)| k == "state").unwrap().1.into_owned();
        let answer = send(&app, "GET", &format!("/api/auth/oidc/callback?error=access_denied&state={state}"),
            None, Some(&pending)).await;
        assert_eq!(answer.location(), "/login/?sso=cancelled");
    }

    #[sqlx::test]
    async fn rotated_keys_are_fetched_again(pool: PgPool) {
        let app = app(&pool);
        let (_, cookie) = admin(&app, &pool).await;
        let p = start_provider().await;
        let id = add(&app, &cookie, &p, json!([{ "group": "ops", "role": "operator" }])).await;
        assert_eq!(sign_in(&app, &p, id, alice(&["ops"])).await.location(), "/");
        let before = *p.fake.jwks_requests.lock().unwrap();
        p.fake.rotate("k2");
        assert_eq!(sign_in(&app, &p, id, alice(&["ops"])).await.location(), "/");
        assert_eq!(*p.fake.jwks_requests.lock().unwrap(), before + 1);
        // Another unknown key at once: not fetched again within 30 seconds.
        p.fake.rotate("k3");
        assert_eq!(sign_in(&app, &p, id, alice(&["ops"])).await.location(), "/login/?sso=provider_error");
        assert_eq!(*p.fake.jwks_requests.lock().unwrap(), before + 1);
    }

    #[sqlx::test]
    async fn an_identity_never_takes_an_existing_account(pool: PgPool) {
        let app = app(&pool);
        let (root, cookie) = admin(&app, &pool).await;
        let p = start_provider().await;
        let id = add(&app, &cookie, &p, json!([{ "group": "ops", "role": "viewer" }])).await;
        let answer = sign_in(&app, &p, id, json!({ "sub": "u-evil", "preferred_username": "ROOT", "groups": ["ops"] })).await;
        assert_eq!(answer.location(), "/login/?sso=username_taken");
        let provider: Option<Uuid> = sqlx::query_scalar("SELECT identity_provider_id FROM users WHERE id = $1")
            .bind(root).fetch_one(&pool).await.unwrap();
        assert_eq!(provider, None);
        assert_eq!(roles(&pool, "root").await, [("administrator".to_owned(), None, None)]);
    }

    #[sqlx::test]
    async fn the_local_administrator_always_stays(pool: PgPool) {
        let app = app(&pool);
        let (root, cookie) = admin(&app, &pool).await;
        let p = start_provider().await;
        let id = add(&app, &cookie, &p, json!([{ "group": "admins", "role": "administrator" }])).await;
        let session = session_cookie(&sign_in(&app, &p, id, alice(&["admins"])).await);
        let alice_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE username = 'alice'")
            .fetch_one(&pool).await.unwrap();

        // Alice administers everything through her group, yet the last
        // local administrator cannot go.
        let answer = send(&app, "DELETE", &format!("/api/users/{root}"), None, Some(&session)).await;
        assert_eq!(answer.status, StatusCode::CONFLICT, "{}", answer.body);
        let answer = send(&app, "PUT", &format!("/api/users/{root}/disabled"),
            Some(json!({ "disabled": true })), Some(&session)).await;
        assert_eq!(answer.status, StatusCode::CONFLICT);
        let answer = send(&app, "DELETE", &format!("/api/users/{root}/roles"),
            Some(json!({ "role": "administrator" })), Some(&session)).await;
        assert_eq!(answer.status, StatusCode::CONFLICT);

        // Her roles follow her groups, never a hand-made grant, and she
        // has no password to get.
        let answer = send(&app, "POST", &format!("/api/users/{alice_id}/roles"),
            Some(json!({ "role": "viewer" })), Some(&cookie)).await;
        assert_eq!(answer.status, StatusCode::CONFLICT);
        let answer = send(&app, "POST", &format!("/api/users/{alice_id}/link"), None, Some(&cookie)).await;
        assert_eq!(answer.status, StatusCode::CONFLICT);
        let users = send(&app, "GET", "/api/users", None, Some(&cookie)).await;
        let alice = users.body["users"].as_array().unwrap().iter().find(|u| u["username"] == "alice").unwrap().clone();
        assert_eq!(alice["state"], "active");
        assert_eq!(alice["provider"]["name"], "Fake");

        let list = send(&app, "GET", "/api/identity-providers", None, Some(&cookie)).await;
        assert_eq!(list.body["local_administrators"], 1);

        // The provider down: the local administrator still signs in.
        let answer = send(&app, "PUT", &format!("/api/identity-providers/{id}"),
            Some(json!({ "name": "Fake", "enabled": false, "settings": settings(&p) })), Some(&cookie)).await;
        assert_eq!(answer.status, StatusCode::OK);
        let me = send(&app, "GET", "/api/auth/me", None, Some(&session)).await;
        assert_eq!(me.status, StatusCode::UNAUTHORIZED, "turning it off ends its sessions");
        let answer = send(&app, "POST", "/api/auth/login",
            Some(json!({ "username": "root", "password": PASSWORD })), None).await;
        assert_eq!(answer.status, StatusCode::OK);
    }

    #[sqlx::test]
    async fn settings_are_tested_sealed_and_audited(pool: PgPool) {
        let app = app(&pool);
        let (_, cookie) = admin(&app, &pool).await;
        let p = start_provider().await;

        // The test reads discovery and keys and checks the client.
        let answer = send(&app, "POST", "/api/identity-providers/test",
            Some(json!({ "secret": CLIENT_SECRET, "settings": settings(&p) })), Some(&cookie)).await;
        assert_eq!(answer.status, StatusCode::OK);
        assert_eq!(answer.body["ok"], true, "{}", answer.body);
        assert_eq!(answer.body["oidc"]["keys"], 1);
        let answer = send(&app, "POST", "/api/identity-providers/test",
            Some(json!({ "secret": "wrong", "settings": settings(&p) })), Some(&cookie)).await;
        assert_eq!(answer.body["ok"], false);
        assert!(answer.body["error"].as_str().unwrap().contains("client ID or the client secret"));
        // Without the authority, the provider's certificate is not trusted.
        let mut untrusted = settings(&p);
        untrusted["ca_pem"] = Value::Null;
        let answer = send(&app, "POST", "/api/identity-providers/test",
            Some(json!({ "secret": CLIENT_SECRET, "settings": untrusted })), Some(&cookie)).await;
        assert_eq!(answer.body["ok"], false);

        // Turning on a provider whose test fails is refused.
        let answer = send(&app, "POST", "/api/identity-providers",
            Some(json!({ "name": "Fake", "enabled": true, "secret": "wrong", "settings": settings(&p) })),
            Some(&cookie)).await;
        assert_eq!(answer.status, StatusCode::BAD_REQUEST);
        // Saved off, then on with the stored secret.
        let answer = send(&app, "POST", "/api/identity-providers",
            Some(json!({ "name": "Fake", "secret": CLIENT_SECRET, "settings": settings(&p) })),
            Some(&cookie)).await;
        assert_eq!(answer.status, StatusCode::CREATED);
        assert_eq!(answer.body["has_secret"], true);
        assert_eq!(answer.body["enabled"], false);
        assert!(!answer.body.to_string().contains(CLIENT_SECRET));
        let id = answer.body["id"].as_str().unwrap().to_owned();
        let answer = send(&app, "PUT", &format!("/api/identity-providers/{id}"),
            Some(json!({ "name": "Fake", "enabled": true, "settings": settings(&p) })), Some(&cookie)).await;
        assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
        let stored: String = sqlx::query_scalar("SELECT secret FROM identity_providers")
            .fetch_one(&pool).await.unwrap();
        assert!(stored.starts_with("mfs1:"));

        // The stored secret never goes to another issuer.
        let mut moved = settings(&p);
        moved["issuer"] = json!("https://elsewhere.example.com");
        let answer = send(&app, "PUT", &format!("/api/identity-providers/{id}"),
            Some(json!({ "name": "Fake", "enabled": false, "settings": moved.clone() })), Some(&cookie)).await;
        assert_eq!(answer.status, StatusCode::BAD_REQUEST);
        assert!(answer.body["error"].as_str().unwrap().contains("another address"));
        let answer = send(&app, "POST", "/api/identity-providers/test",
            Some(json!({ "id": id, "settings": moved })), Some(&cookie)).await;
        assert_eq!(answer.status, StatusCode::BAD_REQUEST);

        // Plain LDAP is refused when saving.
        let answer = send(&app, "POST", "/api/identity-providers", Some(json!({
            "name": "Directory", "secret": "x", "settings": {
                "kind": "ldap", "url": "ldap://ldap.example.com", "bind_dn": "cn=svc",
                "user_base_dn": "dc=example" } })), Some(&cookie)).await;
        assert_eq!(answer.status, StatusCode::BAD_REQUEST);
        assert!(answer.body["error"].as_str().unwrap().contains("Plain LDAP is refused"));

        let updates = audited(&pool, "identity_provider.update").await;
        let text = updates.last().unwrap().to_string();
        assert!(!text.contains(CLIENT_SECRET));
        assert_eq!(updates[0]["new"]["enabled"], true);

        // The login page lists it; the public API leaves the sign-in out.
        let answer = send(&app, "GET", "/api/auth/providers", None, None).await;
        assert_eq!(answer.body["providers"][0]["name"], "Fake");
        assert_eq!(answer.body["providers"][0]["kind"], "oidc");
        let answer = send(&app, "GET", "/api/v1/auth/providers", None, None).await;
        assert_eq!(answer.status, StatusCode::NOT_FOUND);
    }

    #[sqlx::test]
    async fn only_global_administrators_manage_providers(pool: PgPool) {
        let app = app(&pool);
        users::create_user(&pool, "op", &Secret::new(PASSWORD), Role::Operator, "test").await.unwrap();
        let answer = send(&app, "POST", "/api/auth/login",
            Some(json!({ "username": "op", "password": PASSWORD })), None).await;
        let cookie = answer.cookies()[0].clone();
        for (method, uri) in [("GET", "/api/identity-providers"), ("POST", "/api/identity-providers/test")] {
            let body = (method == "POST").then(|| json!({ "settings": { "kind": "oidc", "issuer": "https://a", "client_id": "b" } }));
            let answer = send(&app, method, uri, body, Some(&cookie)).await;
            assert_eq!(answer.status, StatusCode::FORBIDDEN, "{method} {uri}");
        }
    }

    #[sqlx::test]
    async fn removing_a_provider_keeps_its_accounts_without_access(pool: PgPool) {
        let app = app(&pool);
        let (_, cookie) = admin(&app, &pool).await;
        let p = start_provider().await;
        let id = add(&app, &cookie, &p, json!([{ "group": "ops", "role": "operator" }])).await;
        let session = session_cookie(&sign_in(&app, &p, id, alice(&["ops"])).await);
        let answer = send(&app, "DELETE", &format!("/api/identity-providers/{id}"), None, Some(&cookie)).await;
        assert_eq!(answer.status, StatusCode::NO_CONTENT);
        assert_eq!(send(&app, "GET", "/api/auth/me", None, Some(&session)).await.status, StatusCode::UNAUTHORIZED);
        let row: (Option<Uuid>, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT identity_provider_id, external_subject, password_hash FROM users WHERE username = 'alice'")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(row, (None, None, None));
        assert!(roles(&pool, "alice").await.is_empty());
        assert_eq!(audited(&pool, "identity_provider.delete").await[0]["accounts"], 1);
    }
}
