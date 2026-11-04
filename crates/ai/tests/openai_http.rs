//! The OpenAI adapter against a fake OpenAI-compatible server: tool calls,
//! JSON in the text, malformed answers, a server refusing tools,
//! timeouts, retries, the test and the model list.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::Router;
use monitorflow_ai::openai::{Flavor, OpenAiOptions, OpenAiProvider};
use monitorflow_ai::{AiError, AiProvider, Connection, DataBlock, Step, StepRequest};
use monitorflow_common::config::AiProviderKind;
use monitorflow_common::redact::Secret;
use serde_json::{json, Value};

/// A scripted answer: status, body, delay.
type Reply = (u16, String, Duration);

#[derive(Default)]
struct Fake {
    replies: Mutex<VecDeque<Reply>>,
    seen: Mutex<Vec<(HeaderMap, Value)>>,
}

impl Fake {
    fn seen(&self) -> Vec<(HeaderMap, Value)> {
        self.seen.lock().unwrap().clone()
    }
}

async fn completions(
    State(fake): State<Arc<Fake>>,
    headers: HeaderMap,
    body: String,
) -> (StatusCode, String) {
    fake.seen
        .lock()
        .unwrap()
        .push((headers, serde_json::from_str(&body).unwrap()));
    let (status, body, delay) = fake.replies.lock().unwrap().pop_front().unwrap();
    tokio::time::sleep(delay).await;
    (StatusCode::from_u16(status).unwrap(), body)
}

async fn models(headers: HeaderMap) -> (StatusCode, String) {
    if headers.get("authorization").is_some() {
        return (
            StatusCode::UNAUTHORIZED,
            json!({"error": "no keys here"}).to_string(),
        );
    }
    let list = json!({"object": "list", "data": [
        {"id": "qwen3:8b", "object": "model"}, {"id": "llama3.1:8b", "object": "model"},
        {"id": "qwen3:8b", "object": "model"},
    ]});
    (StatusCode::OK, list.to_string())
}

/// Serves `replies` under `/v1`; returns the fake and its base URL.
async fn serve(replies: Vec<Reply>) -> (Arc<Fake>, String) {
    let fake = Arc::new(Fake::default());
    fake.replies.lock().unwrap().extend(replies);
    let app = Router::new()
        .route("/v1/chat/completions", post(completions))
        .route("/v1/models", get(models))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (fake, format!("http://{addr}/v1"))
}

fn provider(base_url: &str, key: Option<&str>) -> OpenAiProvider {
    let mut options = OpenAiOptions::new(Flavor::Compatible, base_url, "qwen3:8b");
    options.api_key = key.map(Secret::new);
    options.retry_base_delay = Duration::from_millis(1);
    OpenAiProvider::new(options).unwrap()
}

fn ok(message: Value) -> Reply {
    let body = json!({
        "id": "chatcmpl-1", "object": "chat.completion", "model": "qwen3:8b",
        "choices": [{"index": 0, "message": message, "finish_reason": "stop"}],
    });
    (200, body.to_string(), Duration::ZERO)
}

fn tool_call() -> Reply {
    let arguments = json!({"diagnosis": "disk full", "command": "df -h", "reasoning": "usage"});
    ok(json!({"role": "assistant", "content": "", "tool_calls": [
        {"id": "call_1", "type": "function", "function": {"name": "propose_command", "arguments": arguments.to_string()}},
    ]}))
}

fn text(content: &str) -> Reply {
    ok(json!({"role": "assistant", "content": content}))
}

fn error(status: u16, message: &str) -> Reply {
    let body = json!({"error": {"message": message, "type": "api_error"}});
    (status, body.to_string(), Duration::ZERO)
}

fn request() -> StepRequest {
    StepRequest {
        system_prompt: "Diagnose.".into(),
        context: vec![DataBlock {
            title: "incident context".into(),
            content: "disk 97%".into(),
        }],
        history: vec![],
    }
}

fn user_text(body: &Value) -> &str {
    body["messages"][1]["content"].as_str().unwrap()
}

#[tokio::test]
async fn a_tool_call_is_a_step() {
    let (fake, url) = serve(vec![tool_call()]).await;
    let step = provider(&url, Some("sk-local"))
        .next_step(&request())
        .await
        .unwrap();
    assert_eq!(step, Step::run("disk full", "df -h", "usage"));
    let seen = fake.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0["authorization"], "Bearer sk-local");
    assert_eq!(seen[0].1["model"], "qwen3:8b");
    assert_eq!(seen[0].1["tools"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn plain_json_in_the_text_is_a_step_and_no_key_sends_none() {
    let (fake, url) = serve(vec![text(
        "```json\n{\"tool\": \"stop\", \"diagnosis\": \"fine\", \"outcome\": \"resolved\", \"reason\": \"usage dropped\"}\n```",
    )])
    .await;
    let step = provider(&url, None).next_step(&request()).await.unwrap();
    assert_eq!(
        step,
        Step::stop(
            "fine",
            monitorflow_ai::StopOutcome::Resolved,
            "usage dropped"
        )
    );
    assert!(fake.seen()[0].0.get("authorization").is_none());
}

#[tokio::test]
async fn a_malformed_answer_is_asked_again_once_then_refused() {
    let (fake, url) = serve(vec![text("I think you should run df -h"), tool_call()]).await;
    let step = provider(&url, None).next_step(&request()).await.unwrap();
    assert_eq!(step, Step::run("disk full", "df -h", "usage"));
    let seen = fake.seen();
    assert_eq!(seen.len(), 2);
    assert!(!user_text(&seen[0].1).contains("could not be used"));
    assert!(user_text(&seen[1].1).contains("could not be used: no tool call and no JSON object"));

    let two_lines = r#"{"tool": "propose_command", "diagnosis": "d", "command": "df -h\nreboot", "reasoning": "r"}"#;
    let (fake, url) = serve(vec![text("sure!"), text(two_lines)]).await;
    match provider(&url, None).next_step(&request()).await {
        Err(AiError::Malformed(reason)) => assert!(reason.contains("line break"), "{reason}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(fake.seen().len(), 2, "asked again once, not more");
}

#[tokio::test]
async fn a_server_refusing_tools_gets_json_requests_from_then_on() {
    let refused = error(
        400,
        "registry.ollama.ai/library/gemma3:4b does not support tools",
    );
    let json_answer = r#"{"tool": "propose_command", "diagnosis": "disk full", "command": "df -h", "reasoning": "usage"}"#;
    let (fake, url) = serve(vec![refused, text(json_answer), text(json_answer)]).await;
    let provider = provider(&url, None);
    for _ in 0..2 {
        let step = provider.next_step(&request()).await.unwrap();
        assert_eq!(step, Step::run("disk full", "df -h", "usage"));
    }
    let seen = fake.seen();
    assert_eq!(seen.len(), 3);
    assert!(seen[0].1.get("tools").is_some());
    for (_, body) in &seen[1..] {
        assert!(body.get("tools").is_none() && body.get("tool_choice").is_none());
        assert!(user_text(body).ends_with("as one JSON object, nothing else."));
    }
}

#[tokio::test]
async fn other_client_errors_are_not_retried_or_downgraded() {
    let (fake, url) = serve(vec![error(400, "context length exceeded")]).await;
    match provider(&url, None).next_step(&request()).await {
        Err(AiError::Api {
            status: 400,
            message,
            ..
        }) => assert_eq!(message, "context length exceeded"),
        other => panic!("{other:?}"),
    }
    assert_eq!(fake.seen().len(), 1);
}

#[tokio::test]
async fn transient_errors_are_retried_and_slow_answers_time_out() {
    let (fake, url) = serve(vec![error(503, "loading model"), tool_call()]).await;
    provider(&url, None).next_step(&request()).await.unwrap();
    assert_eq!(fake.seen().len(), 2);

    let slow = |reply: Reply| (reply.0, reply.1, Duration::from_secs(5));
    let (fake, url) = serve(vec![slow(tool_call()), slow(tool_call())]).await;
    let mut options = OpenAiOptions::new(Flavor::Compatible, url, "qwen3:8b");
    options.request_timeout = Duration::from_millis(200);
    options.max_retries = 1;
    options.retry_base_delay = Duration::from_millis(1);
    let started = tokio::time::Instant::now();
    match OpenAiProvider::new(options)
        .unwrap()
        .next_step(&request())
        .await
    {
        Err(error @ AiError::Transport(_)) => assert!(error.is_transient()),
        other => panic!("{other:?}"),
    }
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(fake.seen().len(), 2);
}

#[tokio::test]
async fn refusals_and_truncation_end_the_step() {
    let cut = json!({"choices": [{"message": {"role": "assistant", "content": "{\"tool\": "}, "finish_reason": "length"}]});
    let (fake, url) = serve(vec![(200, cut.to_string(), Duration::ZERO)]).await;
    assert!(matches!(
        provider(&url, None).next_step(&request()).await,
        Err(AiError::Truncated)
    ));
    assert_eq!(
        fake.seen().len(),
        1,
        "a truncated answer is not asked again"
    );
}

#[tokio::test]
async fn the_test_and_the_model_list_need_no_key_on_a_local_server() {
    let (fake, url) = serve(vec![text("pong"), error(404, "model 'nope' not found")]).await;
    let mut connection = Connection {
        kind: AiProviderKind::OpenAiCompatible,
        api_key: None,
        base_url: Some(url.clone()),
        model: "qwen3:8b".into(),
        max_tokens: None,
        request_timeout: Duration::from_secs(10),
    };
    connection.check().await.unwrap();
    let ping = &fake.seen()[0].1;
    assert_eq!(ping["max_tokens"], 16);
    assert!(ping.get("tools").is_none());
    assert_eq!(
        connection.models().await.unwrap(),
        ["llama3.1:8b", "qwen3:8b"]
    );

    connection.model = "nope".into();
    let error = connection.check().await.unwrap_err();
    assert!(error.contains("does not serve this model"), "{error}");
    connection.api_key = Some(Secret::new("sk-x"));
    let error = connection.models().await.unwrap_err();
    assert!(error.contains("refused the API key"), "{error}");

    connection.base_url = Some("http://8.8.8.8/v1".into());
    let error = connection.check().await.unwrap_err();
    assert!(error.contains("must start with https://"), "{error}");
    connection.base_url = None;
    let error = connection.check().await.unwrap_err();
    assert!(error.contains("enter the address"), "{error}");
}

#[tokio::test]
async fn openai_is_asked_for_strict_tools() {
    let (fake, url) = serve(vec![tool_call()]).await;
    let connection = Connection {
        kind: AiProviderKind::OpenAi,
        api_key: Some(Secret::new("sk-proj-1")),
        base_url: Some(url),
        model: "gpt-5.1".into(),
        max_tokens: Some(2000),
        request_timeout: Duration::from_secs(10),
    };
    let provider = connection.provider().unwrap();
    assert_eq!(provider.name(), "openai");
    provider.next_step(&request()).await.unwrap();
    let body = &fake.seen()[0].1;
    assert_eq!(body["max_completion_tokens"], 2000);
    assert_eq!(body["tool_choice"], "required");
    assert_eq!(body["tools"][0]["function"]["strict"], true);

    let keyless = Connection {
        api_key: None,
        ..connection
    };
    assert!(keyless
        .check()
        .await
        .unwrap_err()
        .contains("OpenAI refused this API key"));
}
