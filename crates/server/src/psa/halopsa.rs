//! The HaloPSA API (ADR-0164, ADR-0166): OAuth2 client credentials on
//! `{base}/auth/token` with the scope `all`, then the API under
//! `{base}/api` with the Bearer token.
//!
//! Requests follow no redirects and have a timeout; answers are read up to
//! [`MAX_ANSWER`]. Every request goes through a [`Limiter`] that keeps
//! the process under HaloPSA's documented limit of 700 requests in a
//! rolling 5 minutes, and a 429 pauses them all for its `Retry-After`
//! (ADR-0191). The token, the secret and ticket text never appear in
//! errors or logs: an error is Halo's own text, cut short, with the secret
//! taken out in case Halo echoes it.

use std::collections::VecDeque;
use std::sync::{Arc, LazyLock};
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

/// HaloPSA's limit, from its public API documentation: 700 requests in a
/// rolling 5 minutes.
pub const HALO_LIMIT: usize = 700;
/// The rolling window of [`HALO_LIMIT`].
pub const WINDOW: Duration = Duration::from_secs(300);
/// What this process allows itself within [`WINDOW`]: the rest is left to
/// the tenant's other integrations and to other central server instances'
/// pages.
pub const BUDGET: usize = 600;
/// The pause after a 429 without a usable `Retry-After`.
const DEFAULT_PAUSE: Duration = Duration::from_secs(60);
/// The longest pause a `Retry-After` may ask for.
const MAX_PAUSE: Duration = Duration::from_secs(300);

/// Requests sent in the last [`WINDOW`], and a pause after a 429.
#[derive(Debug)]
pub struct Limiter {
    budget: usize,
    state: std::sync::Mutex<LimiterState>,
}

#[derive(Debug, Default)]
struct LimiterState {
    sent: VecDeque<Instant>,
    paused_until: Option<Instant>,
}

/// The limiter every request of this process goes through.
static LIMITER: LazyLock<Arc<Limiter>> = LazyLock::new(|| Arc::new(Limiter::new(BUDGET)));

impl Limiter {
    pub fn new(budget: usize) -> Self {
        Self {
            budget,
            state: std::sync::Mutex::new(LimiterState::default()),
        }
    }

    /// The limiter of this process.
    pub fn shared() -> Arc<Self> {
        LIMITER.clone()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, LimiterState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Counts one request at `now`, or how long to wait before one may be
    /// sent: until the pause ends, or until the oldest request leaves the
    /// window.
    pub fn take(&self, now: Instant) -> Result<(), Duration> {
        let mut state = self.state();
        if let Some(until) = state.paused_until {
            if now < until {
                return Err(until - now);
            }
            state.paused_until = None;
        }
        while state
            .sent
            .front()
            .is_some_and(|t| now.duration_since(*t) >= WINDOW)
        {
            state.sent.pop_front();
        }
        if state.sent.len() >= self.budget {
            let oldest = state.sent.front().copied().unwrap_or(now);
            return Err(WINDOW.saturating_sub(now.duration_since(oldest)));
        }
        state.sent.push_back(now);
        Ok(())
    }

    /// Stops every request for `pause` from `now`, after a 429.
    pub fn pause(&self, now: Instant, pause: Duration) {
        let mut state = self.state();
        let until = now + pause.min(MAX_PAUSE);
        state.paused_until = Some(state.paused_until.map_or(until, |u| u.max(until)));
    }

    /// Requests that may still be sent at `now` without waiting.
    pub fn remaining(&self, now: Instant) -> usize {
        let state = self.state();
        if state.paused_until.is_some_and(|u| now < u) {
            return 0;
        }
        let recent = state
            .sent
            .iter()
            .filter(|t| now.duration_since(**t) < WINDOW)
            .count();
        self.budget.saturating_sub(recent)
    }
}

/// The pause a 429's `Retry-After` asks for, in seconds; HTTP dates are
/// not used by HaloPSA's documentation and fall back to the default.
fn retry_after(headers: &reqwest::header::HeaderMap) -> Duration {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map_or(DEFAULT_PAUSE, Duration::from_secs)
}

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
    limiter: Arc<Limiter>,
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
            limiter: Limiter::shared(),
        }
    }

    /// This client with its own limiter, for tests.
    pub fn with_limiter(mut self, limiter: Arc<Limiter>) -> Self {
        self.limiter = limiter;
        self
    }

    /// Counts one request, or says how long until one may be sent: a
    /// retryable error, so loops try again later and pages say so.
    fn admit(&self) -> Result<(), HaloError> {
        self.limiter.take(Instant::now()).map_err(|wait| {
            HaloError::new(format!(
                "MonitorFlow keeps under HaloPSA's limit of {HALO_LIMIT} requests in 5 minutes: try again in {} s.",
                wait.as_secs().max(1)
            ))
        })
    }

    /// After a 429, pauses every request for what Halo asks.
    fn rate_limited(&self, status: reqwest::StatusCode, headers: &reqwest::header::HeaderMap) {
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let pause = retry_after(headers);
            tracing::warn!(
                seconds = pause.as_secs(),
                "HaloPSA answered 429; pausing its requests"
            );
            self.limiter.pause(Instant::now(), pause);
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
        self.admit()?;
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
        self.rate_limited(status, response.headers());
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
        self.admit()?;
        let host = self.host();
        let url = format!("{}/api/{path}", self.credentials.base_url);
        let response = request(&self.http, url)
            .bearer_auth(token.expose())
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|e| HaloError::new(request_error(&host, e).message))?;
        let status = response.status();
        self.rate_limited(status, response.headers());
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

/// A ticket to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTicket {
    pub summary: String,
    pub details: String,
    pub client_id: i64,
    pub site_id: Option<i64>,
    pub ticket_type_id: Option<i64>,
    pub category: Option<String>,
    /// The priority within the ticket type's SLA; `None` for Halo's
    /// default.
    pub priority_id: Option<i64>,
    /// The server's asset, attached when Halo accepts it.
    pub asset_id: Option<i64>,
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

/// Clients asked of Halo per page while searching.
const SCAN_PAGE: usize = 100;
/// Pages read at most for one search: 3,000 clients.
const SCAN_PAGES: usize = 30;

/// `text` lowercased and without accents, for a forgiving match:
/// "Écoles" holds "ecol".
fn folded(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
            'ç' => 'c',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ñ' => 'n',
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' => 'o',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'ý' | 'ÿ' => 'y',
            other => other,
        })
        .collect()
}

impl Halo {
    /// Active clients whose name holds `search`, by name, at most
    /// [`PAGE_SIZE`].
    ///
    /// Halo is asked to search, but some instances ignore it and answer
    /// every client: the names are matched here too, reading the pages
    /// until enough clients match.
    pub async fn clients(&self, search: &str) -> Result<Vec<Record>, HaloError> {
        let search = search.trim();
        let needle = folded(search);
        let mut found = Vec::new();
        for page in 1..=SCAN_PAGES {
            let mut query = vec![
                ("pageinate", "true".to_owned()),
                ("page_size", SCAN_PAGE.to_string()),
                ("page_no", page.to_string()),
                ("order", "name".to_owned()),
                ("includeinactive", "false".to_owned()),
            ];
            if !search.is_empty() {
                query.push(("search", search.to_owned()));
            }
            let answer = self.get("the client list", "Client", &query).await?;
            let listed = records(&answer, "clients");
            found.extend(
                listed
                    .iter()
                    .filter_map(record)
                    .filter(|c| folded(&c.name).contains(&needle)),
            );
            if found.len() >= PAGE_SIZE || listed.len() < SCAN_PAGE {
                break;
            }
        }
        found.truncate(PAGE_SIZE);
        Ok(found)
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

    /// Creates `ticket` by posting it in an array, as Halo takes them; its
    /// id.
    pub async fn create_ticket(&self, ticket: &NewTicket) -> Result<i64, HaloError> {
        let mut body = serde_json::json!({
            "summary": ticket.summary,
            "details": ticket.details,
            "client_id": ticket.client_id,
        });
        if let Some(site) = ticket.site_id {
            body["site_id"] = site.into();
        }
        if let Some(kind) = ticket.ticket_type_id {
            body["tickettype_id"] = kind.into();
        }
        if let Some(category) = &ticket.category {
            body["category_1"] = category.clone().into();
        }
        if let Some(priority) = ticket.priority_id {
            body["priority_id"] = priority.into();
        }
        let answer = match ticket.asset_id {
            None => {
                self.post("the new ticket", "Tickets", &Value::Array(vec![body]))
                    .await?
            }
            Some(asset) => {
                // How a ticket names its asset is not in Halo's public
                // documentation: an instance that refuses it gets the
                // ticket without (ADR-0191).
                let mut with_asset = body.clone();
                with_asset["assets"] = serde_json::json!([{ "id": asset }]);
                match self
                    .post("the new ticket", "Tickets", &Value::Array(vec![with_asset]))
                    .await
                {
                    Err(error)
                        if error
                            .status
                            .is_some_and(|s| (400..500).contains(&s) && s != 429) =>
                    {
                        self.post("the new ticket", "Tickets", &Value::Array(vec![body]))
                            .await?
                    }
                    other => other?,
                }
            }
        };
        let created = match &answer {
            Value::Array(items) => items.first().cloned().unwrap_or(Value::Null),
            other => other.clone(),
        };
        created.get("id").and_then(Value::as_i64).ok_or_else(|| {
            HaloError::new(format!(
                "{} created the ticket without saying its number.",
                self.host()
            ))
        })
    }

    /// A ticket of `client` whose text holds `marker`, the incident's id:
    /// what a retry after a lost answer finds instead of creating a second
    /// ticket.
    pub async fn find_ticket(&self, client: i64, marker: &str) -> Result<Option<i64>, HaloError> {
        let query = [
            ("client_id", client.to_string()),
            ("search", marker.to_owned()),
            ("pageinate", "true".to_owned()),
            ("page_size", "5".to_owned()),
            ("page_no", "1".to_owned()),
        ];
        let answer = self.get("the ticket search", "Tickets", &query).await?;
        Ok(records(&answer, "tickets")
            .first()
            .and_then(|t| t.get("id").and_then(Value::as_i64)))
    }

    /// Adds a private note to `ticket`, hidden from the end user.
    pub async fn add_note(&self, ticket: i64, note: &str) -> Result<(), HaloError> {
        let action = serde_json::json!([{
            "ticket_id": ticket,
            "outcome": "MonitorFlow",
            "note": note,
            "hiddenfromuser": true,
        }]);
        self.post("the note", "Actions", &action).await.map(drop)
    }

    /// Moves `ticket` to the status `status`.
    pub async fn set_status(&self, ticket: i64, status: i64) -> Result<(), HaloError> {
        let update = serde_json::json!([{ "id": ticket, "status_id": status }]);
        self.post("the status change", "Tickets", &update)
            .await
            .map(drop)
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

/// A ticket as the poller reads it back (ADR-0191).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketState {
    pub status_id: Option<i64>,
    /// The assigned agent; 1 or less is nobody.
    pub agent_id: Option<i64>,
    pub deleted: bool,
    /// The ticket it was merged into; `None` when not merged.
    pub merged_into: Option<i64>,
}

impl TicketState {
    fn from_answer(answer: &Value) -> Self {
        let int = |key: &str| answer.get(key).and_then(Value::as_i64);
        Self {
            status_id: int("status_id"),
            agent_id: int("agent_id"),
            deleted: answer
                .get("deleted")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            merged_into: int("merged_into_id").filter(|id| *id > 0),
        }
    }
}

/// An asset of a client, as the matching reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteAsset {
    pub id: i64,
    pub inventory_number: Option<String>,
    /// The type's key field, what Halo shows as the asset's name.
    pub key_field: Option<String>,
    /// The asset type's fields: id and value.
    pub fields: Vec<(i64, String)>,
}

impl RemoteAsset {
    fn from_answer(value: &Value) -> Option<Self> {
        let text = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        let fields = value
            .get("fields")
            .and_then(Value::as_array)
            .map(|fields| {
                fields
                    .iter()
                    .filter_map(|f| {
                        let id = f.get("id")?.as_i64()?;
                        let value = match f.get("value")? {
                            Value::String(s) => s.trim().to_owned(),
                            Value::Number(n) => n.to_string(),
                            _ => return None,
                        };
                        Some((id, value))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(Self {
            id: value.get("id")?.as_i64()?,
            inventory_number: text("inventory_number"),
            key_field: text("key_field"),
            fields,
        })
    }

    /// The value of `target`: `inventory_number` or a field id.
    pub fn value(&self, target: &str) -> Option<&str> {
        if target == INVENTORY_NUMBER {
            return self.inventory_number.as_deref();
        }
        let id: i64 = target.parse().ok()?;
        self.fields
            .iter()
            .find(|(f, _)| *f == id)
            .map(|(_, v)| v.as_str())
    }
}

/// The asset field that is HaloPSA's asset tag rather than a field of the
/// type.
pub const INVENTORY_NUMBER: &str = "inventory_number";

/// An asset to create or update. `fields` maps a target, `inventory_number`
/// or a field id, to its value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetBody {
    /// `None` creates the asset.
    pub id: Option<i64>,
    pub type_id: i64,
    pub client_id: i64,
    pub site_id: Option<i64>,
    pub fields: Vec<(String, String)>,
    /// Marks it inactive, for a removed server.
    pub inactive: bool,
    /// The External Link back to the server: its id, host name and page.
    pub link: Option<(String, String, String)>,
}

impl AssetBody {
    /// The JSON Halo takes, inside the array it wants.
    pub fn json(&self) -> Value {
        let mut body = serde_json::json!({
            "assettype_id": self.type_id,
            "client_id": self.client_id,
        });
        if let Some(id) = self.id {
            body["id"] = id.into();
        }
        if let Some(site) = self.site_id {
            body["site_id"] = site.into();
        }
        let mut fields = Vec::new();
        for (target, value) in &self.fields {
            if target == INVENTORY_NUMBER {
                body["inventory_number"] = value.clone().into();
            } else if let Ok(id) = target.parse::<i64>() {
                fields.push(serde_json::json!({ "id": id, "value": value }));
            }
        }
        if !fields.is_empty() {
            body["fields"] = fields.into();
        }
        if self.inactive {
            body["inactive"] = true.into();
        }
        if let Some((id, name, url)) = &self.link {
            body["new_external_link"] = serde_json::json!({
                "integration_name": "MonitorFlow",
                "third_party_id": id,
                "third_party_desc": name,
                "third_party_url": url,
            });
        }
        body
    }
}

/// Asset pages read at most per client and type: 2,000 assets.
const ASSET_PAGES: usize = 20;

impl Halo {
    /// The ticket `id` as it is now; `None` when Halo no longer has it.
    pub async fn ticket(&self, id: i64) -> Result<Option<TicketState>, HaloError> {
        match self.get("the ticket", &format!("Tickets/{id}"), &[]).await {
            Ok(answer) => Ok(Some(TicketState::from_answer(&answer))),
            Err(error) if error.status == Some(404) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The name of the agent `id`.
    pub async fn agent(&self, id: i64) -> Result<Option<String>, HaloError> {
        match self.get("the agent", &format!("Agent/{id}"), &[]).await {
            Ok(answer) => Ok(answer
                .get("name")
                .and_then(Value::as_str)
                .map(|n| n.trim().to_owned())
                .filter(|n| !n.is_empty())),
            Err(error) if error.status == Some(404) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Moves `ticket` to the priority `priority`.
    pub async fn set_priority(&self, ticket: i64, priority: i64) -> Result<(), HaloError> {
        let update = serde_json::json!([{ "id": ticket, "priority_id": priority }]);
        self.post("the priority change", "Tickets", &update)
            .await
            .map(drop)
    }

    /// The priorities a ticket of type `ticket_type` may take: those of the
    /// type's default SLA, by their number within it (`priorityid`), which
    /// is what a ticket's `priority_id` names. Without a type or an SLA,
    /// every priority number once.
    pub async fn priorities(&self, ticket_type: Option<i64>) -> Result<Vec<Record>, HaloError> {
        let sla = match ticket_type {
            Some(id) => self
                .get("the ticket type", &format!("TicketType/{id}"), &[])
                .await?
                .get("default_sla")
                .and_then(Value::as_i64)
                .filter(|s| *s > 0),
            None => None,
        };
        let answer = self.get("the priority list", "Priority", &[]).await?;
        let mut found: Vec<Record> = Vec::new();
        for p in records(&answer, "priorities") {
            let Some(number) = p.get("priorityid").and_then(Value::as_i64) else {
                continue;
            };
            let slaid = p.get("slaid").and_then(Value::as_i64);
            if sla.is_some() && slaid.is_some() && slaid != sla {
                continue;
            }
            if found.iter().any(|r| r.id == number) {
                continue;
            }
            let name = p
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_owned();
            found.push(Record { id: number, name });
        }
        found.sort_by_key(|r| r.id);
        Ok(found)
    }

    /// Every active client, by name, up to the scan's limit: what name
    /// suggestions are made from.
    pub async fn all_clients(&self) -> Result<Vec<Record>, HaloError> {
        let mut found = Vec::new();
        for page in 1..=SCAN_PAGES {
            let query = [
                ("pageinate", "true".to_owned()),
                ("page_size", SCAN_PAGE.to_string()),
                ("page_no", page.to_string()),
                ("order", "name".to_owned()),
                ("includeinactive", "false".to_owned()),
            ];
            let answer = self.get("the client list", "Client", &query).await?;
            let listed = records(&answer, "clients");
            found.extend(listed.iter().filter_map(record));
            if listed.len() < SCAN_PAGE {
                break;
            }
        }
        Ok(found)
    }

    /// Asset types, by name.
    pub async fn asset_types(&self) -> Result<Vec<Record>, HaloError> {
        let answer = self.get("the asset type list", "AssetType", &[]).await?;
        let mut types: Vec<Record> = records(&answer, "assettypes")
            .iter()
            .filter(|t| !t.get("inactive").and_then(Value::as_bool).unwrap_or(false))
            .filter_map(record)
            .collect();
        types.sort_by_key(|t| t.name.to_lowercase());
        Ok(types)
    }

    /// The fields of the asset type `id`, by name: from the type's details,
    /// or else from one of its assets, whose `fields` name them.
    pub async fn asset_fields(&self, id: i64) -> Result<Vec<Record>, HaloError> {
        let named = |list: &[Value]| -> Vec<Record> {
            list.iter()
                .filter_map(|f| {
                    Some(Record {
                        id: f.get("id")?.as_i64()?,
                        name: f.get("name").and_then(Value::as_str)?.trim().to_owned(),
                    })
                })
                .collect()
        };
        let details = self
            .get(
                "the asset type",
                &format!("AssetType/{id}"),
                &[("includedetails", "true".to_owned())],
            )
            .await?;
        let mut fields = named(&records(&details, "fields"));
        if fields.is_empty() {
            let query = [
                ("assettypes", id.to_string()),
                ("includeassetfields", "true".to_owned()),
                ("pageinate", "true".to_owned()),
                ("page_size", "1".to_owned()),
                ("page_no", "1".to_owned()),
            ];
            let answer = self.get("the asset list", "Asset", &query).await?;
            if let Some(asset) = records(&answer, "assets").first() {
                fields = named(&records(
                    asset.get("fields").unwrap_or(&Value::Null),
                    "fields",
                ));
            }
        }
        fields.sort_by_key(|f| f.name.to_lowercase());
        fields.dedup_by_key(|f| f.id);
        Ok(fields)
    }

    /// The active assets of `client` of the type `type_id`, with their
    /// fields.
    pub async fn assets(&self, client: i64, type_id: i64) -> Result<Vec<RemoteAsset>, HaloError> {
        let mut found = Vec::new();
        for page in 1..=ASSET_PAGES {
            let query = [
                ("client_id", client.to_string()),
                ("assettypes", type_id.to_string()),
                ("includeassetfields", "true".to_owned()),
                ("includeinactive", "false".to_owned()),
                ("pageinate", "true".to_owned()),
                ("page_size", SCAN_PAGE.to_string()),
                ("page_no", page.to_string()),
            ];
            let answer = self.get("the asset list", "Asset", &query).await?;
            let listed = records(&answer, "assets");
            found.extend(listed.iter().filter_map(RemoteAsset::from_answer));
            if listed.len() < SCAN_PAGE {
                break;
            }
        }
        Ok(found)
    }

    /// The asset `id` and its client; `None` when Halo does not know it.
    pub async fn asset(&self, id: i64) -> Result<Option<(RemoteAsset, Option<i64>)>, HaloError> {
        match self.get("the asset", &format!("Asset/{id}"), &[]).await {
            Ok(answer) => Ok(RemoteAsset::from_answer(&answer)
                .map(|asset| (asset, answer.get("client_id").and_then(Value::as_i64)))),
            Err(error) if error.status == Some(404) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Creates or updates an asset, posted in an array as Halo takes them;
    /// its id.
    pub async fn upsert_asset(&self, asset: &AssetBody) -> Result<i64, HaloError> {
        let answer = self
            .post("the asset", "Asset", &Value::Array(vec![asset.json()]))
            .await?;
        let saved = match &answer {
            Value::Array(items) => items.first().cloned().unwrap_or(Value::Null),
            other => other.clone(),
        };
        saved
            .get("id")
            .and_then(Value::as_i64)
            .or(asset.id)
            .ok_or_else(|| {
                HaloError::new(format!(
                    "{} saved the asset without saying its number.",
                    self.host()
                ))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_limiter_keeps_under_its_budget_and_pauses_after_a_429() {
        let limiter = Limiter::new(3);
        let start = Instant::now();
        for _ in 0..3 {
            assert_eq!(limiter.take(start), Ok(()));
        }
        assert_eq!(limiter.remaining(start), 0);
        let wait = limiter.take(start + Duration::from_secs(100)).unwrap_err();
        assert_eq!(wait, Duration::from_secs(200));
        // The oldest leaves the window: room again.
        assert_eq!(limiter.take(start + WINDOW), Ok(()));
        let later = start + WINDOW + Duration::from_secs(1);
        limiter.pause(later, Duration::from_secs(30));
        assert_eq!(limiter.remaining(later), 0);
        assert_eq!(limiter.take(later).unwrap_err(), Duration::from_secs(30));
        // A pause asks for at most five minutes.
        limiter.pause(later, Duration::from_secs(10_000));
        assert_eq!(limiter.take(later).unwrap_err(), MAX_PAUSE);
    }

    #[test]
    fn retry_after_is_read_in_seconds() {
        let mut headers = reqwest::header::HeaderMap::new();
        assert_eq!(retry_after(&headers), DEFAULT_PAUSE);
        headers.insert(reqwest::header::RETRY_AFTER, "12".parse().unwrap());
        assert_eq!(retry_after(&headers), Duration::from_secs(12));
        headers.insert(
            reqwest::header::RETRY_AFTER,
            "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(retry_after(&headers), DEFAULT_PAUSE);
    }

    #[test]
    fn tickets_read_back_say_status_agent_deletion_and_merge() {
        let state = TicketState::from_answer(&serde_json::json!({
            "id": 5001, "status_id": 9, "agent_id": 4, "deleted": false, "merged_into_id": 0,
        }));
        assert_eq!(
            state,
            TicketState {
                status_id: Some(9),
                agent_id: Some(4),
                deleted: false,
                merged_into: None
            }
        );
        let merged = TicketState::from_answer(&serde_json::json!({"merged_into_id": 77}));
        assert_eq!(merged.merged_into, Some(77));
    }

    #[test]
    fn assets_are_sent_with_their_fields_and_link() {
        let body = AssetBody {
            id: None,
            type_id: 3,
            client_id: 10,
            site_id: Some(100),
            fields: vec![
                (INVENTORY_NUMBER.into(), "web-01".into()),
                ("12".into(), "SN123".into()),
            ],
            inactive: false,
            link: Some(("srv".into(), "web-01".into(), "https://mf/servers".into())),
        };
        let json = body.json();
        assert_eq!(json["inventory_number"], "web-01");
        assert_eq!(
            json["fields"],
            serde_json::json!([{"id": 12, "value": "SN123"}])
        );
        assert_eq!(json["new_external_link"]["integration_name"], "MonitorFlow");
        assert!(json.get("id").is_none());
        let asset = RemoteAsset::from_answer(&serde_json::json!({
            "id": 7, "inventory_number": " web-01 ", "key_field": "web-01",
            "fields": [{"id": 12, "name": "Serial", "value": "SN123"}],
        }))
        .unwrap();
        assert_eq!(asset.value("12"), Some("SN123"));
        assert_eq!(asset.value(INVENTORY_NUMBER), Some("web-01"));
        assert_eq!(asset.value("13"), None);
    }

    #[test]
    fn names_match_without_case_or_accents() {
        assert!(folded("Écoles Réunies").contains(&folded("ecoles reu")));
        assert!(folded("Sample Corp").contains(&folded("CORP")));
        assert!(!folded("Sample Corp").contains(&folded("example")));
    }

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
