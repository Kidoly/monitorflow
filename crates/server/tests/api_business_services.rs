//! The API of business services (ADR-0155): who may do what, what is
//! checked, the default customer of a single company, and the audit log.
//! Ignored without `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";
const SERVICES: &str = "/api/business-services";
const DEFAULT_CUSTOMER: Uuid = Uuid::from_u128(1);

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
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        cookie,
    )
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

/// A site of `customer` with a server `web` running postfix.
async fn site(pool: &PgPool, customer: Uuid, name: &str) -> (Uuid, Uuid) {
    let site: Uuid =
        sqlx::query_scalar("INSERT INTO sites (name, customer_id) VALUES ($1, $2) RETURNING id")
            .bind(name)
            .bind(customer)
            .fetch_one(pool)
            .await
            .unwrap();
    let server: Uuid = sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family)
         VALUES ($1, $2, $1, 'debian') RETURNING id",
    )
    .bind(format!("{name}-web"))
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO sensor_configs (server_id, kind, name, instance, interval_ms, params)
         VALUES ($1, 'systemd', 'service_postfix', '', 60000, '{\"unit\": \"postfix.service\"}')",
    )
    .bind(server)
    .execute(pool)
    .await
    .unwrap();
    (site, server)
}

async fn audited(pool: &PgPool, action: &str) -> Vec<Value> {
    sqlx::query_scalar("SELECT details FROM audit_log WHERE action = $1 ORDER BY id")
        .bind(action)
        .fetch_all(pool)
        .await
        .unwrap()
}

fn email(server: Uuid) -> Value {
    json!({
        "name": "Email", "rule": "all", "target": 99.9,
        "members": [
            {"server_id": server},
            {"server_id": server, "check": {"name": "service_postfix"}}
        ]
    })
}

db_tests! {
    #[sqlx::test]
    async fn a_single_company_needs_no_customer(pool: PgPool) {
        let (_, web) = site(&pool, DEFAULT_CUSTOMER, "paris").await;
        let admin = login(&pool, "root", Role::Administrator, GrantScope::Global).await;
        let (status, view, _) = call(&pool, "POST", SERVICES, Some(&admin), Some(email(web))).await;
        assert_eq!(status, StatusCode::CREATED, "{view}");
        assert_eq!(view["customer_id"], json!(DEFAULT_CUSTOMER));
        assert_eq!(view["members"][1]["check"]["name"], "service_postfix");
        assert_eq!(view["members"][0]["server_name"], "paris-web");
        assert_eq!(view["can_change"], true);
        let audit = audited(&pool, "business_service.create").await;
        assert_eq!(audit[0]["name"], "Email");
        // The name is taken.
        let (status, _, _) = call(&pool, "POST", SERVICES, Some(&admin), Some(email(web))).await;
        assert_eq!(status, StatusCode::CONFLICT);
        // At least 2 of 2, then removed.
        let id = view["id"].as_str().unwrap();
        let mut change = email(web);
        change["rule"] = json!("at_least");
        change["at_least"] = json!(2);
        let uri = format!("{SERVICES}/{id}");
        let (status, view, _) = call(&pool, "PUT", &uri, Some(&admin), Some(change)).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["at_least"], 2);
        let (status, _, _) = call(&pool, "DELETE", &uri, Some(&admin), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(audited(&pool, "business_service.remove").await.len(), 1);
    }

    #[sqlx::test]
    async fn services_belong_to_whole_customers(pool: PgPool) {
        let acme: Uuid = sqlx::query_scalar("INSERT INTO customers (name) VALUES ('acme') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
        let (acme_site, acme_web) = site(&pool, acme, "acme").await;
        let (_, default_web) = site(&pool, DEFAULT_CUSTOMER, "paris").await;
        let admin = login(&pool, "root", Role::Administrator, GrantScope::Global).await;
        // Two customers: the customer must be named.
        let (status, _, _) = call(&pool, "POST", SERVICES, Some(&admin), Some(email(acme_web))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let mut body = email(acme_web);
        body["customer_id"] = json!(acme);
        let operator = login(&pool, "olga", Role::Operator, GrantScope::Customer(acme)).await;
        let (status, view, _) = call(&pool, "POST", SERVICES, Some(&operator), Some(body.clone())).await;
        assert_eq!(status, StatusCode::CREATED, "{view}");
        // A server of another customer is refused.
        let mut other = body.clone();
        other["name"] = json!("Other");
        other["members"] = json!([{"server_id": default_web}]);
        let (status, _, _) = call(&pool, "POST", SERVICES, Some(&operator), Some(other)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // A viewer of the customer reads but does not change.
        let viewer = login(&pool, "vic", Role::Viewer, GrantScope::Customer(acme)).await;
        let (_, list, _) = call(&pool, "GET", SERVICES, Some(&viewer), None).await;
        assert_eq!(list["services"][0]["can_change"], false);
        body["name"] = json!("Web");
        let (status, _, _) = call(&pool, "POST", SERVICES, Some(&viewer), Some(body.clone())).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let uri = format!("{SERVICES}/{}", view["id"].as_str().unwrap());
        let (status, _, _) = call(&pool, "DELETE", &uri, Some(&viewer), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        // An operator of one site sees no service and makes none.
        let local = login(&pool, "sam", Role::Operator, GrantScope::Site(acme_site)).await;
        let (_, list, _) = call(&pool, "GET", SERVICES, Some(&local), None).await;
        assert_eq!(list["services"], json!([]));
        let (status, _, _) = call(&pool, "POST", SERVICES, Some(&local), Some(body)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[sqlx::test]
    async fn unknown_checks_and_rules_are_refused(pool: PgPool) {
        let (_, web) = site(&pool, DEFAULT_CUSTOMER, "paris").await;
        let admin = login(&pool, "root", Role::Administrator, GrantScope::Global).await;
        let mut body = email(web);
        body["members"][1]["check"]["name"] = json!("service_exim");
        let (status, error, _) = call(&pool, "POST", SERVICES, Some(&admin), Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error["error"].as_str().unwrap().contains("service_exim"), "{error}");
        let mut body = email(web);
        body["rule"] = json!("at_least");
        body["at_least"] = json!(3);
        let (status, _, _) = call(&pool, "POST", SERVICES, Some(&admin), Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}
