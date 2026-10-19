//! Read client for VictoriaMetrics' Prometheus-compatible query API
//! (`/api/v1/query`, `/api/v1/query_range`, `/api/v1/series`).
//!
//! It only sends what the caller passes: never a header, cookie or
//! credential of the API request it serves. Requests have a timeout,
//! redirects are not followed and responses are read up to a size cap.
//! Error messages never carry the URL, which may hold credentials.

use std::time::Duration;

use serde_json::Value;

/// Time allowed for one query, connection included.
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(15);
/// Largest response body read, in bytes.
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
/// Bytes of an error response kept in the error message.
const MAX_ERROR_BODY: usize = 256;

#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    /// Connection refused, reset, DNS, 5xx or 429.
    #[error("unavailable: {0}")]
    Unavailable(String),
    #[error("timed out")]
    Timeout,
    /// A 4xx other than 429: the backend refused the query, for instance
    /// over one of its own limits.
    #[error("rejected: {0}")]
    Rejected(String),
    /// Not the Prometheus JSON envelope, or larger than the cap.
    #[error("malformed response: {0}")]
    Malformed(String),
}

/// Cheap to clone; shares one connection pool.
#[derive(Debug, Clone)]
pub struct VmQuery {
    base: String,
    client: reqwest::Client,
}

impl VmQuery {
    pub fn new(base: &str) -> Result<Self, reqwest::Error> {
        Self::with_timeout(base, QUERY_TIMEOUT)
    }

    pub fn with_timeout(base: &str, timeout: Duration) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(Duration::from_secs(5).min(timeout))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            base: base.trim_end_matches('/').to_owned(),
            client,
        })
    }

    /// GETs `path` with `params` and returns the `data` member of a
    /// `{"status": "success"}` answer.
    pub async fn get(&self, path: &str, params: &[(&str, String)]) -> Result<Value, QueryError> {
        let response = self
            .client
            .get(format!("{}{path}", self.base))
            .query(params)
            .send()
            .await
            .map_err(transport)?;
        let status = response.status();
        let body = read_capped(response).await?;
        if !status.is_success() {
            let message = format!("HTTP {status}: {}", excerpt(&body));
            return Err(
                if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    QueryError::Unavailable(message)
                } else {
                    QueryError::Rejected(message)
                },
            );
        }
        let mut envelope: Value =
            serde_json::from_slice(&body).map_err(|e| QueryError::Malformed(e.to_string()))?;
        if envelope["status"] != "success" {
            return Err(QueryError::Malformed(excerpt(&body)));
        }
        Ok(envelope["data"].take())
    }
}

fn transport(error: reqwest::Error) -> QueryError {
    if error.is_timeout() {
        QueryError::Timeout
    } else {
        QueryError::Unavailable(error.without_url().to_string())
    }
}

async fn read_capped(mut response: reqwest::Response) -> Result<Vec<u8>, QueryError> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport)? {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(QueryError::Malformed(format!(
                "response larger than {MAX_RESPONSE_BYTES} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn excerpt(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let mut end = text.len().min(MAX_ERROR_BODY);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excerpts_are_short_and_on_char_boundaries() {
        assert_eq!(excerpt(b"  oops \n"), "oops");
        let long = "é".repeat(200);
        let cut = excerpt(long.as_bytes());
        assert!(cut.len() <= MAX_ERROR_BODY && cut.chars().all(|c| c == 'é'));
    }

    #[test]
    fn base_url_loses_its_trailing_slash() {
        let client = VmQuery::new("http://vm:8428/").unwrap();
        assert_eq!(client.base, "http://vm:8428");
    }
}
