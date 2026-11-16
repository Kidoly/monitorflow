//! The signed-in person's own preferences (ADR-0137): read and replaced
//! through the session only, checked against their schema, never reached
//! for someone else, and removed with the user. Ignored without
//! `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::{users, v1, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";
const URI: &str = "/api/me/preferences";

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
    monitorflow_server::secrets::install_for_tests();
    v1::app(ApiState::new(
        pool.clone(),
        Registry::default(),
        ApiSettings::default(),
    ))
}

struct Call<'a> {
    method: &'a str,
    uri: &'a str,
    cookie: Option<&'a str>,
    key: Option<&'a str>,
    csrf: bool,
    body: Option<String>,
}

impl<'a> Call<'a> {
    fn new(method: &'a str, uri: &'a str, cookie: Option<&'a str>) -> Self {
        Self {
            method,
            uri,
            cookie,
            key: None,
            csrf: true,
            body: None,
        }
    }

    fn json(mut self, body: Value) -> Self {
        self.body = Some(body.to_string());
        self
    }

    /// `(status, set-cookie, body)`.
    async fn send(self, pool: &PgPool) -> (StatusCode, Option<String>, Value) {
        let mut request = Request::builder().method(self.method).uri(self.uri);
        if self.csrf {
            request = request.header(CSRF_HEADER, "1");
        }
        if let Some(cookie) = self.cookie {
            request = request.header("cookie", cookie);
        }
        if let Some(key) = self.key {
            request = request.header("authorization", format!("Bearer {key}"));
        }
        let request = match self.body {
            Some(body) => request
                .header("content-type", "application/json")
                .body(Body::from(body)),
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
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, cookie, body)
    }
}

/// A user `name` with `role`, signed in: `(id, cookie)`.
async fn person(pool: &PgPool, name: &str, role: Role) -> (Uuid, String) {
    let id = users::create_user(pool, name, &Secret::new(PASSWORD), role, "test")
        .await
        .unwrap();
    let body = json!({ "username": name, "password": PASSWORD });
    let (status, cookie, _) = Call::new("POST", "/api/auth/login", None)
        .json(body)
        .send(pool)
        .await;
    assert_eq!(status, StatusCode::OK);
    (id, cookie.unwrap())
}

async fn stored(pool: &PgPool, user: Uuid) -> Option<Value> {
    sqlx::query_scalar("SELECT document FROM user_preferences WHERE user_id = $1")
        .bind(user)
        .fetch_optional(pool)
        .await
        .unwrap()
}

db_tests! {
    #[sqlx::test]
    async fn a_person_reads_and_replaces_their_preferences(pool: PgPool) {
        let (id, cookie) = person(&pool, "ana", Role::Viewer).await;
        let (status, _, body) = Call::new("GET", URI, Some(&cookie)).send(&pool).await;
        assert_eq!((status, body), (StatusCode::OK, json!({})));

        let document = json!({
            "theme": "dark",
            "language": "fr",
            "sidebar": { "collapsed": true, "hidden": ["audit"], "order": ["incidents", "fleet"] }
        });
        let (status, _, body) = Call::new("PUT", URI, Some(&cookie)).json(document.clone()).send(&pool).await;
        assert_eq!((status, &body), (StatusCode::OK, &document));
        let (status, _, body) = Call::new("GET", URI, Some(&cookie)).send(&pool).await;
        assert_eq!((status, &body), (StatusCode::OK, &document));
        assert_eq!(stored(&pool, id).await.as_ref(), Some(&document));

        // A PUT replaces the whole document.
        let (status, _, body) = Call::new("PUT", URI, Some(&cookie))
            .json(json!({ "theme": "light" }))
            .send(&pool)
            .await;
        assert_eq!((status, body), (StatusCode::OK, json!({ "theme": "light" })));
        let (_, _, body) = Call::new("GET", URI, Some(&cookie)).send(&pool).await;
        assert_eq!(body, json!({ "theme": "light" }));
        // No audit entry for a preference.
        let audited: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE actor_id = $1 AND action NOT LIKE 'auth.%'")
            .bind(id.to_string())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(audited, 0);
    }

    #[sqlx::test]
    async fn invalid_documents_are_refused_and_change_nothing(pool: PgPool) {
        let (id, cookie) = person(&pool, "ana", Role::Viewer).await;
        let kept = json!({ "theme": "dark" });
        let (status, _, _) = Call::new("PUT", URI, Some(&cookie)).json(kept.clone()).send(&pool).await;
        assert_eq!(status, StatusCode::OK);
        let many: Vec<String> = (0..65).map(|i| format!("item{i}")).collect();
        for bad in [
            json!({ "colour": "blue" }),
            json!({ "theme": "sepia" }),
            json!({ "language": "de" }),
            json!({ "sidebar": { "width": 300 } }),
            json!({ "sidebar": { "hidden": ["fleet", "fleet"] } }),
            json!({ "sidebar": { "order": ["<script>"] } }),
            json!({ "sidebar": { "hidden": many } }),
            json!(["theme"]),
            json!([]),
            json!({ "sidebar": [] }),
            json!(null),
        ] {
            let (status, _, body) = Call::new("PUT", URI, Some(&cookie)).json(bad.clone()).send(&pool).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
            assert_eq!(body["code"], "invalid", "{bad}: {body}");
        }
        assert_eq!(stored(&pool, id).await, Some(kept));
    }

    #[sqlx::test]
    async fn a_body_over_the_cap_is_refused(pool: PgPool) {
        let (id, cookie) = person(&pool, "ana", Role::Viewer).await;
        // Valid JSON of a known key, padded with spaces past 16 KiB.
        let mut call = Call::new("PUT", URI, Some(&cookie));
        call.body = Some(format!("{{\"theme\": \"dark\"{}}}", " ".repeat(16 * 1024)));
        let (status, _, body) = call.send(&pool).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body["error"].as_str().unwrap().contains("16 KiB"), "{body}");
        assert_eq!(stored(&pool, id).await, None);
    }

    #[sqlx::test]
    async fn the_session_and_csrf_rules_apply(pool: PgPool) {
        let (_, cookie) = person(&pool, "ana", Role::Viewer).await;
        let (status, _, _) = Call::new("GET", URI, None).send(&pool).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _, _) = Call::new("PUT", URI, None).json(json!({})).send(&pool).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let mut call = Call::new("PUT", URI, Some(&cookie)).json(json!({ "theme": "dark" }));
        call.csrf = false;
        let (status, _, _) = call.send(&pool).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[sqlx::test]
    async fn another_persons_preferences_are_unreachable(pool: PgPool) {
        let (ana, ana_cookie) = person(&pool, "ana", Role::Administrator).await;
        let (bob, bob_cookie) = person(&pool, "bob", Role::Viewer).await;
        let (status, _, _) = Call::new("PUT", URI, Some(&bob_cookie))
            .json(json!({ "theme": "light" }))
            .send(&pool)
            .await;
        assert_eq!(status, StatusCode::OK);
        // Even a global administrator reads and writes only their own.
        let (_, _, body) = Call::new("GET", URI, Some(&ana_cookie)).send(&pool).await;
        assert_eq!(body, json!({}));
        let (status, _, _) = Call::new("PUT", URI, Some(&ana_cookie))
            .json(json!({ "theme": "dark" }))
            .send(&pool)
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(stored(&pool, bob).await, Some(json!({ "theme": "light" })));
        assert_eq!(stored(&pool, ana).await, Some(json!({ "theme": "dark" })));
        // No route takes a user: these do not exist.
        for uri in [format!("/api/users/{bob}/preferences"), format!("{URI}/{bob}")] {
            let (status, _, _) = Call::new("GET", &uri, Some(&ana_cookie)).send(&pool).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        }
    }

    #[sqlx::test]
    async fn the_public_api_and_api_keys_never_reach_them(pool: PgPool) {
        let (_, cookie) = person(&pool, "ana", Role::Administrator).await;
        let (status, _, created) = Call::new("POST", "/api/api-keys", Some(&cookie))
            .json(json!({ "name": "Script" }))
            .send(&pool)
            .await;
        assert_eq!(status, StatusCode::CREATED, "{}", created["error"]);
        let secret = created["secret"].as_str().unwrap().to_owned();
        for method in ["GET", "PUT"] {
            let mut call = Call::new(method, "/api/v1/me/preferences", None).json(json!({}));
            call.key = Some(&secret);
            let (status, _, _) = call.send(&pool).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{method} with a key");
            let (status, _, _) = Call::new(method, "/api/v1/me/preferences", Some(&cookie))
                .json(json!({}))
                .send(&pool)
                .await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{method} with a session");
        }
    }

    #[sqlx::test]
    async fn removing_a_user_removes_their_preferences(pool: PgPool) {
        let (_, root) = person(&pool, "root", Role::Administrator).await;
        let (bob, bob_cookie) = person(&pool, "bob", Role::Viewer).await;
        let (status, _, _) = Call::new("PUT", URI, Some(&bob_cookie))
            .json(json!({ "sidebar": { "collapsed": true } }))
            .send(&pool)
            .await;
        assert_eq!(status, StatusCode::OK);
        assert!(stored(&pool, bob).await.is_some());
        let (status, _, body) = Call::new("DELETE", &format!("/api/users/{bob}"), Some(&root)).send(&pool).await;
        assert!(status.is_success(), "{status} {body}");
        assert_eq!(stored(&pool, bob).await, None);
        // Their session went with them.
        let (status, _, _) = Call::new("GET", URI, Some(&bob_cookie)).send(&pool).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}
