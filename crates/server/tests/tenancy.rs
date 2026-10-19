//! Customers and customer-scoped grants (ADR-0026) against a real
//! PostgreSQL: the default-customer migration and how grants on a customer
//! reach its sites through the API. Ignored without `DATABASE_URL`.

use std::borrow::Cow;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users;
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::db;
use monitorflow_server::grpc::Registry;
use serde_json::{json, Value};
use sqlx::migrate::Migrator;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";
const CUSTOMERS_MIGRATION: i64 = 20261020120000;
const DEFAULT_CUSTOMER: Uuid = Uuid::from_u128(1);

/// Marks every test in the block as ignored when `DATABASE_URL` is unset.
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

fn app(pool: &PgPool) -> Router {
    api::router(ApiState::new(
        pool.clone(),
        Registry::default(),
        ApiSettings::default(),
    ))
}

async fn send(pool: &PgPool, request: Request<Body>) -> (StatusCode, Value, Option<String>) {
    let response = app(pool).oneshot(request).await.unwrap();
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

/// A user whose only grant is `role` with `(customer_id, site_id)`.
async fn session(
    pool: &PgPool,
    username: &str,
    role: Role,
    scope: (Option<Uuid>, Option<Uuid>),
) -> String {
    let id = users::create_user(pool, username, &Secret::new(PASSWORD), role, "test")
        .await
        .unwrap();
    sqlx::query("UPDATE user_roles SET customer_id = $2, site_id = $3 WHERE user_id = $1")
        .bind(id)
        .bind(scope.0)
        .bind(scope.1)
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
    body: Value,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("cookie", cookie)
        .header(CSRF_HEADER, "1")
        .header("content-type", "application/json")
        .body(if body.is_null() {
            Body::empty()
        } else {
            Body::from(body.to_string())
        })
        .unwrap();
    let (status, body, _) = send(pool, request).await;
    (status, body)
}

async fn customer(pool: &PgPool, name: &str) -> Uuid {
    sqlx::query_scalar("INSERT INTO customers (name) VALUES ($1) RETURNING id")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// A site of `customer` with one server; returns `(site, server)`.
async fn site(pool: &PgPool, customer: Uuid, name: &str) -> (Uuid, Uuid) {
    let site: Uuid =
        sqlx::query_scalar("INSERT INTO sites (name, customer_id) VALUES ($1, $2) RETURNING id")
            .bind(name)
            .bind(customer)
            .fetch_one(pool)
            .await
            .unwrap();
    let server = sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family)
         VALUES ($1, $2, $1, 'debian') RETURNING id",
    )
    .bind(format!("agent-{name}"))
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap();
    (site, server)
}

fn hostnames(body: &Value) -> Vec<&str> {
    body["servers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["hostname"].as_str().unwrap())
        .collect()
}

db_tests! {
    #[sqlx::test(migrations = false)]
    async fn migration_moves_existing_sites_to_the_default_customer(pool: PgPool) {
        // The schema as released before customers, with data in it.
        let before = Migrator {
            migrations: Cow::Owned(
                db::MIGRATOR.iter().filter(|m| m.version < CUSTOMERS_MIGRATION).cloned().collect(),
            ),
            ..Migrator::DEFAULT
        };
        before.run(&pool).await.unwrap();
        let paris: Uuid = sqlx::query_scalar("INSERT INTO sites (name) VALUES ('paris') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
        users::create_user(&pool, "alice", &Secret::new(PASSWORD), Role::Viewer, "test")
            .await
            .unwrap();

        db::migrate(&pool).await.unwrap();
        let (id, name): (Uuid, String) = sqlx::query_as("SELECT id, name FROM customers")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!((id, name.as_str()), (DEFAULT_CUSTOMER, "default"));
        let owner: Uuid = sqlx::query_scalar("SELECT customer_id FROM sites WHERE id = $1")
            .bind(paris)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(owner, DEFAULT_CUSTOMER);
        // Existing grants stay global; new sites default to the same customer.
        let scope: (Option<Uuid>, Option<Uuid>) =
            sqlx::query_as("SELECT customer_id, site_id FROM user_roles").fetch_one(&pool).await.unwrap();
        assert_eq!(scope, (None, None));
        let owner: Uuid = sqlx::query_scalar("INSERT INTO sites (name) VALUES ('lyon') RETURNING customer_id")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(owner, DEFAULT_CUSTOMER);
    }

    #[sqlx::test]
    async fn grants_are_global_customer_or_site_never_both(pool: PgPool) {
        let acme = customer(&pool, "acme").await;
        let (paris, _) = site(&pool, acme, "paris").await;
        let user: Uuid = sqlx::query_scalar(
            "INSERT INTO users (username, password_hash) VALUES ('u', '$argon2id$x') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let grant = |customer: Option<Uuid>, site: Option<Uuid>| {
            sqlx::query("INSERT INTO user_roles (user_id, role, customer_id, site_id) VALUES ($1, 'viewer', $2, $3)")
                .bind(user)
                .bind(customer)
                .bind(site)
                .execute(&pool)
        };
        grant(Some(acme), None).await.unwrap();
        grant(None, Some(paris)).await.unwrap();
        grant(None, None).await.unwrap();
        assert!(grant(Some(acme), Some(paris)).await.is_err());
        assert!(grant(Some(acme), None).await.is_err(), "duplicate customer grant");
        assert!(sqlx::query("INSERT INTO customers (name) VALUES ('acme')").execute(&pool).await.is_err());
        assert!(sqlx::query("INSERT INTO customers (name) VALUES ('')").execute(&pool).await.is_err());
    }

    #[sqlx::test]
    async fn customer_grants_reach_every_site_of_the_customer(pool: PgPool) {
        let acme = customer(&pool, "acme").await;
        let globex = customer(&pool, "globex").await;
        let (paris, paris_server) = site(&pool, acme, "paris").await;
        let (_, lyon_server) = site(&pool, acme, "lyon").await;
        let (berlin, berlin_server) = site(&pool, globex, "berlin").await;

        let viewer = session(&pool, "viewer", Role::Viewer, (Some(acme), None)).await;
        let (status, body) = call(&pool, &viewer, "GET", "/api/servers", Value::Null).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(hostnames(&body), ["agent-lyon", "agent-paris"]);
        for server in [paris_server, lyon_server] {
            let (status, _) = call(&pool, &viewer, "GET", &format!("/api/servers/{server}"), Value::Null).await;
            assert_eq!(status, StatusCode::OK);
        }
        let (status, _) = call(&pool, &viewer, "GET", &format!("/api/servers/{berlin_server}"), Value::Null).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // A site created after login is covered on the next request.
        let (_, nice_server) = site(&pool, acme, "nice").await;
        let (status, _) = call(&pool, &viewer, "GET", &format!("/api/servers/{nice_server}"), Value::Null).await;
        assert_eq!(status, StatusCode::OK);

        // Read-only on the customer: 403; another customer's site: 404.
        let row = json!({ "kind": "cpu", "interval_ms": 10000 });
        let (status, _) = call(&pool, &viewer, "POST", &format!("/api/sites/{paris}/sensor_configs"), row.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let operator = session(&pool, "operator", Role::Operator, (Some(acme), None)).await;
        let (status, _) = call(&pool, &operator, "POST", &format!("/api/sites/{paris}/sensor_configs"), row.clone()).await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, _) = call(&pool, &operator, "POST", &format!("/api/sites/{berlin}/sensor_configs"), row).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // A customer without sites gives empty lists, not 403.
        let initech = customer(&pool, "initech").await;
        let empty = session(&pool, "empty", Role::Viewer, (Some(initech), None)).await;
        let (status, body) = call(&pool, &empty, "GET", "/api/servers", Value::Null).await;
        assert_eq!((status, hostnames(&body).len()), (StatusCode::OK, 0));
        let (status, body) = call(&pool, &empty, "GET", "/api/sites", Value::Null).await;
        assert_eq!((status, body["sites"].as_array().unwrap().len()), (StatusCode::OK, 0));
    }
}
