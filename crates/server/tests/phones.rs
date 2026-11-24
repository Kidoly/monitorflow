//! Phones (ADR-0186): adding one makes the session last, removing it
//! signs it out, and the person on call hears of alerts through a push
//! service, encrypted to the phone.

use std::sync::{Arc, Mutex};

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, Request, StatusCode};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chrono::{TimeDelta, Utc};
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use monitorflow_server::notify::Dispatcher;
use ring::rand::{SecureRandom, SystemRandom};
use ring::{aead, agreement, hkdf};
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::net::TcpListener;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";
const PHONES: &str = "/api/me/phones";

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

struct Answer {
    status: StatusCode,
    body: Value,
    set_cookie: Option<String>,
}

async fn call(
    pool: &PgPool,
    method: &str,
    uri: &str,
    cookie: Option<&str>,
    body: Option<Value>,
) -> Answer {
    let state = ApiState::new(pool.clone(), Registry::default(), ApiSettings::default());
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(CSRF_HEADER, "1")
        .header("content-type", "application/json");
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    let request = request
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let response = api::router(state).oneshot(request).await.unwrap();
    let status = response.status();
    let set_cookie = response
        .headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    Answer {
        status,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        set_cookie,
    }
}

async fn user(pool: &PgPool, name: &str, role: Role, scope: GrantScope) -> Uuid {
    users::create_user_with(pool, name, &Secret::new(PASSWORD), role, scope, "test")
        .await
        .unwrap()
}

async fn login(pool: &PgPool, name: &str) -> String {
    let body = json!({ "username": name, "password": PASSWORD });
    let answer = call(pool, "POST", "/api/auth/login", None, Some(body)).await;
    assert_eq!(answer.status, StatusCode::OK);
    answer
        .set_cookie
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

/// A browser's push keys, kept to read what is pushed to it.
struct Browser {
    private: agreement::EphemeralPrivateKey,
    public: Vec<u8>,
    auth: [u8; 16],
}

impl Browser {
    fn new() -> Self {
        let rng = SystemRandom::new();
        let private =
            agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng).unwrap();
        let public = private.compute_public_key().unwrap().as_ref().to_vec();
        let mut auth = [0u8; 16];
        rng.fill(&mut auth).unwrap();
        Self {
            private,
            public,
            auth,
        }
    }

    fn subscription(&self, endpoint: &str) -> Value {
        json!({
            "endpoint": endpoint,
            "expirationTime": null,
            "keys": {
                "p256dh": URL_SAFE_NO_PAD.encode(&self.public),
                "auth": URL_SAFE_NO_PAD.encode(self.auth),
            },
        })
    }

    /// Decrypts one message as the browser does (RFC 8291).
    fn read(self, body: &[u8]) -> Value {
        struct Len(usize);
        impl hkdf::KeyType for Len {
            fn len(&self) -> usize {
                self.0
            }
        }
        let expand = |salt: &[u8], ikm: &[u8], info: &[u8], len: usize| {
            let mut out = vec![0; len];
            hkdf::Salt::new(hkdf::HKDF_SHA256, salt)
                .extract(ikm)
                .expand(&[info], Len(len))
                .unwrap()
                .fill(&mut out)
                .unwrap();
            out
        };
        let salt = &body[..16];
        let id_len = usize::from(body[20]);
        let server_key = &body[21..21 + id_len];
        let peer = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, server_key);
        let secret = agreement::agree_ephemeral(self.private, &peer, |s| s.to_vec()).unwrap();
        let mut info = b"WebPush: info\0".to_vec();
        info.extend_from_slice(&self.public);
        info.extend_from_slice(server_key);
        let ikm = expand(&self.auth, &secret, &info, 32);
        let cek = expand(salt, &ikm, b"Content-Encoding: aes128gcm\0", 16);
        let nonce = expand(salt, &ikm, b"Content-Encoding: nonce\0", 12);
        let key = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_128_GCM, &cek).unwrap());
        let mut record = body[21 + id_len..].to_vec();
        let plain = key
            .open_in_place(
                aead::Nonce::try_assume_unique_for_key(&nonce).unwrap(),
                aead::Aad::empty(),
                &mut record,
            )
            .unwrap();
        serde_json::from_slice(&plain[..plain.len() - 1]).unwrap()
    }
}

/// A push service on loopback: records each request's headers and body,
/// and answers `status`.
#[derive(Clone)]
struct PushService {
    endpoint: String,
    seen: Arc<Mutex<Vec<(HeaderMap, Bytes)>>>,
    status: Arc<Mutex<StatusCode>>,
}

impl PushService {
    async fn start() -> Self {
        let seen: Arc<Mutex<Vec<(HeaderMap, Bytes)>>> = Arc::default();
        let status = Arc::new(Mutex::new(StatusCode::CREATED));
        let (store, answer) = (seen.clone(), status.clone());
        let app = axum::Router::new().route(
            "/push/{id}",
            axum::routing::post(move |headers: HeaderMap, body: Bytes| {
                let (store, answer) = (store.clone(), answer.clone());
                async move {
                    store.lock().unwrap().push((headers, body));
                    *answer.lock().unwrap()
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/push/abc", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            endpoint,
            seen,
            status,
        }
    }

    fn taken(&self) -> Vec<(HeaderMap, Bytes)> {
        std::mem::take(&mut *self.seen.lock().unwrap())
    }
}

/// Stores `browser`'s subscription on `phone` as the API would, but with a
/// loopback endpoint the API refuses.
async fn subscribe(pool: &PgPool, phone: Uuid, browser: &Browser, endpoint: &str) {
    sqlx::query(
        "UPDATE phones SET push_endpoint = $2, push_p256dh = $3, push_auth = $4 WHERE id = $1",
    )
    .bind(phone)
    .bind(endpoint)
    .bind(&browser.public)
    .bind(&browser.auth[..])
    .execute(pool)
    .await
    .unwrap();
}

async fn audit_actions(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar("SELECT action FROM audit_log WHERE action LIKE 'user.phone%' ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap()
}

db_tests! {
    #[sqlx::test]
    async fn a_phone_keeps_its_session_until_it_is_removed(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        user(&pool, "alice", Role::Operator, GrantScope::Global).await;
        user(&pool, "bob", Role::Operator, GrantScope::Global).await;
        let alice = login(&pool, "alice").await;
        let bob = login(&pool, "bob").await;

        let answer = call(&pool, "POST", PHONES, Some(&alice), Some(json!({"name": "  Pixel "}))).await;
        assert_eq!(answer.status, StatusCode::CREATED, "{}", answer.body);
        let phone = answer.body["id"].as_str().unwrap().to_owned();
        assert_eq!(answer.body["name"], "Pixel");
        assert_eq!(answer.body["this_phone"], true);
        assert_eq!(answer.body["push"], false);
        // The cookie and the session last 90 days now.
        let cookie = answer.set_cookie.unwrap();
        assert!(cookie.contains(&format!("Max-Age={}", 90 * 86_400)), "{cookie}");
        let expires: chrono::DateTime<Utc> = sqlx::query_scalar(
            "SELECT expires_at FROM sessions WHERE phone_id = $1::uuid",
        )
        .bind(&phone)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(expires > Utc::now() + TimeDelta::days(89));
        // Adding again from the same session renames it, nothing more.
        let again = call(&pool, "POST", PHONES, Some(&alice), Some(json!({"name": "Mine"}))).await;
        assert_eq!(again.body["id"].as_str(), Some(phone.as_str()));

        // A session idle longer than a browser's but not a phone's lives.
        sqlx::query("UPDATE sessions SET last_seen_at = now() - interval '2 days'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(call(&pool, "GET", "/api/auth/me", Some(&alice), None).await.status, StatusCode::OK);
        assert_eq!(call(&pool, "GET", "/api/auth/me", Some(&bob), None).await.status, StatusCode::UNAUTHORIZED);
        let bob = login(&pool, "bob").await;

        let list = call(&pool, "GET", PHONES, Some(&alice), None).await;
        assert_eq!(list.status, StatusCode::OK);
        assert_eq!(list.body["phones"].as_array().unwrap().len(), 1);
        assert_eq!(list.body["session_days"], 90);
        assert_eq!(list.body["push_key"].as_str().unwrap().len(), 87);

        // Push: only a subscription on the internet, from the phone itself.
        let browser = Browser::new();
        let push = format!("{PHONES}/{phone}/push");
        let local = call(&pool, "PUT", &push, Some(&alice), Some(browser.subscription("https://127.0.0.1/x"))).await;
        assert_eq!(local.status, StatusCode::BAD_REQUEST);
        let on = call(&pool, "PUT", &push, Some(&alice), Some(browser.subscription("https://push.example.net/send/1"))).await;
        assert_eq!(on.status, StatusCode::OK, "{}", on.body);
        assert_eq!(on.body["push"], true);
        assert_eq!(on.body["push_host"], "push.example.net");

        // Someone else's phone is not found.
        for (method, uri) in [("DELETE", format!("{PHONES}/{phone}")), ("PUT", push.clone()), ("POST", format!("{PHONES}/{phone}/test"))] {
            let body = (method == "PUT").then(|| browser.subscription("https://push.example.net/send/2"));
            assert_eq!(call(&pool, method, &uri, Some(&bob), body).await.status, StatusCode::NOT_FOUND, "{method} {uri}");
        }
        // Nor can another session of the owner subscribe for it.
        let elsewhere = login(&pool, "alice").await;
        let refused = call(&pool, "PUT", &push, Some(&elsewhere), Some(browser.subscription("https://push.example.net/send/3"))).await;
        assert_eq!(refused.status, StatusCode::BAD_REQUEST);
        assert_eq!(call(&pool, "GET", PHONES, Some(&elsewhere), None).await.body["phones"][0]["this_phone"], false);

        let off = call(&pool, "DELETE", &push, Some(&alice), None).await;
        assert_eq!(off.body["push"], false);

        // Removing it from another session signs the phone out.
        let removed = call(&pool, "DELETE", &format!("{PHONES}/{phone}"), Some(&elsewhere), None).await;
        assert_eq!(removed.status, StatusCode::NO_CONTENT);
        assert!(removed.set_cookie.is_none());
        assert_eq!(call(&pool, "GET", "/api/auth/me", Some(&alice), None).await.status, StatusCode::UNAUTHORIZED);
        assert_eq!(call(&pool, "GET", "/api/auth/me", Some(&elsewhere), None).await.status, StatusCode::OK);

        assert_eq!(
            audit_actions(&pool).await,
            ["user.phone_add", "user.phone_add", "user.phone_push_on", "user.phone_push_off", "user.phone_remove"]
        );
    }

    #[sqlx::test]
    async fn a_phone_needs_a_session_of_a_person(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        assert_eq!(call(&pool, "GET", PHONES, None, None).await.status, StatusCode::UNAUTHORIZED);
        user(&pool, "alice", Role::Viewer, GrantScope::Global).await;
        let alice = login(&pool, "alice").await;
        assert_eq!(
            call(&pool, "POST", PHONES, Some(&alice), Some(json!({"name": ""}))).await.status,
            StatusCode::BAD_REQUEST
        );
        // Ten phones at most.
        for n in 0..10 {
            let session = login(&pool, "alice").await;
            let answer = call(&pool, "POST", PHONES, Some(&session), Some(json!({"name": format!("p{n}")}))).await;
            assert_eq!(answer.status, StatusCode::CREATED);
        }
        let answer = call(&pool, "POST", PHONES, Some(&alice), Some(json!({"name": "one more"}))).await;
        assert_eq!(answer.status, StatusCode::CONFLICT);
    }

    #[sqlx::test]
    async fn the_person_on_call_hears_of_an_alert_on_their_phone(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let customer: Uuid = sqlx::query_scalar("INSERT INTO customers (name) VALUES ('acme') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
        let site: Uuid = sqlx::query_scalar("INSERT INTO sites (name, customer_id) VALUES ('paris', $1) RETURNING id")
            .bind(customer)
            .fetch_one(&pool)
            .await
            .unwrap();
        let server: Uuid = sqlx::query_scalar(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family)
             VALUES ('web-1', $1, 'web-1', 'debian') RETURNING id",
        )
        .bind(site)
        .fetch_one(&pool)
        .await
        .unwrap();
        let alice = user(&pool, "alice", Role::Operator, GrantScope::Customer(customer)).await;
        let session = login(&pool, "alice").await;
        let added = call(&pool, "POST", PHONES, Some(&session), Some(json!({"name": "Pixel"}))).await;
        let phone: Uuid = added.body["id"].as_str().unwrap().parse().unwrap();
        let service = PushService::start().await;
        let browser = Browser::new();
        subscribe(&pool, phone, &browser, &service.endpoint).await;
        // Alice has no email nor chat: her phone is how she is reached.
        let since = Utc::now() - TimeDelta::hours(12);
        let schedule: Uuid = sqlx::query_scalar(
            "INSERT INTO on_call_schedules (name, customer_id, time_zone, rotation, handoff_time,
                                            anchor_date)
             VALUES ('Ops', $1, 'UTC', 'daily', $2, $3) RETURNING id",
        )
        .bind(customer)
        .bind(since.time())
        .bind(since.date_naive())
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO on_call_members (schedule_id, position, user_id) VALUES ($1, 0, $2)")
            .bind(schedule)
            .bind(alice)
            .execute(&pool)
            .await
            .unwrap();
        let incident: Uuid = sqlx::query_scalar(
            "INSERT INTO incidents (server_id, metric, instance, severity, opened_at, last_value,
                                    last_value_at, peak_value, threshold)
             VALUES ($1, 'monitorflow_disk_used_percent', '/', 'critical', now(), 91, now(), 91, '{}')
             RETURNING id",
        )
        .bind(server)
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO incident_events (incident_id, occurred_at, kind, severity, value)
             VALUES ($1, now(), 'opened', 'critical', 91)",
        )
        .bind(incident)
        .execute(&pool)
        .await
        .unwrap();

        let dispatcher = Dispatcher::new(pool.clone(), "https://mf.example")
            .with_retries(Vec::new())
            .without_pace()
            .without_waits()
            .with_push_client(reqwest::Client::new());
        dispatcher.drain().await.unwrap();
        let taken = service.taken();
        assert_eq!(taken.len(), 1, "one notification");
        let (headers, body) = &taken[0];
        assert_eq!(headers["content-encoding"], "aes128gcm");
        assert_eq!(headers["urgency"], "high");
        assert_eq!(headers["ttl"], "14400");
        let authorization = headers["authorization"].to_str().unwrap();
        assert!(authorization.starts_with("vapid t="), "{authorization}");
        let notice = browser.read(body);
        assert_eq!(notice["incident"], json!(incident));
        assert_eq!(notice["actions"], json!(["acknowledge", "silence"]));
        assert_eq!(notice["path"], format!("/mobile/incident/?id={incident}"));
        assert!(notice["body"].as_str().unwrap().contains("web-1"), "{notice}");
        assert!(!notice.to_string().contains("token"), "no acknowledge link: {notice}");
        let sent: Option<chrono::DateTime<Utc>> = sqlx::query_scalar("SELECT push_sent_at FROM phones WHERE id = $1")
            .bind(phone)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(sent.is_some());
        // She was told: a follow-up knows it.
        let told: i64 = sqlx::query_scalar("SELECT count(*) FROM incident_told WHERE incident_id = $1")
            .bind(incident)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(told > 0);

        // A push service that forgot the phone ends its subscription.
        *service.status.lock().unwrap() = StatusCode::GONE;
        sqlx::query(
            "INSERT INTO incident_events (incident_id, occurred_at, kind, severity, value)
             VALUES ($1, now(), 'resolved', NULL, 10)",
        )
        .bind(incident)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE incidents SET state = 'resolved', resolved_at = now() WHERE id = $1")
            .bind(incident)
            .execute(&pool)
            .await
            .unwrap();
        dispatcher.drain().await.unwrap();
        assert_eq!(service.taken().len(), 1);
        let (endpoint, error): (Option<String>, Option<String>) =
            sqlx::query_as("SELECT push_endpoint, push_error FROM phones WHERE id = $1")
                .bind(phone)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(endpoint, None);
        assert!(error.unwrap().contains("no longer knows"));
    }
}
