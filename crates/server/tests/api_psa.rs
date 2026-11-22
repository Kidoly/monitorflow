//! The PSA connection (ADR-0164, ADR-0166): who manages it, the secret
//! sealed and never answered, the test against a stand-in for HaloPSA, and
//! the audit. Never a real instance. Ignored without `DATABASE_URL`.

mod fake_halo;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use fake_halo::{FakeHalo, AGENT, CLIENT_ID, CLIENT_SECRET};
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use monitorflow_server::secrets::{self, Column};
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
const CONNECTION: &str = "/api/psa/connection";
const TEST: &str = "/api/psa/connection/test";

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
    cookie: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let state = ApiState::new(pool.clone(), Registry::default(), ApiSettings::default());
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header(CSRF_HEADER, "1")
        .header("content-type", "application/json")
        .header("cookie", cookie)
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let response = api::router(state).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn login(pool: &PgPool, name: &str, role: Role, scope: GrantScope) -> String {
    users::create_user_with(pool, name, &Secret::new(PASSWORD), role, scope, "test")
        .await
        .unwrap();
    let state = ApiState::new(pool.clone(), Registry::default(), ApiSettings::default());
    let request = Request::builder()
        .method("POST")
        .uri("/api/auth/login")
        .header(CSRF_HEADER, "1")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "username": name, "password": PASSWORD }).to_string(),
        ))
        .unwrap();
    let response = api::router(state).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()["set-cookie"].to_str().unwrap();
    cookie.split(';').next().unwrap().to_owned()
}

async fn admin(pool: &PgPool) -> String {
    login(pool, "root", Role::Administrator, GrantScope::Global).await
}

fn settings(halo: &FakeHalo) -> Value {
    json!({
        "base_url": format!("{}/api/", halo.url),
        "client_id": CLIENT_ID,
        "client_secret": CLIENT_SECRET,
    })
}

async fn actions(pool: &PgPool) -> Vec<(String, Value)> {
    sqlx::query_as("SELECT action, details FROM audit_log WHERE action LIKE 'psa.%' ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap()
}

db_tests! {
    #[sqlx::test]
    async fn only_global_administrators_see_or_change_it(pool: PgPool) {
        secrets::install_for_tests();
        let halo = FakeHalo::start().await;
        let customer: uuid::Uuid =
            sqlx::query_scalar("INSERT INTO customers (name) VALUES ('acme') RETURNING id")
                .fetch_one(&pool)
                .await
                .unwrap();
        for (name, role, scope) in [
            ("viewer", Role::Viewer, GrantScope::Global),
            ("operator", Role::Operator, GrantScope::Global),
            ("acme-admin", Role::Administrator, GrantScope::Customer(customer)),
        ] {
            let cookie = login(&pool, name, role, scope).await;
            assert_eq!(call(&pool, "GET", CONNECTION, &cookie, None).await.0, StatusCode::FORBIDDEN);
            assert_eq!(
                call(&pool, "PUT", CONNECTION, &cookie, Some(settings(&halo))).await.0,
                StatusCode::FORBIDDEN
            );
            assert_eq!(
                call(&pool, "POST", TEST, &cookie, Some(settings(&halo))).await.0,
                StatusCode::FORBIDDEN
            );
        }
        assert!(halo.token_requests().is_empty());
        let cookie = admin(&pool).await;
        let (status, body) = call(&pool, "GET", CONNECTION, &cookie, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "connection": null }));
    }

    #[sqlx::test]
    async fn the_test_signs_in_reads_the_agent_and_saves_nothing(pool: PgPool) {
        secrets::install_for_tests();
        let halo = FakeHalo::start().await;
        let cookie = admin(&pool).await;
        let (status, body) = call(&pool, "POST", TEST, &cookie, Some(settings(&halo))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({ "ok": true, "agent_name": AGENT, "error": null }));
        let form = &halo.token_requests()[0];
        assert_eq!(form["grant_type"], "client_credentials");
        assert_eq!(form["scope"], "all");
        assert_eq!(halo.requests(), ["GET /api/Agent/me"]);
        let (_, body) = call(&pool, "GET", CONNECTION, &cookie, None).await;
        assert_eq!(body["connection"], Value::Null);

        // A wrong secret: Halo's words, never the secret.
        let mut wrong = settings(&halo);
        wrong["client_secret"] = "not-the-secret".into();
        let (_, body) = call(&pool, "POST", TEST, &cookie, Some(wrong)).await;
        assert_eq!(body["ok"], false);
        let error = body["error"].as_str().unwrap();
        assert!(error.contains("401") && error.contains("invalid_client"), "{error}");
        assert!(error.contains("Client Credentials"), "{error}");
        assert!(!error.contains("not-the-secret"));

        // Nothing listening.
        let gone = json!({ "base_url": "http://127.0.0.1:9", "client_id": CLIENT_ID, "client_secret": CLIENT_SECRET });
        let (_, body) = call(&pool, "POST", TEST, &cookie, Some(gone)).await;
        assert_eq!(body["ok"], false);
        assert!(body["error"].as_str().unwrap().contains("127.0.0.1"), "{body}");
    }

    #[sqlx::test]
    async fn the_secret_is_sealed_never_answered_and_kept_for_the_same_address(pool: PgPool) {
        secrets::install_for_tests();
        let halo = FakeHalo::start().await;
        let cookie = admin(&pool).await;
        let (status, body) = call(&pool, "PUT", CONNECTION, &cookie, Some(settings(&halo))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["base_url"], halo.url.as_str());
        assert_eq!(body["client_id"], CLIENT_ID);
        assert_eq!(body["enabled"], true);
        assert!(!body.to_string().contains(CLIENT_SECRET));
        let stored: String = sqlx::query_scalar("SELECT client_secret FROM psa_connections")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(!stored.contains(CLIENT_SECRET));
        assert_eq!(
            secrets::open(Column::PsaConnections, &stored).unwrap().expose(),
            CLIENT_SECRET
        );

        // Testing the stored connection records its agent.
        let (_, body) = call(&pool, "POST", TEST, &cookie, Some(json!({}))).await;
        assert_eq!(body["ok"], true, "{body}");
        let (_, body) = call(&pool, "GET", CONNECTION, &cookie, None).await;
        assert_eq!(body["connection"]["agent_name"], AGENT);
        assert!(body["connection"]["last_success_at"].is_string());

        // An edit without the secret keeps it for the same address...
        let mut edit = settings(&halo);
        edit["client_secret"] = "".into();
        edit["enabled"] = false.into();
        let (status, body) = call(&pool, "PUT", CONNECTION, &cookie, Some(edit.clone())).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["enabled"], false);
        assert_eq!(body["agent_name"], AGENT);
        let (_, body) = call(&pool, "POST", TEST, &cookie, Some(json!({}))).await;
        assert_eq!(body["ok"], true, "{body}");

        // ...but never sends it to another one.
        edit["base_url"] = "https://elsewhere.example.com".into();
        let (status, body) = call(&pool, "PUT", CONNECTION, &cookie, Some(edit.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "Enter the Client Secret.");
        let (_, body) = call(&pool, "POST", TEST, &cookie, Some(edit)).await;
        assert_eq!(body["error"], "Enter the Client Secret.");

        let (status, _) = call(&pool, "DELETE", CONNECTION, &cookie, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = call(&pool, "DELETE", CONNECTION, &cookie, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let audit = actions(&pool).await;
        let names: Vec<&str> = audit.iter().map(|(a, _)| a.as_str()).collect();
        assert_eq!(names, ["psa.connection.create", "psa.connection.update", "psa.connection.delete"]);
        assert_eq!(audit[0].1["secret_changed"], true);
        assert_eq!(audit[1].1["secret_changed"], false);
        for (_, details) in &audit {
            assert!(!details.to_string().contains(CLIENT_SECRET));
        }
    }

    #[sqlx::test]
    async fn bad_values_are_refused_in_plain_words(pool: PgPool) {
        secrets::install_for_tests();
        let cookie = admin(&pool).await;
        for (body, error) in [
            (json!({"base_url": "halo.example.com", "client_id": "x", "client_secret": "y"}),
             "Enter a full address, like https://halo.example.com."),
            (json!({"base_url": "https://u:p@halo.example.com", "client_id": "x", "client_secret": "y"}),
             "Put no user or password in the address: use the Client ID and Secret."),
            (json!({"base_url": "https://halo.example.com", "client_secret": "y"}),
             "Enter the Client ID."),
            (json!({"base_url": "https://halo.example.com", "client_id": "x"}),
             "Enter the Client Secret."),
            (json!({"kind": "other", "base_url": "https://halo.example.com", "client_id": "x", "client_secret": "y"}),
             "`kind` is `halopsa`."),
        ] {
            let (status, answer) = call(&pool, "PUT", CONNECTION, &cookie, Some(body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(answer["error"], error);
        }
    }

    #[sqlx::test]
    async fn sites_are_matched_to_clients_read_from_the_psa(pool: PgPool) {
        secrets::install_for_tests();
        let halo = FakeHalo::start().await;
        let cookie = admin(&pool).await;
        let (status, body) = call(&pool, "GET", "/api/psa/clients?search=example", &cookie, None).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "Set up the HaloPSA connection first.");
        call(&pool, "PUT", CONNECTION, &cookie, Some(settings(&halo))).await;

        let (status, body) = call(&pool, "GET", "/api/psa/clients?search=example", &cookie, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["records"],
            json!([{"id": 10, "name": "Example Industries"}, {"id": 11, "name": "Example Logistics"}])
        );
        let (_, body) = call(&pool, "GET", "/api/psa/clients/10/sites", &cookie, None).await;
        assert_eq!(body["records"], json!([{"id": 100, "name": "Main office"}, {"id": 101, "name": "Warehouse"}]));

        let customer: uuid::Uuid =
            sqlx::query_scalar("INSERT INTO customers (name) VALUES ('acme') RETURNING id")
                .fetch_one(&pool).await.unwrap();
        let paris: uuid::Uuid =
            sqlx::query_scalar("INSERT INTO sites (name, customer_id) VALUES ('paris', $1) RETURNING id")
                .bind(customer).fetch_one(&pool).await.unwrap();
        let lyon: uuid::Uuid =
            sqlx::query_scalar("INSERT INTO sites (name, customer_id) VALUES ('lyon', $1) RETURNING id")
                .bind(customer).fetch_one(&pool).await.unwrap();
        let link = |site: uuid::Uuid| format!("/api/psa/site-links/{site}");

        let (status, body) = call(&pool, "PUT", &link(paris), &cookie, Some(json!({"client_id": 10, "psa_site_id": 100}))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["client_name"], "Example Industries");
        assert_eq!(body["psa_site_name"], "Main office");

        // A site of another client, an unknown client, a site taken.
        let (status, body) = call(&pool, "PUT", &link(lyon), &cookie, Some(json!({"client_id": 10, "psa_site_id": 110}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "That HaloPSA site belongs to another client than Example Industries.");
        let (status, body) = call(&pool, "PUT", &link(lyon), &cookie, Some(json!({"client_id": 99}))).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("HaloPSA has no such client.")));
        let (status, body) = call(&pool, "PUT", &link(lyon), &cookie, Some(json!({"client_id": 10, "psa_site_id": 100}))).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("That HaloPSA site is already matched to paris.")));
        let (status, _) = call(&pool, "PUT", &link(lyon), &cookie, Some(json!({"client_id": 10}))).await;
        assert_eq!(status, StatusCode::OK);

        let (_, body) = call(&pool, "GET", "/api/psa/site-links", &cookie, None).await;
        let sites = body["sites"].as_array().unwrap();
        assert_eq!(sites.len(), 2);
        assert_eq!(sites[0]["site_name"], "lyon");
        assert_eq!(sites[0]["client_id"], 10);
        assert_eq!(sites[0]["psa_site_id"], Value::Null);

        let (status, _) = call(&pool, "DELETE", &link(lyon), &cookie, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = call(&pool, "DELETE", &link(lyon), &cookie, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // Another instance: its ids mean nothing, so the matches go.
        let mut moved = settings(&halo);
        moved["base_url"] = format!("{}/other", halo.url).into();
        call(&pool, "PUT", CONNECTION, &cookie, Some(moved)).await;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM psa_site_links").fetch_one(&pool).await.unwrap();
        assert_eq!(count, 0);

        let names: Vec<String> = actions(&pool).await.into_iter().map(|(a, _)| a).filter(|a| a.starts_with("psa.link")).collect();
        assert_eq!(names, ["psa.link.update", "psa.link.update", "psa.link.delete"]);
    }

    #[sqlx::test]
    async fn the_psa_s_refusal_is_a_502_in_its_words(pool: PgPool) {
        secrets::install_for_tests();
        let halo = FakeHalo::start().await;
        let cookie = admin(&pool).await;
        let mut wrong = settings(&halo);
        wrong["client_secret"] = "not-the-secret".into();
        call(&pool, "PUT", CONNECTION, &cookie, Some(wrong)).await;
        let (status, body) = call(&pool, "GET", "/api/psa/clients", &cookie, None).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body["code"], "psa_unavailable");
        assert!(body["error"].as_str().unwrap().contains("invalid_client"), "{body}");
        let (_, body) = call(&pool, "GET", CONNECTION, &cookie, None).await;
        assert!(body["connection"]["last_error"].as_str().unwrap().contains("invalid_client"));
    }
}
