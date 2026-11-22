//! The HaloPSA API (ADR-0164, ADR-0166): OAuth2 client credentials on
//! `{base}/auth/token` with the scope `all`, then the API under
//! `{base}/api` with the Bearer token.
//!
//! Requests follow no redirects and have a timeout; answers are read up to
//! [`MAX_ANSWER`]. The token, the secret and ticket text never appear in
//! errors or logs: an error is Halo's own text, cut short, with the secret
//! taken out in case Halo echoes it.

use std::time::{Duration, Instant};

use monitorflow_common::redact::{Secret, REDACTED};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::notify::send::{http_client, request_error};

/// The scope asked for on the token request (ADR-0166).
pub const SCOPE: &str = "all";
/// The most of an answer read; the rest is refused.
pub const MAX_ANSWER: usize = 4 << 20;
/// Characters of Halo's error text kept.
const ERROR_TEXT: usize = 300;
/// A token is used until this long before it expires.
const TOKEN_MARGIN: Duration = Duration::from_secs(60);
/// The token's life when Halo does not say.
const DEFAULT_TOKEN_LIFE: Duration = Duration::from_secs(3600);

/// Why a request to Halo failed, in plain words.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct HaloError {
    pub message: String,
    /// Halo's HTTP status, when it answered.
    pub status: Option<u16>,
}

impl HaloError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: None,
        }
    }

    /// Whether trying again later may work: no answer, a rate limit or a
    /// server error. A 4xx is Halo saying the request or the setup is
    /// wrong, which a retry does not change (ADR-0164).
    pub fn retryable(&self) -> bool {
        match self.status {
            None => true,
            Some(status) => status == 429 || status >= 500,
        }
    }
}

/// What a connection needs to sign in.
#[derive(Debug, Clone)]
pub struct Credentials {
    /// Like `https://halo.example.com`, without a trailing slash or `/api`.
    pub base_url: String,
    pub tenant: Option<String>,
    pub client_id: String,
    pub client_secret: Secret,
}

/// The agent the application acts as.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Agent {
    pub id: i64,
    #[serde(default)]
    pub name: String,
}

struct Token {
    value: Secret,
    until: Instant,
}

/// A client for one Halo instance, keeping its token until shortly before
/// it expires. Never stored: client credentials can always ask again.
pub struct Halo {
    http: reqwest::Client,
    credentials: Credentials,
    token: Mutex<Option<Token>>,
}

impl std::fmt::Debug for Halo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Halo")
            .field("base_url", &self.credentials.base_url)
            .finish_non_exhaustive()
    }
}

/// `url` checked and made the base of the instance: http or https, no
/// user, password, query or fragment, and without a trailing `/api` or
/// `/auth`, which people paste from Halo's pages. The error is a sentence
/// for the form.
pub fn normalize_base_url(url: &str) -> Result<String, String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("Enter the address of your HaloPSA, like https://halo.example.com.".into());
    }
    let parsed = reqwest::Url::parse(url)
        .map_err(|_| "Enter a full address, like https://halo.example.com.".to_owned())?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err("The address starts with https://, like https://halo.example.com.".into());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("Put no user or password in the address: use the Client ID and Secret.".into());
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err("Enter the address without anything after ? or #.".into());
    }
    let mut base = parsed.as_str().trim_end_matches('/').to_owned();
    for suffix in ["/api", "/auth"] {
        if let Some(stripped) = base.strip_suffix(suffix) {
            base = stripped.to_owned();
        }
    }
    Ok(base)
}

/// The host of `base`, for messages.
fn host(base: &str) -> String {
    reqwest::Url::parse(base)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The body of `response`, at most [`MAX_ANSWER`] bytes.
async fn read(mut response: reqwest::Response, host: &str) -> Result<Vec<u8>, HaloError> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| HaloError::new(request_error(host, e).message))?
    {
        if body.len() + chunk.len() > MAX_ANSWER {
            return Err(HaloError::new(format!(
                "{host} sent an answer larger than {} MiB.",
                MAX_ANSWER >> 20
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Halo's error text for a refusal: the `error_description` or `message`
/// of a JSON answer, or the plain text some instances send, cut short.
fn error_text(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let json: Option<Value> = serde_json::from_str(&text).ok();
    let picked = json
        .as_ref()
        .and_then(|v| {
            ["error_description", "message", "error", "title"]
                .iter()
                .find_map(|k| v.get(*k).and_then(Value::as_str))
        })
        .map(str::to_owned)
        .unwrap_or_else(|| text.into_owned());
    let picked = picked.trim().trim_matches('"');
    picked.chars().take(ERROR_TEXT).collect()
}

impl Halo {
    pub fn new(credentials: Credentials) -> Self {
        Self::with_client(http_client(), credentials)
    }

    pub fn with_client(http: reqwest::Client, credentials: Credentials) -> Self {
        Self {
            http,
            credentials,
            token: Mutex::new(None),
        }
    }

    pub fn credentials(&self) -> &Credentials {
        &self.credentials
    }

    fn host(&self) -> String {
        host(&self.credentials.base_url)
    }

    /// `text` without the client secret, should Halo echo it.
    fn scrub(&self, text: String) -> String {
        let secret = self.credentials.client_secret.expose();
        if secret.is_empty() {
            text
        } else {
            text.replace(secret, REDACTED)
        }
    }

    /// A refusal with `status` and `body` in plain words.
    fn refusal(&self, what: &str, status: reqwest::StatusCode, body: &[u8]) -> HaloError {
        let host = self.host();
        let text = self.scrub(error_text(body));
        let message = if status.is_redirection() {
            format!("{host} answered {status} to {what}, a redirect, which is not followed: check the address.")
        } else if text.is_empty() {
            format!("{host} answered {status} to {what}.")
        } else {
            format!("{host} answered {status} to {what}: {text}")
        };
        HaloError {
            message,
            status: Some(status.as_u16()),
        }
    }

    /// A Bearer token, from the cache or a new one.
    pub async fn token(&self) -> Result<Secret, HaloError> {
        let mut cached = self.token.lock().await;
        if let Some(token) = cached.as_ref().filter(|t| Instant::now() < t.until) {
            return Ok(token.value.clone());
        }
        let host = self.host();
        let mut url = format!("{}/auth/token", self.credentials.base_url);
        if let Some(tenant) = &self.credentials.tenant {
            url = reqwest::Url::parse_with_params(&url, [("tenant", tenant)])
                .map(String::from)
                .unwrap_or(url);
        }
        let form = [
            ("grant_type", "client_credentials"),
            ("client_id", self.credentials.client_id.as_str()),
            ("client_secret", self.credentials.client_secret.expose()),
            ("scope", SCOPE),
        ];
        let response = self
            .http
            .post(url)
            .form(&form)
            .send()
            .await
            .map_err(|e| HaloError::new(request_error(&host, e).message))?;
        let status = response.status();
        let body = read(response, &host).await?;
        if !status.is_success() {
            let mut error = self.refusal("the sign-in", status, &body);
            if matches!(status.as_u16(), 400 | 401) {
                error.message.push_str(
                    " Check the Client ID and Secret, and that the application uses Client Credentials.",
                );
            }
            return Err(error);
        }
        #[derive(Deserialize)]
        struct Answer {
            access_token: String,
            expires_in: Option<u64>,
        }
        let answer: Answer = serde_json::from_slice(&body).map_err(|_| {
            HaloError::new(format!(
                "{host} answered the sign-in without a token: is this the address of a HaloPSA?"
            ))
        })?;
        let life = answer
            .expires_in
            .map_or(DEFAULT_TOKEN_LIFE, Duration::from_secs);
        let token = Secret::new(answer.access_token);
        *cached = Some(Token {
            value: token.clone(),
            until: Instant::now() + life.saturating_sub(TOKEN_MARGIN),
        });
        Ok(token)
    }

    /// Sends `request` to `{base}/api/{path}` with the token; the JSON
    /// answer. A 401 drops the cached token, so the next call signs in
    /// again.
    async fn send(
        &self,
        what: &str,
        request: impl FnOnce(&reqwest::Client, String) -> reqwest::RequestBuilder,
        path: &str,
    ) -> Result<Value, HaloError> {
        let token = self.token().await?;
        let host = self.host();
        let url = format!("{}/api/{path}", self.credentials.base_url);
        let response = request(&self.http, url)
            .bearer_auth(token.expose())
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|e| HaloError::new(request_error(&host, e).message))?;
        let status = response.status();
        let body = read(response, &host).await?;
        if !status.is_success() {
            if status.as_u16() == 401 {
                *self.token.lock().await = None;
            }
            return Err(self.refusal(what, status, &body));
        }
        if body.iter().all(u8::is_ascii_whitespace) {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&body).map_err(|_| {
            HaloError::new(format!(
                "{host} answered {what} with something that is not JSON."
            ))
        })
    }

    /// `GET {base}/api/{path}` with `query`.
    pub async fn get(
        &self,
        what: &str,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<Value, HaloError> {
        self.send(what, |http, url| http.get(url).query(query), path)
            .await
    }

    /// `POST {base}/api/{path}` with a JSON `body`.
    pub async fn post(&self, what: &str, path: &str, body: &Value) -> Result<Value, HaloError> {
        self.send(what, |http, url| http.post(url).json(body), path)
            .await
    }

    /// The agent the application acts as: what "Test connection" shows.
    pub async fn me(&self) -> Result<Agent, HaloError> {
        let answer = self.get("the agent request", "Agent/me", &[]).await?;
        serde_json::from_value(answer).map_err(|_| {
            HaloError::new(format!(
                "{} did not describe the application's agent: is this the address of a HaloPSA?",
                self.host()
            ))
        })
    }
}

/// A client or site of Halo, as the matching shows it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[schema(as = PsaRecord)]
pub struct Record {
    /// Halo's id.
    pub id: i64,
    /// Halo's name for it.
    pub name: String,
}

/// Clients listed per search.
pub const PAGE_SIZE: usize = 50;

/// The records of a list answer: Halo answers either an array or an
/// object holding the array under `key` (`clients`, `sites`...).
pub fn records(answer: &Value, key: &str) -> Vec<Value> {
    match answer {
        Value::Array(items) => items.clone(),
        Value::Object(map) => map
            .get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// `value`'s `id` and `name`, when it has them.
fn record(value: &Value) -> Option<Record> {
    Some(Record {
        id: value.get("id")?.as_i64()?,
        name: value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned(),
    })
}

impl Halo {
    /// Active clients whose name holds `search`, by name, at most
    /// [`PAGE_SIZE`].
    pub async fn clients(&self, search: &str) -> Result<Vec<Record>, HaloError> {
        let mut query = vec![
            ("pageinate", "true".to_owned()),
            ("page_size", PAGE_SIZE.to_string()),
            ("page_no", "1".to_owned()),
            ("order", "name".to_owned()),
            ("includeinactive", "false".to_owned()),
        ];
        if !search.trim().is_empty() {
            query.push(("search", search.trim().to_owned()));
        }
        let answer = self.get("the client list", "Client", &query).await?;
        Ok(records(&answer, "clients")
            .iter()
            .filter_map(record)
            .take(PAGE_SIZE)
            .collect())
    }

    /// The client `id`; `None` when Halo does not know it.
    pub async fn client(&self, id: i64) -> Result<Option<Record>, HaloError> {
        match self.get("the client", &format!("Client/{id}"), &[]).await {
            Ok(answer) => Ok(record(&answer)),
            Err(error) if error.status == Some(404) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The sites of client `client`, by name.
    pub async fn sites(&self, client: i64) -> Result<Vec<Record>, HaloError> {
        let query = [
            ("client_id", client.to_string()),
            ("includeinactive", "false".to_owned()),
        ];
        let answer = self.get("the site list", "Site", &query).await?;
        let mut sites: Vec<Record> = records(&answer, "sites")
            .iter()
            .filter(|s| {
                s.get("client_id")
                    .and_then(Value::as_i64)
                    .is_none_or(|c| c == client)
            })
            .filter_map(record)
            .collect();
        sites.sort_by_key(|s| s.name.to_lowercase());
        Ok(sites)
    }

    /// Ticket types for tickets (not opportunities or projects), by name.
    pub async fn ticket_types(&self) -> Result<Vec<Record>, HaloError> {
        let answer = self.get("the ticket type list", "TicketType", &[]).await?;
        let mut types: Vec<Record> = records(&answer, "tickettypes")
            .iter()
            .filter(|t| {
                t.get("use")
                    .and_then(Value::as_str)
                    .is_none_or(|u| u.eq_ignore_ascii_case("tickets"))
            })
            .filter_map(record)
            .collect();
        types.sort_by_key(|t| t.name.to_lowercase());
        Ok(types)
    }

    /// The first-level categories, as `category_1` takes them: their
    /// `value`, like `Hardware>Server`.
    pub async fn categories(&self) -> Result<Vec<String>, HaloError> {
        let answer = self
            .get(
                "the category list",
                "Category",
                &[("type_id", "1".to_owned())],
            )
            .await?;
        let mut values: Vec<String> = records(&answer, "categories")
            .iter()
            .filter(|c| {
                c.get("type_id")
                    .and_then(Value::as_i64)
                    .is_none_or(|t| t == 1)
            })
            .filter_map(|c| {
                ["value", "category_name", "name"]
                    .iter()
                    .find_map(|k| c.get(*k).and_then(Value::as_str))
                    .map(|v| v.trim().to_owned())
            })
            .filter(|v| !v.is_empty())
            .collect();
        values.sort_by_key(|v| v.to_lowercase());
        values.dedup();
        Ok(values)
    }

    /// Ticket statuses, by id: 1 is New and 9 Closed on every instance.
    pub async fn statuses(&self) -> Result<Vec<Record>, HaloError> {
        let answer = self
            .get(
                "the status list",
                "Status",
                &[("type", "ticket".to_owned())],
            )
            .await?;
        let mut statuses: Vec<Record> = records(&answer, "statuses")
            .iter()
            .filter_map(record)
            .collect();
        statuses.sort_by_key(|s| s.id);
        Ok(statuses)
    }

    /// The site `id` and its client; `None` when Halo does not know it.
    pub async fn site(&self, id: i64) -> Result<Option<(Record, Option<i64>)>, HaloError> {
        match self.get("the site", &format!("Site/{id}"), &[]).await {
            Ok(answer) => {
                Ok(record(&answer)
                    .map(|site| (site, answer.get("client_id").and_then(Value::as_i64))))
            }
            Err(error) if error.status == Some(404) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_urls_lose_api_and_slashes() {
        let ok = |s: &str| normalize_base_url(s).unwrap();
        assert_eq!(
            ok(" https://halo.example.com/ "),
            "https://halo.example.com"
        );
        assert_eq!(
            ok("https://halo.example.com/api"),
            "https://halo.example.com"
        );
        assert_eq!(
            ok("https://halo.example.com/api/"),
            "https://halo.example.com"
        );
        assert_eq!(
            ok("https://halo.example.com/auth"),
            "https://halo.example.com"
        );
        assert_eq!(ok("http://10.0.0.5:8080/halo"), "http://10.0.0.5:8080/halo");
        for bad in [
            "",
            "halo.example.com",
            "ftp://halo.example.com",
            "https://user:pw@halo.example.com",
            "https://halo.example.com/?tenant=x",
        ] {
            assert!(normalize_base_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn error_text_prefers_halo_s_own_words() {
        assert_eq!(
            error_text(br#"{"error":"invalid_client","error_description":"Bad secret"}"#),
            "Bad secret"
        );
        assert_eq!(error_text(br#"{"message":"No access"}"#), "No access");
        assert_eq!(
            error_text("\"Veuillez choisir une catégorie\"".as_bytes()),
            "Veuillez choisir une catégorie"
        );
        assert_eq!(error_text(&[b'x'; 1000]).len(), ERROR_TEXT);
    }

    #[test]
    fn lists_are_arrays_or_objects_holding_one() {
        let array = serde_json::json!([{"id": 1, "name": " A "}]);
        let object = serde_json::json!({"record_count": 1, "clients": [{"id": 1, "name": "A"}]});
        assert_eq!(records(&array, "clients").len(), 1);
        assert_eq!(records(&object, "clients").len(), 1);
        assert!(records(&object, "sites").is_empty());
        assert_eq!(
            record(&array[0]),
            Some(Record {
                id: 1,
                name: "A".into()
            })
        );
        assert_eq!(record(&serde_json::json!({"name": "no id"})), None);
    }

    #[test]
    fn only_missing_answers_rate_limits_and_server_errors_are_retried() {
        let error = |status| HaloError {
            message: String::new(),
            status,
        };
        assert!(error(None).retryable());
        assert!(error(Some(429)).retryable());
        assert!(error(Some(503)).retryable());
        assert!(!error(Some(400)).retryable());
        assert!(!error(Some(403)).retryable());
    }

    #[test]
    fn the_secret_never_shows() {
        let halo = Halo::new(Credentials {
            base_url: "https://halo.example.com".into(),
            tenant: None,
            client_id: "id".into(),
            client_secret: Secret::new("s3cr3t-value"),
        });
        assert!(!format!("{halo:?}").contains("s3cr3t"));
        let error = halo.refusal(
            "the sign-in",
            reqwest::StatusCode::BAD_REQUEST,
            b"unknown secret s3cr3t-value",
        );
        assert!(!error.message.contains("s3cr3t"), "{}", error.message);
        assert!(error.message.contains(REDACTED));
    }
}
