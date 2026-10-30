//! Users managed from the web interface (ADR-0059): invitations and
//! password resets by one-time link, grants, disabling and removal, and the
//! guard that keeps one administrator. Ignored without `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::{self, users, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

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

fn app(pool: &PgPool) -> Router {
    api::router(ApiState::new(
        pool.clone(),
        Registry::default(),
        ApiSettings::default(),
    ))
}

/// `(status, set-cookie, body)` of one request, with `cookie` if any.
async fn send(
    pool: &PgPool,
    method: &str,
    uri: &str,
    cookie: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Option<String>, Value) {
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
    let response = app(pool).oneshot(request).await.unwrap();
    let status = response.status();
    let cookie = response.headers().get("set-cookie").map(|v| {
        let v = v.to_str().unwrap();
        v.split(';').next().unwrap().to_owned()
    });
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, cookie, body)
}

async fn login(pool: &PgPool, username: &str, password: &str) -> Option<String> {
    let body = json!({ "username": username, "password": password });
    let (status, cookie, _) = send(pool, "POST", "/api/auth/login", None, Some(body)).await;
    (status == StatusCode::OK).then(|| cookie.unwrap())
}

/// A global administrator `root`, signed in.
async fn root(pool: &PgPool) -> (Uuid, String) {
    let id = users::create_user(
        pool,
        "root",
        &Secret::new(PASSWORD),
        Role::Administrator,
        "test",
    )
    .await
    .unwrap();
    (id, login(pool, "root", PASSWORD).await.unwrap())
}

async fn audit_actions(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar("SELECT action FROM audit_log WHERE action LIKE 'user.%' ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap()
}

db_tests! {
    #[sqlx::test]
    async fn an_invited_user_sets_a_password_with_the_link(pool: PgPool) {
        let (_, admin) = root(&pool).await;
        let site: Uuid = sqlx::query_scalar("INSERT INTO sites (name) VALUES ('paris') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
        let body = json!({ "username": "bob", "role": "operator", "site_id": site });
        let (status, _, invited) = send(&pool, "POST", "/api/users", Some(&admin), Some(body)).await;
        assert_eq!(status, StatusCode::CREATED, "{invited}");
        assert_eq!(invited["user"]["state"], "invited");
        assert_eq!(
            invited["user"]["roles"],
            json!([{ "role": "operator", "customer_id": null, "site_id": site }])
        );
        let token = invited["link"]["token"].as_str().unwrap().to_owned();
        assert_eq!(token.len(), 64);

        // No password yet: signing in fails.
        assert!(login(&pool, "bob", "").await.is_none());
        let (status, _, owner) =
            send(&pool, "POST", "/api/password-link", None, Some(json!({ "token": token }))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(owner, json!({ "username": "bob", "invited": true }));

        // A short password keeps the link.
        let short = json!({ "token": token, "password": "short" });
        let (status, _, _) = send(&pool, "POST", "/api/password-link/use", None, Some(short)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let set = json!({ "token": token, "password": PASSWORD });
        let (status, cookie, me) =
            send(&pool, "POST", "/api/password-link/use", None, Some(set.clone())).await;
        assert_eq!(status, StatusCode::OK, "{me}");
        assert_eq!(me["username"], "bob");
        let (status, _, _) = send(&pool, "GET", "/api/auth/me", cookie.as_deref(), None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(login(&pool, "bob", PASSWORD).await.is_some());

        // Single use.
        let (status, _, _) = send(&pool, "POST", "/api/password-link/use", None, Some(set)).await;
        assert_eq!(status, StatusCode::GONE);
        let (_, _, list) = send(&pool, "GET", "/api/users", Some(&admin), None).await;
        let names: Vec<(&str, &str)> = list["users"]
            .as_array()
            .unwrap()
            .iter()
            .map(|u| (u["username"].as_str().unwrap(), u["state"].as_str().unwrap()))
            .collect();
        assert_eq!(names, [("bob", "active"), ("root", "active")]);
        assert_eq!(
            audit_actions(&pool).await,
            ["user.create", "user.create", "user.link_issued", "user.password_set"]
        );
    }

    #[sqlx::test]
    async fn a_reset_link_replaces_the_password_and_ends_sessions(pool: PgPool) {
        let (_, admin) = root(&pool).await;
        let bob = users::create_user(&pool, "bob", &Secret::new(PASSWORD), Role::Viewer, "test")
            .await
            .unwrap();
        let session = login(&pool, "bob", PASSWORD).await.unwrap();
        let uri = format!("/api/users/{bob}/link");
        let (status, _, first) = send(&pool, "POST", &uri, Some(&admin), None).await;
        assert_eq!(status, StatusCode::CREATED);
        let (_, _, second) = send(&pool, "POST", &uri, Some(&admin), None).await;
        // The old password works until the link is used; the first link
        // was replaced.
        assert!(login(&pool, "bob", PASSWORD).await.is_some());
        let set = |token: &Value| json!({ "token": token, "password": "a new long password" });
        let (status, _, _) =
            send(&pool, "POST", "/api/password-link/use", None, Some(set(&first["token"]))).await;
        assert_eq!(status, StatusCode::GONE);
        let (status, _, owner) =
            send(&pool, "POST", "/api/password-link", None, Some(json!({ "token": second["token"] }))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(owner["invited"], false);
        let (status, _, _) =
            send(&pool, "POST", "/api/password-link/use", None, Some(set(&second["token"]))).await;
        assert_eq!(status, StatusCode::OK);
        assert!(login(&pool, "bob", PASSWORD).await.is_none());
        assert!(login(&pool, "bob", "a new long password").await.is_some());
        let (status, _, _) = send(&pool, "GET", "/api/auth/me", Some(&session), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // An expired link is refused.
        let (_, _, third) = send(&pool, "POST", &uri, Some(&admin), None).await;
        sqlx::query("UPDATE password_links SET created_at = now() - interval '9 days', expires_at = now() - interval '1 day'")
            .execute(&pool)
            .await
            .unwrap();
        let (status, _, _) =
            send(&pool, "POST", "/api/password-link/use", None, Some(set(&third["token"]))).await;
        assert_eq!(status, StatusCode::GONE);
    }

    #[sqlx::test]
    async fn roles_are_granted_and_revoked(pool: PgPool) {
        let (_, admin) = root(&pool).await;
        let customer: Uuid = sqlx::query_scalar("INSERT INTO customers (name) VALUES ('acme') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
        let bob = users::create_user(&pool, "bob", &Secret::new(PASSWORD), Role::Viewer, "test")
            .await
            .unwrap();
        let uri = format!("/api/users/{bob}/roles");
        let grant = json!({ "role": "administrator", "customer_id": customer });
        let (status, _, view) = send(&pool, "POST", &uri, Some(&admin), Some(grant.clone())).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["roles"].as_array().unwrap().len(), 2);
        // Granting it again changes nothing.
        let (_, _, again) = send(&pool, "POST", &uri, Some(&admin), Some(grant.clone())).await;
        assert_eq!(again["roles"], view["roles"]);
        let both = json!({ "role": "viewer", "customer_id": customer, "site_id": customer });
        let (status, _, _) = send(&pool, "POST", &uri, Some(&admin), Some(both)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let unknown = json!({ "role": "viewer", "site_id": Uuid::from_u128(9) });
        let (status, _, _) = send(&pool, "POST", &uri, Some(&admin), Some(unknown)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, _, view) = send(&pool, "DELETE", &uri, Some(&admin), Some(grant)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(view["roles"], json!([{ "role": "viewer", "customer_id": null, "site_id": null }]));
        assert_eq!(audit_actions(&pool).await, ["user.create", "user.create", "user.grant", "user.revoke"]);

        // Customers are renamed by global administrators.
        let uri = format!("/api/customers/{customer}");
        let (status, _, view) = send(&pool, "PUT", &uri, Some(&admin), Some(json!({ "name": "Acme Corp" }))).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["name"], "Acme Corp");
        let (status, _, _) = send(&pool, "PUT", &uri, Some(&admin), Some(json!({ "name": "default" }))).await;
        assert_eq!(status, StatusCode::CONFLICT);
        let bob_session = login(&pool, "bob", PASSWORD).await.unwrap();
        let (status, _, _) = send(&pool, "PUT", &uri, Some(&bob_session), Some(json!({ "name": "x" }))).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _, _) = send(&pool, "GET", "/api/users", Some(&bob_session), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[sqlx::test]
    async fn the_last_administrator_cannot_be_locked_out(pool: PgPool) {
        let (me, admin) = root(&pool).await;
        let global = json!({ "role": "administrator" });
        let (status, _, _) = send(&pool, "DELETE", &format!("/api/users/{me}/roles"), Some(&admin), Some(global.clone())).await;
        assert_eq!(status, StatusCode::CONFLICT);
        let (status, _, _) = send(&pool, "DELETE", &format!("/api/users/{me}"), Some(&admin), None).await;
        assert_eq!(status, StatusCode::CONFLICT);
        let (status, _, _) = send(&pool, "PUT", &format!("/api/users/{me}/disabled"), Some(&admin), Some(json!({ "disabled": true }))).await;
        assert_eq!(status, StatusCode::CONFLICT);

        // An invited administrator does not count until it has a password.
        let (_, _, invited) = send(&pool, "POST", "/api/users", Some(&admin), Some(json!({ "username": "ann", "role": "administrator" }))).await;
        let (status, _, _) = send(&pool, "DELETE", &format!("/api/users/{me}/roles"), Some(&admin), Some(global.clone())).await;
        assert_eq!(status, StatusCode::CONFLICT);
        let set = json!({ "token": invited["link"]["token"], "password": PASSWORD });
        let (status, _, _) = send(&pool, "POST", "/api/password-link/use", None, Some(set)).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _, view) = send(&pool, "DELETE", &format!("/api/users/{me}/roles"), Some(&admin), Some(global)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(view["roles"], json!([]));
    }

    #[sqlx::test]
    async fn disabled_and_removed_users_lose_access(pool: PgPool) {
        let (_, admin) = root(&pool).await;
        let bob = users::create_user(&pool, "bob", &Secret::new(PASSWORD), Role::Operator, "test")
            .await
            .unwrap();
        let session = login(&pool, "bob", PASSWORD).await.unwrap();
        // History that must survive the removal.
        sqlx::query("INSERT INTO sites (name) VALUES ('paris')").execute(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO enrollment_tokens (token_hash, role, site_id, created_by, expires_at, used_at, used_by_peer_id, peer_id)
             SELECT sha256('x'), 'agent', id, $1, now() + interval '1 hour', now(), 'agent-1', 'agent-1' FROM sites",
        )
        .bind(bob)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO install_keys (key_hash, site_id, created_by, expires_at)
             SELECT sha256('y'), id, $1, now() + interval '1 hour' FROM sites",
        )
        .bind(bob)
        .execute(&pool)
        .await
        .unwrap();

        let uri = format!("/api/users/{bob}/disabled");
        let (status, _, view) = send(&pool, "PUT", &uri, Some(&admin), Some(json!({ "disabled": true }))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(view["state"], "disabled");
        let (status, _, _) = send(&pool, "GET", "/api/auth/me", Some(&session), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(login(&pool, "bob", PASSWORD).await.is_none());
        let (status, _, _) = send(&pool, "POST", &format!("/api/users/{bob}/link"), Some(&admin), None).await;
        assert_eq!(status, StatusCode::CONFLICT);
        let (_, _, view) = send(&pool, "PUT", &uri, Some(&admin), Some(json!({ "disabled": false }))).await;
        assert_eq!(view["state"], "active");
        assert!(login(&pool, "bob", PASSWORD).await.is_some());

        let (status, _, _) = send(&pool, "DELETE", &format!("/api/users/{bob}"), Some(&admin), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _, _) = send(&pool, "DELETE", &format!("/api/users/{bob}"), Some(&admin), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let creators: Vec<Option<Uuid>> = sqlx::query_scalar("SELECT created_by FROM enrollment_tokens")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(creators, [None]);
        let key_owners: Vec<Option<Uuid>> = sqlx::query_scalar("SELECT created_by FROM install_keys")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(key_owners, [None]);
        // The name is free again.
        let (status, _, _) = send(&pool, "POST", "/api/users", Some(&admin), Some(json!({ "username": "BOB", "role": "viewer" }))).await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, _, _) = send(&pool, "POST", "/api/users", Some(&admin), Some(json!({ "username": "bob", "role": "viewer" }))).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            audit_actions(&pool).await,
            ["user.create", "user.create", "user.disable", "user.enable", "user.remove", "user.create", "user.link_issued"]
        );
    }
}
