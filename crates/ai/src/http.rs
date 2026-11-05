//! What every adapter does over HTTP: retries of transient failures and
//! API errors read from the body, whichever shape the provider gives them.

use std::time::Duration;

use serde_json::Value;

use crate::AiError;

/// Longest wait between attempts, `retry-after` included.
pub(crate) const MAX_RETRY_DELAY: Duration = Duration::from_secs(60);
/// Characters of an API error message kept in [`AiError::Api`].
pub(crate) const MAX_ERROR_MESSAGE: usize = 512;

/// How a provider retries: attempts after the first on 408, 409, 429, 5xx
/// and transport errors, and the first backoff without `retry-after`,
/// doubled at each attempt.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Retries {
    pub max: u32,
    pub base_delay: Duration,
}

/// A client with the provider's timeout, never following a redirect to
/// another host with the key.
pub(crate) fn client(timeout: Duration) -> Result<reqwest::Client, AiError> {
    reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| AiError::Transport(e.without_url().to_string()))
}

/// Sends what `request` builds, again for transient failures, and returns
/// the body of the first successful answer.
pub(crate) async fn post(
    retries: Retries,
    request: impl Fn() -> reqwest::RequestBuilder,
) -> Result<Vec<u8>, AiError> {
    let mut attempt = 0;
    loop {
        let (error, retry_after) = match send(request()).await {
            Ok(bytes) => return Ok(bytes),
            Err(failure) => failure,
        };
        if attempt >= retries.max || !error.is_transient() {
            return Err(error);
        }
        let delay = retry_after
            .unwrap_or_else(|| backoff(retries.base_delay, attempt))
            .min(MAX_RETRY_DELAY);
        attempt += 1;
        tracing::warn!(attempt, delay_ms = delay.as_millis() as u64, %error, "retrying AI request");
        tokio::time::sleep(delay).await;
    }
}

/// One attempt: the body of a 2xx answer, or the error with the
/// `retry-after` the API asked for.
pub(crate) async fn send(
    request: reqwest::RequestBuilder,
) -> Result<Vec<u8>, (AiError, Option<Duration>)> {
    let transport = |e: reqwest::Error| (AiError::Transport(e.without_url().to_string()), None);
    let response = request.send().await.map_err(transport)?;
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.bytes().await.map_err(transport)?;
    if status.is_success() {
        return Ok(bytes.to_vec());
    }
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let request_id = header("request-id").or_else(|| header("x-request-id"));
    let error = api_error(status.as_u16(), &bytes, request_id);
    Err((error, header("retry-after").and_then(parse_retry_after)))
}

pub(crate) fn backoff(base: Duration, attempt: u32) -> Duration {
    base.saturating_mul(1 << attempt.min(16))
}

/// `retry-after` in seconds; the HTTP-date form is ignored.
pub(crate) fn parse_retry_after(value: &str) -> Option<Duration> {
    let seconds: f64 = value.trim().parse().ok()?;
    (seconds.is_finite() && seconds >= 0.0).then(|| Duration::from_secs_f64(seconds))
}

/// The error of a failed answer. Anthropic and OpenAI wrap it in `error`,
/// vLLM puts it at the top, Ollama sometimes gives `error` as a string;
/// anything else keeps the body as text.
pub(crate) fn api_error(status: u16, body: &[u8], request_id: Option<&str>) -> AiError {
    let value: Option<Value> = serde_json::from_slice(body).ok();
    let detail = value
        .as_ref()
        .map(|v| v.get("error").filter(|e| e.is_object()).unwrap_or(v));
    let text = |v: Option<&Value>, key: &str| {
        v.and_then(|v| v.get(key))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let message = text(detail, "message")
        .or_else(|| text(value.as_ref(), "error"))
        .unwrap_or_else(|| String::from_utf8_lossy(body).into_owned());
    let error_type = text(detail, "type").or_else(|| text(detail, "code"));
    AiError::Api {
        status,
        error_type,
        message: message.chars().take(MAX_ERROR_MESSAGE).collect(),
        request_id: request_id.map(str::to_owned),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_errors_of_every_shape() {
        let body =
            br#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
        let error = api_error(529, body, Some("req_1"));
        assert!(error.is_transient());
        assert_eq!(
            error.to_string(),
            "AI API answered HTTP 529 (overloaded_error): Overloaded"
        );
        let error = api_error(400, &[b'x'; 2000], None);
        assert!(!error.is_transient());
        match error {
            AiError::Api {
                error_type,
                message,
                ..
            } => {
                assert_eq!(error_type, None);
                assert_eq!(message.len(), MAX_ERROR_MESSAGE);
            }
            other => panic!("{other:?}"),
        }
        for (body, kind, message) in [
            (
                r#"{"error":{"message":"No credit","type":null,"code":"insufficient_quota"}}"#,
                Some("insufficient_quota"),
                "No credit",
            ),
            (
                r#"{"object":"error","message":"too long","type":"BadRequestError","code":400}"#,
                Some("BadRequestError"),
                "too long",
            ),
            (
                r#"{"error":"model 'x' not found"}"#,
                None,
                "model 'x' not found",
            ),
        ] {
            match api_error(404, body.as_bytes(), None) {
                AiError::Api {
                    error_type,
                    message: got,
                    ..
                } => {
                    assert_eq!(error_type.as_deref(), kind, "{body}");
                    assert_eq!(got, message);
                }
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn retry_delays() {
        assert_eq!(parse_retry_after("2"), Some(Duration::from_secs(2)));
        assert_eq!(parse_retry_after(" 0.5 "), Some(Duration::from_millis(500)));
        assert_eq!(parse_retry_after("Wed, 21 Oct 2026 07:28:00 GMT"), None);
        assert_eq!(parse_retry_after("-1"), None);
        let base = Duration::from_secs(1);
        assert_eq!(backoff(base, 0), base);
        assert_eq!(backoff(base, 3), Duration::from_secs(8));
        assert!(backoff(base, 40) >= MAX_RETRY_DELAY);
    }
}
