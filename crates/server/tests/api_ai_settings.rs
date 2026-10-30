//! AI setup in the web interface (ADR-0064): who reads and changes it, the
//! key never coming back, the test against a mock of the Messages API, the
//! audit, and the configuration's precedence. Ignored without
//! `DATABASE_URL`.

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::routing::post;
use axum::Router;
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use monitorflow_server::remediation::switch;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
const AI: &str = "/api/settings/ai";
const TEST: &str = "/api/settings/ai/test";
const KEY: &str = "sk-ant-good-key-0123456789";
const SONNET: &str = "claude-sonnet-5-5";

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

/// The Messages API: accepts [`KEY`] and refuses anything else.
#[derive(Clone, Default)]
struct Mock {
    seen: Arc<Mutex<Vec<(String, String)>>>,
}

async fn messages(
    State(mock): State<Mock>,
    headers: HeaderMap,
    body: String,
) -> (StatusCode, String) {
    let key = headers["x-api-key"].to_str().unwrap().to_owned();
    let body: Value = serde_json::from_str(&body).unwrap();
    let model = body["model"].as_str().unwrap().to_owned();
    let good = key == KEY;
    mock.seen.lock().unwrap().push((key, model));
    if good {
        (StatusCode::OK, json!({"content": []}).to_string())
    } else {
        let error = json!({"type": "error", "error": {"type": "authentication_error", "message": "invalid x-api-key"}});
        (StatusCode::UNAUTHORIZED, error.to_string())
    }
}

async fn mock_api() -> (Mock, ApiSettings) {
    let mock = Mock::default();
    let app = Router::new()
        .route("/v1/messages", post(messages))
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut settings = ApiSettings::default();
    settings.ai.base_url = format!("http://{addr}");
    (mock, settings)
}

async fn call(
    pool: &PgPool,
    settings: ApiSettings,
    method: &str,
    uri: &str,
    cookie: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value, Option<String>) {
    let state = ApiState::new(pool.clone(), Registry::default(), settings);
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
    let (status, _, cookie) = call(
        pool,
        ApiSettings::default(),
        "POST",
        "/api/auth/login",
        None,
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    cookie.unwrap()
}

async fn admin(pool: &PgPool) -> String {
    login(pool, "root", Role::Administrator, GrantScope::Global).await
}

async fn audit_details(pool: &PgPool) -> Vec<Value> {
    sqlx::query_scalar("SELECT details FROM audit_log WHERE action = 'settings.ai' ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap()
}

db_tests! {
    #[sqlx::test]
    async fn off_by_default_and_readable_by_every_user(pool: PgPool) {
        let cookie = login(&pool, "viewer", Role::Viewer, GrantScope::Global).await;
        let (status, view, _) =
            call(&pool, ApiSettings::default(), "GET", AI, Some(&cookie), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(view["enabled"], false);
        assert_eq!(view["from_config"], false);
        assert_eq!(view["has_key"], false);
        assert_eq!(view["model"], view["models"][0]["id"]);
        let (status, _, _) = call(&pool, ApiSettings::default(), "GET", AI, None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[sqlx::test]
    async fn only_global_administrators_change_or_test_it(pool: PgPool) {
        let cookie = login(&pool, "viewer", Role::Operator, GrantScope::Global).await;
        let body = json!({ "api_key": KEY, "model": SONNET, "enabled": true });
        for (method, uri, body) in [
            ("PUT", AI, Some(body)),
            ("DELETE", AI, None),
            ("POST", TEST, Some(json!({}))),
        ] {
            let (status, _, _) =
                call(&pool, ApiSettings::default(), method, uri, Some(&cookie), body).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
        }
    }

    #[sqlx::test]
    async fn the_test_saves_nothing_and_answers_in_plain_words(pool: PgPool) {
        let (mock, settings) = mock_api().await;
        let cookie = admin(&pool).await;
        let test = |key: Option<&str>, settings| {
            let body = json!({ "api_key": key, "model": SONNET });
            let (pool, cookie) = (pool.clone(), cookie.clone());
            async move {
                let (status, answer, _) =
                    call(&pool, settings, "POST", TEST, Some(&cookie), Some(body)).await;
                assert_eq!(status, StatusCode::OK);
                answer
            }
        };
        let answer = test(None, settings.clone()).await;
        assert_eq!(answer["ok"], false);
        assert!(answer["error"].as_str().unwrap().contains("Enter an API key"));
        assert!(mock.seen.lock().unwrap().is_empty());

        let answer = test(Some("sk-ant-bad"), settings.clone()).await;
        assert_eq!(answer["ok"], false);
        let error = answer["error"].as_str().unwrap();
        assert!(error.contains("refused this API key"), "{error}");

        let answer = test(Some(KEY), settings.clone()).await;
        assert_eq!(answer, json!({ "ok": true, "error": null }));
        assert_eq!(mock.seen.lock().unwrap().last().unwrap(), &(KEY.to_owned(), SONNET.to_owned()));

        let (_, view, _) = call(&pool, settings, "GET", AI, Some(&cookie), None).await;
        assert_eq!(view["has_key"], false);
        assert!(audit_details(&pool).await.is_empty());
    }

    #[sqlx::test]
    async fn enabling_stores_the_key_and_never_returns_it(pool: PgPool) {
        let (mock, settings) = mock_api().await;
        let cookie = admin(&pool).await;
        // Off, a key and a model can be kept for later.
        let put = |body: Value, settings| {
            let (pool, cookie) = (pool.clone(), cookie.clone());
            async move { call(&pool, settings, "PUT", AI, Some(&cookie), Some(body)).await }
        };
        // A refused key does not turn AI on, and is not stored.
        let body = json!({ "api_key": "sk-ant-bad", "model": SONNET, "enabled": true });
        let (status, error, _) = put(body, settings.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error.to_string().contains("refused this API key"), "{error}");
        // Nor does it enable without any key.
        let body = json!({ "model": SONNET, "enabled": true });
        let (status, _, _) = put(body, settings.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // An unknown model is refused before any call.
        let body = json!({ "api_key": KEY, "model": "gpt-9", "enabled": false });
        let (status, _, _) = put(body, settings.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let body = json!({ "api_key": format!(" {KEY}\n"), "model": SONNET, "enabled": true });
        let (status, view, _) = put(body, settings.clone()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(view["enabled"], true);
        assert_eq!(view["has_key"], true);
        assert_eq!(view["model"], SONNET);
        assert!(!view.to_string().contains(KEY));

        let (_, read, _) = call(&pool, settings.clone(), "GET", AI, Some(&cookie), None).await;
        assert_eq!(read, view);
        let wanted = switch::wanted(&pool).await.unwrap().unwrap();
        assert_eq!(wanted.api_key.expose(), KEY);
        assert_eq!(wanted.model, SONNET);

        // Empty keeps the key; nothing changed means no new test.
        let calls = mock.seen.lock().unwrap().len();
        let body = json!({ "api_key": "", "model": SONNET, "enabled": true });
        let (status, _, _) = put(body, settings.clone()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(mock.seen.lock().unwrap().len(), calls);
        // A new model is tested with the stored key.
        let body = json!({ "model": "claude-haiku-5-5", "enabled": true });
        let (status, _, _) = put(body, settings.clone()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(mock.seen.lock().unwrap().last().unwrap().0, KEY);

        // Off keeps the key, ready for the next time.
        let body = json!({ "model": SONNET, "enabled": false });
        let (_, view, _) = put(body, settings.clone()).await;
        assert_eq!((&view["enabled"], &view["has_key"]), (&json!(false), &json!(true)));
        assert_eq!(switch::wanted(&pool).await.unwrap(), None);

        let trail = audit_details(&pool).await;
        assert_eq!(trail.len(), 4);
        assert_eq!(trail[0]["new"]["key_changed"], true);
        assert_eq!(trail[1]["new"]["key_changed"], false);
        assert!(!serde_json::to_string(&trail).unwrap().contains(KEY));
    }

    #[sqlx::test]
    async fn removing_the_key_turns_ai_off(pool: PgPool) {
        let (_mock, settings) = mock_api().await;
        let cookie = admin(&pool).await;
        let body = json!({ "api_key": KEY, "model": SONNET, "enabled": true });
        call(&pool, settings.clone(), "PUT", AI, Some(&cookie), Some(body)).await;
        let (status, view, _) = call(&pool, settings, "DELETE", AI, Some(&cookie), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!((&view["enabled"], &view["has_key"]), (&json!(false), &json!(false)));
        let stored: Option<String> = sqlx::query_scalar("SELECT ai_api_key FROM server_settings")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(stored, None);
    }

    #[sqlx::test]
    async fn the_configuration_takes_precedence_and_locks_the_page(pool: PgPool) {
        let (mock, mut settings) = mock_api().await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("ai.key");
        std::fs::write(&file, format!("{KEY}\n")).unwrap();
        settings.ai.config.enabled = true;
        settings.ai.config.api_key_file = Some(file);
        settings.ai.config.model = "claude-custom".into();
        let cookie = admin(&pool).await;

        let (_, view, _) = call(&pool, settings.clone(), "GET", AI, Some(&cookie), None).await;
        assert_eq!(view["enabled"], true);
        assert_eq!(view["from_config"], true);
        assert_eq!(view["model"], "claude-custom");

        let body = json!({ "api_key": KEY, "model": SONNET, "enabled": false });
        let (status, _, _) =
            call(&pool, settings.clone(), "PUT", AI, Some(&cookie), Some(body)).await;
        assert_eq!(status, StatusCode::CONFLICT);
        let (status, _, _) =
            call(&pool, settings.clone(), "DELETE", AI, Some(&cookie), None).await;
        assert_eq!(status, StatusCode::CONFLICT);

        // The test still works, on what the configuration sets.
        let body = json!({ "api_key": "ignored", "model": SONNET });
        let (_, answer, _) = call(&pool, settings, "POST", TEST, Some(&cookie), Some(body)).await;
        assert_eq!(answer["ok"], true);
        assert_eq!(
            mock.seen.lock().unwrap().last().unwrap(),
            &(KEY.to_owned(), "claude-custom".to_owned())
        );
    }
}
