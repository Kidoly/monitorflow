//! HTTP plumbing shared by the VictoriaMetrics and VictoriaLogs sinks.

use std::time::Duration;

use super::writer::WriteError;

/// Time allowed for one request, connection included.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Bytes of an error response kept in the error message.
const MAX_ERROR_BODY: usize = 256;

pub fn client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(Duration::from_secs(5))
        .build()
}

/// Sends `request` and classifies the outcome. Error messages never carry
/// the URL, which may hold credentials.
pub async fn send(request: reqwest::RequestBuilder) -> Result<(), WriteError> {
    let response = request
        .send()
        .await
        .map_err(|e| WriteError::Retryable(e.without_url().to_string()))?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let body = response.text().await.unwrap_or_default();
    let mut end = body.len().min(MAX_ERROR_BODY);
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    let message = format!("HTTP {status}: {}", body[..end].trim());
    Err(classify(status, message))
}

/// 429 and 5xx are worth retrying; other statuses mean the data was refused.
fn classify(status: reqwest::StatusCode, message: String) -> WriteError {
    if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        WriteError::Retryable(message)
    } else {
        WriteError::Permanent(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;

    #[test]
    fn server_errors_and_429_are_retryable() {
        let retryable = |code| matches!(classify(code, String::new()), WriteError::Retryable(_));
        assert!(retryable(StatusCode::INTERNAL_SERVER_ERROR));
        assert!(retryable(StatusCode::SERVICE_UNAVAILABLE));
        assert!(retryable(StatusCode::TOO_MANY_REQUESTS));
        assert!(!retryable(StatusCode::BAD_REQUEST));
        assert!(!retryable(StatusCode::NOT_FOUND));
    }
}
