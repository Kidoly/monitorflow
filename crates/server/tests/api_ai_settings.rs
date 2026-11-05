//! AI setup in the web interface (ADR-0064, ADR-0096): who reads and
//! changes it, the key never coming back, the test against mocks of the
//! Messages and Chat Completions APIs, providers and local models, the
//! audit, and the configuration's precedence. Ignored without
//! `DATABASE_URL`.

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::routing::{get, post};
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
const OPENAI_KEY: &str = "sk-proj-good-0123456789";
const MODELS: &str = "/api/settings/ai/models";

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

/// Chat Completions: accepts no key or [`OPENAI_KEY`], and any model but
/// `missing`.
async fn completions(
    State(mock): State<Mock>,
    headers: HeaderMap,
    body: String,
) -> (StatusCode, String) {
    let key = headers
        .get("authorization")
        .map(|v| v.to_str().unwrap().trim_start_matches("Bearer ").to_owned())
        .unwrap_or_default();
    let body: Value = serde_json::from_str(&body).unwrap();
    let model = body["model"].as_str().unwrap().to_owned();
    mock.seen.lock().unwrap().push((key.clone(), model.clone()));
    if !key.is_empty() && key != OPENAI_KEY {
        let error =
            json!({"error": {"message": "Incorrect API key", "type": "invalid_request_error"}});
        return (StatusCode::UNAUTHORIZED, error.to_string());
    }
    if model == "missing" {
        return (
            StatusCode::NOT_FOUND,
            json!({"error": "model 'missing' not found"}).to_string(),
        );
    }
    let answer = json!({"choices": [{"message": {"role": "assistant", "content": "pong"}, "finish_reason": "length"}]});
    (StatusCode::OK, answer.to_string())
}

async fn model_list() -> String {
    json!({"data": [{"id": "qwen3:8b"}, {"id": "gpt-5.1"}, {"id": "whisper-1"}]}).to_string()
}

/// The mock serves both APIs; returns the address of an
/// OpenAI-compatible server on it.
async fn mock_api() -> (Mock, ApiSettings) {
    let (mock, settings, _) = mock_apis().await;
    (mock, settings)
}

async fn mock_apis() -> (Mock, ApiSettings, String) {
    let mock = Mock::default();
    let app = Router::new()
        .route("/v1/messages", post(messages))
        .route("/v1/chat/completions", post(completions))
        .route("/v1/models", get(model_list))
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut settings = ApiSettings::default();
    settings.ai.base_url = format!("http://{addr}");
    settings.ai.openai_base_url = format!("http://{addr}/v1");
    (mock, settings, format!("http://{addr}/v1"))
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
            ("POST", MODELS, Some(json!({}))),
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
        assert_eq!(wanted.api_key.unwrap().expose(), KEY);
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

    #[sqlx::test]
    async fn a_customers_administrator_cannot_point_ai_anywhere(pool: PgPool) {
        let (mock, settings, local) = mock_apis().await;
        let customer: uuid::Uuid =
            sqlx::query_scalar("INSERT INTO customers (name) VALUES ('acme') RETURNING id")
                .fetch_one(&pool)
                .await
                .unwrap();
        let cookie = login(&pool, "acme-admin", Role::Administrator, GrantScope::Customer(customer)).await;
        let form = json!({ "provider": "openai_compatible", "base_url": local, "model": "qwen3:8b" });
        let mut enable = form.clone();
        enable["enabled"] = json!(true);
        for (method, uri, body) in [("PUT", AI, enable), ("POST", TEST, form.clone()), ("POST", MODELS, form)] {
            let (status, _, _) = call(&pool, settings.clone(), method, uri, Some(&cookie), Some(body)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
        }
        assert!(mock.seen.lock().unwrap().is_empty(), "nothing was sent");
        // It still reads whether AI is on, which Approvals shows.
        let (status, view, _) = call(&pool, settings, "GET", AI, Some(&cookie), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(view["provider"], "anthropic");
    }

    #[sqlx::test]
    async fn a_local_model_needs_an_address_and_no_key(pool: PgPool) {
        let (mock, settings, local) = mock_apis().await;
        let cookie = admin(&pool).await;
        let put = |body: Value| {
            let (pool, cookie, settings) = (pool.clone(), cookie.clone(), settings.clone());
            async move { call(&pool, settings, "PUT", AI, Some(&cookie), Some(body)).await }
        };
        // Its models are listed from the server, without a key.
        let form = json!({ "provider": "openai_compatible", "base_url": local });
        let (status, list, _) = call(&pool, settings.clone(), "POST", MODELS, Some(&cookie), Some(form)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list, json!({ "models": ["gpt-5.1", "qwen3:8b", "whisper-1"], "error": null }));

        let (status, error, _) = put(json!({ "provider": "openai_compatible", "model": "qwen3:8b", "enabled": true })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error.to_string().contains("enter the address"), "{error}");
        // Plain HTTP to a public address is refused before anything is sent.
        let public = json!({ "provider": "openai_compatible", "base_url": "http://8.8.8.8:11434/v1", "model": "qwen3:8b", "enabled": true });
        let (status, error, _) = put(public).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error.to_string().contains("must start with https://"), "{error}");
        let missing = json!({ "provider": "openai_compatible", "base_url": local, "model": "missing", "enabled": true });
        let (status, error, _) = put(missing).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error.to_string().contains("does not serve this model"), "{error}");

        let body = json!({ "provider": "openai_compatible", "base_url": format!("{local}/"), "model": " qwen3:8b ", "enabled": true });
        let (status, view, _) = put(body).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["provider"], "openai_compatible");
        assert_eq!(view["base_url"], local);
        assert_eq!(view["model"], "qwen3:8b");
        assert_eq!((&view["enabled"], &view["has_key"]), (&json!(true), &json!(false)));
        let wanted = switch::wanted(&pool).await.unwrap().unwrap();
        assert_eq!(wanted.provider, monitorflow_common::config::AiProviderKind::OpenAiCompatible);
        assert_eq!((wanted.api_key, wanted.base_url.as_deref()), (None, Some(local.as_str())));
        assert_eq!(mock.seen.lock().unwrap().last().unwrap(), &(String::new(), "qwen3:8b".to_owned()));

        // The test takes the stored address when the form gives none.
        let (_, answer, _) = call(&pool, settings.clone(), "POST", TEST, Some(&cookie), Some(json!({}))).await;
        assert_eq!(answer, json!({ "ok": true, "error": null }));
        let trail = audit_details(&pool).await;
        assert_eq!(trail.last().unwrap()["new"]["provider"], "openai_compatible");
        assert_eq!(trail.last().unwrap()["new"]["base_url"], local);
    }

    #[sqlx::test]
    async fn a_key_stays_with_the_provider_it_was_entered_for(pool: PgPool) {
        let (mock, settings, local) = mock_apis().await;
        let cookie = admin(&pool).await;
        let put = |body: Value| {
            let (pool, cookie, settings) = (pool.clone(), cookie.clone(), settings.clone());
            async move { call(&pool, settings, "PUT", AI, Some(&cookie), Some(body)).await }
        };
        let (status, _, _) = put(json!({ "api_key": KEY, "model": SONNET, "enabled": true })).await;
        assert_eq!(status, StatusCode::OK);

        // OpenAI with Anthropic's key stored: it is never sent there.
        let (status, error, _) = put(json!({ "provider": "openai", "model": "gpt-5.1", "enabled": true })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error.to_string().contains("enter an API key"), "{error}");
        let (_, answer, _) = call(&pool, settings.clone(), "POST", TEST, Some(&cookie), Some(json!({ "provider": "openai" }))).await;
        assert!(answer["error"].as_str().unwrap().contains("Enter an API key"));
        let (_, list, _) = call(&pool, settings.clone(), "POST", MODELS, Some(&cookie), Some(json!({ "provider": "openai", "api_key": OPENAI_KEY }))).await;
        assert_eq!(list["models"], json!(["gpt-5.1"]), "only models that chat");
        let (status, view, _) = put(json!({ "provider": "openai", "api_key": OPENAI_KEY, "model": "gpt-5.1", "enabled": true })).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!((&view["provider"], &view["has_key"], &view["base_url"]), (&json!("openai"), &json!(true), &Value::Null));
        assert_eq!(mock.seen.lock().unwrap().last().unwrap(), &(OPENAI_KEY.to_owned(), "gpt-5.1".to_owned()));

        // A local model needs no key: OpenAI's is forgotten, not sent.
        let body = json!({ "provider": "openai_compatible", "base_url": local, "model": "qwen3:8b", "enabled": true });
        let (status, view, _) = put(body).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["has_key"], false);
        assert_eq!(mock.seen.lock().unwrap().last().unwrap().0, "");
        let stored: Option<String> = sqlx::query_scalar("SELECT ai_api_key FROM server_settings")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(stored, None);
        assert!(!serde_json::to_string(&audit_details(&pool).await).unwrap().contains(OPENAI_KEY));
    }
}
