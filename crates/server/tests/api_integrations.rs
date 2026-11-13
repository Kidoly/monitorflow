//! Integration endpoints through the API (ADR-0102): the secret returned
//! once, a new one for a new URL, the test button, private addresses,
//! pausing and resuming, a new secret, the delivery log, replays, and the
//! audit. Customers apart are in `api_tenancy.rs`. Ignored without `DATABASE_URL`.

use std::sync::{Arc, Mutex};

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, Request, StatusCode};
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use monitorflow_server::integrations::{Deliverer, REPLAY_HEADER};
use monitorflow_server::notify::send::{signature, SIGNATURE_HEADER, TIMESTAMP_HEADER};
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::net::TcpListener;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";
const ENDPOINTS: &str = "/api/integrations";

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

async fn call(
    pool: &PgPool,
    method: &str,
    uri: &str,
    cookie: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value, Option<String>) {
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
    let cookie = response
        .headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body, cookie)
}

async fn login(pool: &PgPool, name: &str, role: Role, scope: GrantScope) -> String {
    users::create_user_with(pool, name, &Secret::new(PASSWORD), role, scope, "test")
        .await
        .unwrap();
    let body = json!({ "username": name, "password": PASSWORD });
    let (status, _, cookie) = call(pool, "POST", "/api/auth/login", None, Some(body)).await;
    assert_eq!(status, StatusCode::OK);
    cookie.unwrap()
}

/// A customer with a site and a server: `(customer, server)`.
async fn place(pool: &PgPool, name: &str) -> (Uuid, Uuid) {
    let customer: Uuid =
        sqlx::query_scalar("INSERT INTO customers (name) VALUES ($1) RETURNING id")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap();
    let site: Uuid = sqlx::query_scalar(
        "INSERT INTO sites (name, customer_id) VALUES ($1 || '-site', $2) RETURNING id",
    )
    .bind(name)
    .bind(customer)
    .fetch_one(pool)
    .await
    .unwrap();
    let server = sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family)
         VALUES ($1 || '-web', $2, $1 || '-web', 'debian') RETURNING id",
    )
    .bind(name)
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap();
    (customer, server)
}

type Seen = Arc<Mutex<Vec<(HeaderMap, Bytes)>>>;

async fn receiver() -> (String, Seen) {
    let seen: Seen = Arc::default();
    let store = seen.clone();
    let app = axum::Router::new().route(
        "/hook",
        axum::routing::post(move |headers: HeaderMap, body: Bytes| {
            let store = store.clone();
            async move {
                store.lock().unwrap().push((headers, body));
                StatusCode::NO_CONTENT
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hook", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, seen)
}

db_tests! {
    #[sqlx::test]
    async fn an_endpoint_is_set_up_tested_and_replayed(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let (_, server) = place(&pool, "acme").await;
        let admin = login(&pool, "admin", Role::Administrator, GrantScope::Global).await;
        let (url, seen) = receiver().await;

        // A receiver on loopback is a private address: refused until a
        // global administrator allows it.
        let ask = json!({ "name": "HaloPSA", "url": url, "events": ["incident.opened"] });
        let (status, created, _) = call(&pool, "POST", ENDPOINTS, Some(&admin), Some(ask.clone())).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let secret = created["secret"].as_str().unwrap().to_owned();
        assert!(secret.starts_with("mfwh_"));
        let id = created["endpoint"]["id"].as_str().unwrap().to_owned();
        let test = format!("{ENDPOINTS}/{id}/test");
        let (_, result, _) = call(&pool, "POST", &test, Some(&admin), None).await;
        assert_eq!(result["ok"], false);
        assert!(result["error"].as_str().unwrap().contains("private address"), "{result}");

        let mut allowed = ask.clone();
        allowed["allow_private"] = json!(true);
        let (status, saved, _) =
            call(&pool, "PUT", &format!("{ENDPOINTS}/{id}"), Some(&admin), Some(allowed)).await;
        // The same URL keeps its secret.
        assert_eq!((status, &saved["secret"]), (StatusCode::OK, &Value::Null), "{saved}");
        let (_, result, _) = call(&pool, "POST", &test, Some(&admin), None).await;
        assert_eq!((&result["ok"], &result["status"]), (&json!(true), &json!(204)), "{result}");
        {
            let requests = seen.lock().unwrap();
            let (headers, body) = &requests[0];
            assert_eq!(headers["x-monitorflow-event"], "test");
            let timestamp: i64 = headers[TIMESTAMP_HEADER].to_str().unwrap().parse().unwrap();
            assert_eq!(headers[SIGNATURE_HEADER].to_str().unwrap(), signature(&secret, timestamp, body));
        }

        // The list never shows the secret.
        let (_, list, _) = call(&pool, "GET", ENDPOINTS, Some(&admin), None).await;
        assert!(!list.to_string().contains(&secret));
        assert_eq!(list["endpoints"][0]["events"], json!(["incident.opened"]));
        assert_eq!(list["events"].as_array().unwrap().len(), 16);

        // An incident is delivered, then replayed from the log with its ID.
        sqlx::query(
            "WITH i AS (
                 INSERT INTO incidents (server_id, metric, instance, severity, opened_at,
                                        last_value, last_value_at, peak_value, threshold)
                 VALUES ($1, 'monitorflow_cpu_usage_percent', '', 'warning', now(), 91, now(), 91, '{}')
                 RETURNING id)
             INSERT INTO incident_events (incident_id, occurred_at, kind, severity, value)
             SELECT id, now(), 'opened', 'warning', 91 FROM i",
        )
        .bind(server)
        .execute(&pool)
        .await
        .unwrap();
        let deliverer = Deliverer::new(pool.clone(), "https://mf.example");
        assert_eq!(deliverer.pass().await.unwrap(), 1);
        let log = format!("{ENDPOINTS}/{id}/deliveries");
        let (_, deliveries, _) = call(&pool, "GET", &log, Some(&admin), None).await;
        let first = &deliveries["deliveries"][0];
        assert_eq!((&first["state"], &first["last_status"]), (&json!("delivered"), &json!(204)));
        let event = first["event_id"].clone();
        let replay = format!("{ENDPOINTS}/{id}/replay");
        let (status, queued, _) =
            call(&pool, "POST", &replay, Some(&admin), Some(json!({ "event_id": event }))).await;
        assert_eq!((status, &queued["queued"]), (StatusCode::OK, &json!(1)));
        assert_eq!(deliverer.pass().await.unwrap(), 1);
        {
            let requests = seen.lock().unwrap();
            let (headers, _) = requests.last().unwrap();
            assert_eq!(headers[REPLAY_HEADER], "1");
            assert_eq!(headers["x-monitorflow-delivery"].to_str().unwrap(), event.as_str().unwrap());
        }

        // Paused, nothing new is queued; on again, the failure count restarts.
        let state = format!("{ENDPOINTS}/{id}/state");
        let (_, paused, _) = call(&pool, "POST", &state, Some(&admin), Some(json!({ "state": "paused" }))).await;
        assert_eq!(paused["state"], "paused");
        let (status, _, _) =
            call(&pool, "POST", &state, Some(&admin), Some(json!({ "state": "disabled" }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        sqlx::query("UPDATE integration_endpoints SET state = 'disabled', failing_since = now()")
            .execute(&pool)
            .await
            .unwrap();
        let (_, on, _) = call(&pool, "POST", &state, Some(&admin), Some(json!({ "state": "on" }))).await;
        assert_eq!((&on["state"], &on["failing_since"]), (&json!("on"), &Value::Null));

        // A new secret replaces the old one, shown once.
        let (_, renewed, _) =
            call(&pool, "POST", &format!("{ENDPOINTS}/{id}/secret"), Some(&admin), None).await;
        let new_secret = renewed["secret"].as_str().unwrap();
        assert_ne!(new_secret, secret);
        let stored: String = sqlx::query_scalar("SELECT secret FROM integration_endpoints")
            .fetch_one(&pool)
            .await
            .unwrap();
        let opened = monitorflow_server::secrets::open(
            monitorflow_server::secrets::Column::IntegrationEndpoints,
            &stored,
        )
        .unwrap();
        assert_eq!(opened.expose(), new_secret, "stored sealed: {stored}");

        // The audit keeps the host, never the secret or the path.
        let audit: Vec<(String, Value)> =
            sqlx::query_as("SELECT action, details FROM audit_log WHERE action LIKE 'integration.%' ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        let actions: Vec<&str> = audit.iter().map(|(a, _)| a.as_str()).collect();
        assert_eq!(
            actions,
            ["integration.create", "integration.update", "integration.replay",
             "integration.state", "integration.state", "integration.secret_renew"]
        );
        let text = format!("{audit:?}");
        assert!(!text.contains(&secret) && !text.contains(new_secret) && !text.contains("/hook"));
        assert!(text.contains("127.0.0.1"));

        let (status, _, _) = call(&pool, "DELETE", &format!("{ENDPOINTS}/{id}"), Some(&admin), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    #[sqlx::test]
    async fn customer_administrators_manage_theirs_but_not_private_addresses(pool: PgPool) {
        monitorflow_server::secrets::install_for_tests();
        let (acme, _) = place(&pool, "acme").await;
        let global = login(&pool, "root", Role::Administrator, GrantScope::Global).await;
        let admin = login(&pool, "acme-admin", Role::Administrator, GrantScope::Customer(acme)).await;
        let operator = login(&pool, "acme-op", Role::Operator, GrantScope::Customer(acme)).await;
        let ask = json!({ "name": "PSA", "url": "http://10.0.0.5/hook", "customer_id": acme });

        let (status, _, _) = call(&pool, "GET", ENDPOINTS, Some(&operator), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _, _) = call(&pool, "POST", ENDPOINTS, Some(&operator), Some(ask.clone())).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let (status, created, _) = call(&pool, "POST", ENDPOINTS, Some(&admin), Some(ask.clone())).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["endpoint"]["allow_private"], false);
        assert_eq!(created["endpoint"]["customer_name"], "acme");

        // The global administrator allows the private address; the
        // customer administrator keeps it only for the same URL.
        let id = created["endpoint"]["id"].as_str().unwrap();
        let path = format!("{ENDPOINTS}/{id}");
        let mut private = ask.clone();
        private["allow_private"] = json!(true);
        let (status, view, _) = call(&pool, "PUT", &path, Some(&global), Some(private.clone())).await;
        assert_eq!((status, &view["endpoint"]["allow_private"]), (StatusCode::OK, &json!(true)));
        let mut renamed = private.clone();
        renamed["name"] = json!("Halo");
        let (status, view, _) = call(&pool, "PUT", &path, Some(&admin), Some(renamed)).await;
        assert_eq!((status, &view["endpoint"]["allow_private"]), (StatusCode::OK, &json!(true)));
        let mut moved = private;
        moved["url"] = json!("http://10.0.0.6/hook");
        let (status, _, _) = call(&pool, "PUT", &path, Some(&admin), Some(moved.clone())).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        moved["allow_private"] = json!(false);
        let (status, view, _) = call(&pool, "PUT", &path, Some(&admin), Some(moved)).await;
        assert_eq!((status, &view["endpoint"]["allow_private"]), (StatusCode::OK, &json!(false)));

        let (status, error, _) = call(
            &pool,
            "POST",
            ENDPOINTS,
            Some(&admin),
            Some(json!({ "name": "PSA", "url": "https://psa.example/", "customer_id": acme,
                         "events": ["backup.failed"] })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error.to_string().contains("backup.failed"), "{error}");
    }

    #[sqlx::test]
    async fn an_endpoint_shows_only_its_origin_and_an_edit_without_url_keeps_it(pool: PgPool) {
        let (acme, _) = place(&pool, "acme").await;
        let global = login(&pool, "root", Role::Administrator, GrantScope::Global).await;
        let admin = login(&pool, "boss", Role::Administrator, GrantScope::Customer(acme)).await;
        let url = "https://psa.example/hooks/T0KEN?key=QSECRET";
        let ask = json!({ "name": "HaloPSA", "url": url, "customer_id": acme });
        let (status, created, _) = call(&pool, "POST", ENDPOINTS, Some(&global), Some(ask)).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["endpoint"]["origin"], "https://psa.example");
        let path = format!("{ENDPOINTS}/{}", created["endpoint"]["id"].as_str().unwrap());

        // Neither the MSP's nor the customer's administrator reads the path.
        for user in [&global, &admin] {
            let (status, list, _) = call(&pool, "GET", ENDPOINTS, Some(user), None).await;
            assert_eq!(status, StatusCode::OK);
            let endpoint = &list["endpoints"][0];
            assert_eq!(endpoint["origin"], "https://psa.example");
            assert!(endpoint.get("url").is_none(), "{endpoint}");
            assert!(!list.to_string().contains("T0KEN"), "{list}");
            assert!(!list.to_string().contains("QSECRET"), "{list}");
        }

        // An edit without a URL keeps it.
        for edit in [
            json!({ "name": "Halo", "url": "", "customer_id": acme }),
            json!({ "name": "Halo", "customer_id": acme }),
        ] {
            let (status, view, _) = call(&pool, "PUT", &path, Some(&admin), Some(edit)).await;
            assert_eq!(status, StatusCode::OK, "{view}");
            assert_eq!(view["endpoint"]["name"], "Halo");
            assert_eq!(view["secret"], Value::Null, "{view}");
            let stored: String = sqlx::query_scalar("SELECT url FROM integration_endpoints")
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(stored, url);
        }

        // A new endpoint needs one.
        let (status, error, _) = call(
            &pool,
            "POST",
            ENDPOINTS,
            Some(&admin),
            Some(json!({ "name": "PSA", "url": " ", "customer_id": acme })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error.to_string().contains("Enter the URL"), "{error}");
    }

    #[sqlx::test]
    async fn a_new_url_gets_a_new_secret(pool: PgPool) {
        let (acme, _) = place(&pool, "acme").await;
        let global = login(&pool, "root", Role::Administrator, GrantScope::Global).await;
        let admin = login(&pool, "acme-admin", Role::Administrator, GrantScope::Customer(acme)).await;
        let (msp_url, msp_seen) = receiver().await;
        let (own_url, own_seen) = receiver().await;

        // The MSP sets up an endpoint for the customer.
        let ask = json!({ "name": "MSP PSA", "url": msp_url, "customer_id": acme,
                          "allow_private": true });
        let (status, created, _) = call(&pool, "POST", ENDPOINTS, Some(&global), Some(ask.clone())).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let old = created["secret"].as_str().unwrap().to_owned();
        let id = created["endpoint"]["id"].as_str().unwrap().to_owned();
        let path = format!("{ENDPOINTS}/{id}");

        // The customer administrator renames it: same URL, same secret,
        // even written differently.
        let mut renamed = ask.clone();
        renamed["name"] = json!("PSA");
        renamed["url"] = json!(format!("  {msp_url}  "));
        let (status, saved, _) = call(&pool, "PUT", &path, Some(&admin), Some(renamed)).await;
        assert_eq!((status, &saved["secret"]), (StatusCode::OK, &Value::Null), "{saved}");

        // Then points it at their own server: the MSP's secret stays behind.
        let moved = json!({ "name": "PSA", "url": own_url, "customer_id": acme });
        let (status, saved, _) = call(&pool, "PUT", &path, Some(&admin), Some(moved.clone())).await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        let new = saved["secret"].as_str().unwrap().to_owned();
        assert!(new.starts_with("mfwh_"));
        assert_ne!(new, old);
        let stored: String = sqlx::query_scalar("SELECT url FROM integration_endpoints")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(stored, own_url);
        let stored: String = sqlx::query_scalar("SELECT secret FROM integration_endpoints")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(stored, new);

        // A global administrator allows the loopback receiver without
        // moving it; the new secret signs what reaches it, never the old.
        let mut allowed = moved;
        allowed["allow_private"] = json!(true);
        let (status, saved, _) = call(&pool, "PUT", &path, Some(&global), Some(allowed)).await;
        assert_eq!((status, &saved["secret"]), (StatusCode::OK, &Value::Null), "{saved}");
        let (_, result, _) =
            call(&pool, "POST", &format!("{path}/test"), Some(&admin), None).await;
        assert_eq!(result["ok"], true, "{result}");
        assert!(msp_seen.lock().unwrap().is_empty());
        {
            let requests = own_seen.lock().unwrap();
            let (headers, body) = &requests[0];
            let timestamp: i64 = headers[TIMESTAMP_HEADER].to_str().unwrap().parse().unwrap();
            let given = headers[SIGNATURE_HEADER].to_str().unwrap();
            assert_eq!(given, signature(&new, timestamp, body));
            assert_ne!(given, signature(&old, timestamp, body));
        }

        // The audit says which saves renewed it, and never shows either.
        let audit: Vec<Value> = sqlx::query_scalar(
            "SELECT details FROM audit_log WHERE action = 'integration.update' ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        let renewed: Vec<&Value> = audit.iter().map(|d| &d["secret_renewed"]).collect();
        assert_eq!(renewed, [&json!(false), &json!(true), &json!(false)]);
        let text = format!("{audit:?}");
        assert!(!text.contains(&old) && !text.contains(&new));
    }
}
