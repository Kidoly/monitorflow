//! The Claude adapter against a local mock of the Messages API.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::Router;
use monitorflow_ai::claude::{ClaudeOptions, ClaudeProvider};
use monitorflow_ai::{AiError, AiProvider, DataBlock, Step, StepRequest};
use monitorflow_common::config::AiConfig;
use monitorflow_common::redact::Secret;
use serde_json::{json, Value};

const KEY: &str = "sk-ant-test-key";

/// A scripted answer: status, extra headers, body, delay.
type Reply = (u16, Vec<(&'static str, &'static str)>, String, Duration);

#[derive(Default)]
struct Mock {
    replies: Mutex<VecDeque<Reply>>,
    seen: Mutex<Vec<(HeaderMap, Value)>>,
}

async fn messages(
    State(mock): State<Arc<Mock>>,
    headers: HeaderMap,
    body: String,
) -> (StatusCode, HeaderMap, String) {
    mock.seen
        .lock()
        .unwrap()
        .push((headers, serde_json::from_str(&body).unwrap()));
    let (status, extra, body, delay) = mock.replies.lock().unwrap().pop_front().unwrap();
    tokio::time::sleep(delay).await;
    let mut out = HeaderMap::new();
    for (name, value) in extra {
        out.insert(name, value.parse().unwrap());
    }
    (StatusCode::from_u16(status).unwrap(), out, body)
}

async fn serve(replies: Vec<Reply>) -> (Arc<Mock>, ClaudeOptions) {
    let mock = Arc::new(Mock::default());
    mock.replies.lock().unwrap().extend(replies);
    let app = Router::new()
        .route("/v1/messages", post(messages))
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut options = ClaudeOptions::new(Secret::new(KEY));
    options.base_url = format!("http://{addr}/");
    options.retry_base_delay = Duration::from_millis(1);
    (mock, options)
}

fn ok() -> Reply {
    let body = json!({
        "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-opus-5-5",
        "stop_reason": "tool_use", "stop_details": null,
        "content": [{"type": "tool_use", "id": "toolu_1", "name": "propose_command",
            "input": {"diagnosis": "disk full", "command": "df -h", "reasoning": "usage"}}],
    });
    (200, vec![], body.to_string(), Duration::ZERO)
}

fn error(status: u16, kind: &str, headers: Vec<(&'static str, &'static str)>) -> Reply {
    let body = json!({"type": "error", "error": {"type": kind, "message": "nope"}});
    (status, headers, body.to_string(), Duration::ZERO)
}

fn request() -> StepRequest {
    StepRequest {
        system_prompt: "Diagnose.".into(),
        context: vec![DataBlock {
            title: "Incident".into(),
            content: "disk at 97%".into(),
        }],
        history: Vec::new(),
    }
}

#[tokio::test]
async fn sends_authenticated_request_and_parses_the_step() {
    let (mock, options) = serve(vec![ok()]).await;
    let provider = ClaudeProvider::new(options).unwrap();
    let step = provider.next_step(&request()).await.unwrap();
    assert_eq!(step, Step::run("disk full", "df -h", "usage"));

    let seen = mock.seen.lock().unwrap();
    let (headers, body) = &seen[0];
    assert_eq!(headers["x-api-key"], KEY);
    assert_eq!(headers["anthropic-version"], "2023-06-01");
    assert_eq!(headers["anthropic-beta"], "server-side-fallback-2026-07-01");
    assert_eq!(headers["content-type"], "application/json");
    assert_eq!(body["model"], "claude-opus-5-5");
    assert_eq!(body["max_tokens"], 16_000);
}

#[tokio::test]
async fn retries_overload_and_server_errors() {
    let replies = vec![
        error(529, "overloaded_error", vec![]),
        error(500, "api_error", vec![]),
        ok(),
    ];
    let (mock, options) = serve(replies).await;
    let step = ClaudeProvider::new(options)
        .unwrap()
        .next_step(&request())
        .await;
    assert!(step.is_ok(), "{step:?}");
    assert_eq!(mock.seen.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn honors_retry_after_on_rate_limits() {
    let limited = error(429, "rate_limit_error", vec![("retry-after", "1")]);
    let (mock, options) = serve(vec![limited, ok()]).await;
    let started = Instant::now();
    ClaudeProvider::new(options)
        .unwrap()
        .next_step(&request())
        .await
        .unwrap();
    assert!(started.elapsed() >= Duration::from_secs(1));
    assert_eq!(mock.seen.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn gives_up_after_max_retries() {
    let replies = (0..3).map(|_| error(503, "api_error", vec![])).collect();
    let (mock, mut options) = serve(replies).await;
    options.max_retries = 2;
    let error = ClaudeProvider::new(options)
        .unwrap()
        .next_step(&request())
        .await
        .unwrap_err();
    assert!(
        matches!(error, AiError::Api { status: 503, .. }),
        "{error:?}"
    );
    assert!(error.is_transient());
    assert_eq!(mock.seen.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn client_errors_are_not_retried() {
    let bad = error(400, "invalid_request_error", vec![("request-id", "req_42")]);
    let (mock, options) = serve(vec![bad]).await;
    let error = ClaudeProvider::new(options)
        .unwrap()
        .next_step(&request())
        .await
        .unwrap_err();
    match &error {
        AiError::Api {
            status,
            error_type,
            request_id,
            ..
        } => {
            assert_eq!(*status, 400);
            assert_eq!(error_type.as_deref(), Some("invalid_request_error"));
            assert_eq!(request_id.as_deref(), Some("req_42"));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(mock.seen.lock().unwrap().len(), 1);
    assert!(!error.to_string().contains(KEY));
}

#[tokio::test]
async fn times_out_and_retries_slow_answers() {
    let (_, _, body, _) = ok();
    let slow = (200, vec![], body, Duration::from_secs(5));
    let (mock, mut options) = serve(vec![slow, ok()]).await;
    options.request_timeout = Duration::from_millis(200);
    let step = ClaudeProvider::new(options.clone())
        .unwrap()
        .next_step(&request())
        .await;
    assert!(step.is_ok(), "{step:?}");
    assert_eq!(mock.seen.lock().unwrap().len(), 2);

    let (_, _, body, _) = ok();
    mock.replies
        .lock()
        .unwrap()
        .push_back((200, vec![], body, Duration::from_secs(5)));
    options.max_retries = 0;
    let error = ClaudeProvider::new(options)
        .unwrap()
        .next_step(&request())
        .await
        .unwrap_err();
    assert!(matches!(error, AiError::Transport(_)), "{error:?}");
}

#[tokio::test]
async fn malformed_success_is_not_retried() {
    let text = json!({"type": "message", "stop_reason": "end_turn",
        "content": [{"type": "text", "text": "just run df"}]});
    let (mock, options) = serve(vec![(200, vec![], text.to_string(), Duration::ZERO)]).await;
    let error = ClaudeProvider::new(options)
        .unwrap()
        .next_step(&request())
        .await
        .unwrap_err();
    assert!(matches!(error, AiError::Malformed(_)), "{error:?}");
    assert_eq!(mock.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn built_from_config() {
    assert!(monitorflow_ai::from_config(&AiConfig::default())
        .await
        .unwrap()
        .is_none());

    let dir = tempfile::tempdir().unwrap();
    let key_file = dir.path().join("key");
    std::fs::write(&key_file, format!("{KEY}\n")).unwrap();
    let config = AiConfig {
        enabled: true,
        api_key_file: Some(key_file),
        model: "claude-sonnet-5-5".into(),
        ..AiConfig::default()
    };
    let provider = monitorflow_ai::from_config(&config).await.unwrap().unwrap();
    assert_eq!(provider.name(), "claude");

    let missing = AiConfig {
        api_key_file: Some(dir.path().join("missing")),
        ..config
    };
    let error = monitorflow_ai::from_config(&missing).await.unwrap_err();
    assert!(matches!(error, AiError::ApiKeyFile { .. }), "{error:?}");
}
