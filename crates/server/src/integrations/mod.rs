//! Outgoing events for integrations (ADR-0102): signed webhooks for
//! machines such as a PSA. Triggers write `integration_events` from the
//! same tables the notification triggers read, and one delivery per
//! endpoint that wants each event; the [`Deliverer`] sends them at least
//! once, in order per subject, retrying for a day. Unlike notification
//! channels (ADR-0060), nothing is grouped, silenced or dropped, and any
//! event of the last 30 days can be sent again.

pub mod deliver;

use chrono::{DateTime, Utc};
use monitorflow_common::redact::Secret;
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{json, Map, Value};
use sqlx::PgExecutor;
use uuid::Uuid;

use crate::notify::send::{
    error_body, http_client, public_http_client, refuse_private_address_in, request_error,
    response_error, signature, SendError, DELIVERY_HEADER, EVENT_HEADER, SIGNATURE_HEADER,
    TIMESTAMP_HEADER,
};
use crate::notify::Coverage;

pub use deliver::Deliverer;

/// The page of integrations, under the web interface's URL.
pub const PAGE: &str = "/admin/integrations/";
/// Version of the payload; fields are only ever added.
pub const PAYLOAD_VERSION: u32 = 1;
/// Says a delivery is a replay of an event already sent.
pub const REPLAY_HEADER: &str = "x-monitorflow-replay";
/// Longest endpoint name.
pub const MAX_NAME: usize = 80;
/// Every event an endpoint can ask for, in the order the interface lists
/// them; `integration_events.event` checks the same list.
pub const EVENTS: [&str; 16] = [
    "incident.opened",
    "incident.escalated",
    "incident.acknowledged",
    "incident.resolved",
    "server.added",
    "server.removed",
    "server.offline",
    "server.online",
    "update.started",
    "update.succeeded",
    "update.failed",
    "campaign.started",
    "campaign.succeeded",
    "campaign.failed",
    "approval.waiting",
    "approval.decided",
];
/// Starts every signing secret, so one found in a log or a repository is
/// recognisable.
const SECRET_PREFIX: &str = "mfwh_";
const SECRET_BYTES: usize = 32;

/// An endpoint gets events.
pub const ON: &str = "on";
/// An administrator paused it.
pub const PAUSED: &str = "paused";
/// Turned off after a day of failures.
pub const DISABLED: &str = "disabled";

/// An endpoint, with its secret, for delivery and for the API's checks.
#[derive(Debug, Clone)]
pub struct Endpoint {
    pub id: Uuid,
    pub name: String,
    pub url: String,
    pub secret: Secret,
    pub events: Vec<String>,
    pub coverage: Coverage,
    pub allow_private: bool,
    /// [`ON`], [`PAUSED`] or [`DISABLED`].
    pub state: String,
}

#[derive(sqlx::FromRow)]
struct EndpointRow {
    id: Uuid,
    name: String,
    url: String,
    secret: String,
    events: Vec<String>,
    customer_id: Option<Uuid>,
    site_id: Option<Uuid>,
    allow_private: bool,
    state: String,
}

impl From<EndpointRow> for Endpoint {
    fn from(row: EndpointRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
            url: row.url,
            secret: Secret::new(row.secret),
            events: row.events,
            // The table's check allows at most one of the two.
            coverage: Coverage::from_columns(row.customer_id, row.site_id)
                .unwrap_or(Coverage::Everything),
            allow_private: row.allow_private,
            state: row.state,
        }
    }
}

const SELECT: &str = "SELECT id, name, url, secret, events, customer_id, site_id, allow_private,
        state FROM integration_endpoints WHERE id = $1";

/// The endpoint `id`.
pub async fn load(db: impl PgExecutor<'_>, id: Uuid) -> Result<Option<Endpoint>, sqlx::Error> {
    let row: Option<EndpointRow> = sqlx::query_as(SELECT).bind(id).fetch_optional(db).await?;
    Ok(row.map(Endpoint::from))
}

/// The endpoint `id`, locked until the transaction ends.
pub async fn lock(db: impl PgExecutor<'_>, id: Uuid) -> Result<Option<Endpoint>, sqlx::Error> {
    // SQL-safe: interpolates only the constant SELECT.
    let row: Option<EndpointRow> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} FOR UPDATE")))
            .bind(id)
            .fetch_optional(db)
            .await?;
    Ok(row.map(Endpoint::from))
}

/// A new signing secret: the prefix and 256 random bits in hex.
pub fn generate_secret() -> Secret {
    let mut bytes = [0u8; SECRET_BYTES];
    SystemRandom::new()
        .fill(&mut bytes)
        .expect("the system random generator failed");
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Secret::new(format!("{SECRET_PREFIX}{hex}"))
}

/// The URL trimmed and checked; the error is a sentence for the form.
pub fn normalize_url(url: &str) -> Result<String, String> {
    let url = url.trim();
    let parsed = reqwest::Url::parse(url)
        .map_err(|_| format!("`{url}` is not a URL; it starts with https://."))?;
    if !matches!(parsed.scheme(), "https" | "http") || parsed.host_str().is_none() {
        return Err(format!("`{url}` is not an http or https URL."));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(
            "Put no password in the URL: requests are signed with the endpoint's secret.".into(),
        );
    }
    Ok(parsed.to_string())
}

/// The events asked for, known ones only, each once, in [`EVENTS`] order;
/// none asked for means all of them.
pub fn normalize_events(events: Option<Vec<String>>) -> Result<Vec<String>, String> {
    let Some(events) = events.filter(|e| !e.is_empty()) else {
        return Ok(EVENTS.iter().map(|&e| e.to_owned()).collect());
    };
    if let Some(unknown) = events.iter().find(|e| !EVENTS.contains(&e.as_str())) {
        return Err(format!("`{unknown}` is not an event MonitorFlow sends."));
    }
    Ok(EVENTS
        .iter()
        .filter(|&&e| events.iter().any(|asked| asked == e))
        .map(|&e| e.to_owned())
        .collect())
}

/// The host of `url`, for messages and the audit: paths often hold
/// tokens.
pub fn host(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default()
}

/// An event as stored.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct StoredEvent {
    pub id: Uuid,
    pub event: String,
    pub details: Value,
    pub created_at: DateTime<Utc>,
}

/// The page that shows what `event` is about.
fn path(event: &str, details: &Value) -> String {
    let id = |object: &str| {
        details[object]["id"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    };
    match event.split('.').next().unwrap_or_default() {
        "incident" => format!("/incidents/view/?id={}", id("incident")),
        "server" => format!("/servers/view/?id={}", id("server")),
        "campaign" => format!("/campaigns/view/?id={}", id("campaign")),
        "approval" => "/remediation/approvals/".into(),
        "update" if details["relay"].is_object() => {
            format!("/admin/relays/view/?id={}", id("relay"))
        }
        "update" => format!("/servers/view/?id={}&tab=updates", id("server")),
        _ => PAGE.into(),
    }
}

/// The JSON body of `event` (ADR-0102, `docs/events/v1.schema.json`).
pub fn payload(event: &StoredEvent, web_url: &str) -> Value {
    let mut payload = Map::new();
    payload.insert("version".into(), PAYLOAD_VERSION.into());
    payload.insert("id".into(), event.id.to_string().into());
    payload.insert("event".into(), event.event.clone().into());
    payload.insert(
        "url".into(),
        format!("{web_url}{}", path(&event.event, &event.details)).into(),
    );
    if let Some(details) = event.details.as_object() {
        for (key, value) in details {
            payload.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    payload
        .entry("occurred_at")
        .or_insert_with(|| json!(event.created_at));
    for place in ["customer", "site", "server", "relay"] {
        payload.entry(place).or_insert(Value::Null);
    }
    Value::Object(payload)
}

/// What "Send test" delivers: a `test` event with a new ID each time, so a
/// receiver that de-duplicates still sees every test.
pub fn test_event() -> StoredEvent {
    StoredEvent {
        id: Uuid::new_v4(),
        event: "test".into(),
        details: json!({}),
        created_at: Utc::now(),
    }
}

/// Why a delivery failed: the answer's status if there was one, and a
/// sentence for the delivery log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub status: Option<u16>,
    pub message: String,
}

impl From<SendError> for Failure {
    fn from(error: SendError) -> Self {
        Self {
            status: None,
            message: error.message,
        }
    }
}

/// The two HTTP clients: one for endpoints allowed private addresses, one
/// that connects only to public ones (ADR-0078's resolver).
#[derive(Clone)]
pub struct Clients {
    any: reqwest::Client,
    public: reqwest::Client,
}

impl Default for Clients {
    fn default() -> Self {
        Self {
            any: http_client(),
            public: public_http_client(),
        }
    }
}

impl Clients {
    /// Posts `event` to `endpoint`, signed; the answer's status when 2xx.
    /// No redirect is followed, and the URL never appears in the error.
    pub async fn post(
        &self,
        endpoint: &Endpoint,
        event: &StoredEvent,
        web_url: &str,
        replay: bool,
    ) -> Result<u16, Failure> {
        let http = if endpoint.allow_private {
            &self.any
        } else {
            refuse_private_address_in(&endpoint.url)?;
            &self.public
        };
        let body = serde_json::to_vec(&payload(event, web_url)).unwrap_or_default();
        let timestamp = Utc::now().timestamp();
        let mut request = http
            .post(&endpoint.url)
            .header("content-type", "application/json")
            .header(EVENT_HEADER, &event.event)
            .header(DELIVERY_HEADER, event.id.to_string())
            .header(TIMESTAMP_HEADER, timestamp.to_string())
            .header(
                SIGNATURE_HEADER,
                signature(endpoint.secret.expose(), timestamp, &body),
            );
        if replay {
            request = request.header(REPLAY_HEADER, "1");
        }
        let host = host(&endpoint.url);
        let response = request
            .body(body)
            .send()
            .await
            .map_err(|error| request_error(&host, error))?;
        let status = response.status();
        if status.is_success() {
            return Ok(status.as_u16());
        }
        let text = error_body(response).await;
        Err(Failure {
            status: Some(status.as_u16()),
            message: response_error(&host, status, &text).message,
        })
    }
}

/// The SQL condition, on endpoint `e` and event `v`, that the endpoint
/// wants the event and covers its place (ADR-0078), as the fan-out trigger
/// decides.
const WANTS: &str = "v.event = ANY (e.events)
    AND ((e.customer_id IS NULL AND e.site_id IS NULL)
         OR e.customer_id = v.customer_id OR e.site_id = v.site_id)";

/// Sends event `id` to `endpoint` again, if the endpoint wants and covers
/// it; whether it was queued.
pub async fn replay_event(
    db: impl PgExecutor<'_>,
    endpoint: Uuid,
    id: Uuid,
) -> Result<bool, sqlx::Error> {
    // SQL-safe: interpolates only the constant WANTS.
    let queued = sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO integration_deliveries (endpoint_id, event_seq, subject, replay)
         SELECT e.id, v.seq, v.subject, true
         FROM integration_endpoints e, integration_events v
         WHERE e.id = $1 AND v.id = $2 AND {WANTS}"
    )))
    .bind(endpoint)
    .bind(id)
    .execute(db)
    .await?;
    Ok(queued.rows_affected() > 0)
}

/// Sends every event between `from` and `to` that `endpoint` wants and
/// covers again, oldest first; how many were queued.
pub async fn replay_range(
    db: impl PgExecutor<'_>,
    endpoint: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<u64, sqlx::Error> {
    // SQL-safe: interpolates only the constant WANTS.
    let queued = sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO integration_deliveries (endpoint_id, event_seq, subject, replay)
         SELECT e.id, v.seq, v.subject, true
         FROM integration_endpoints e, integration_events v
         WHERE e.id = $1 AND v.created_at >= $2 AND v.created_at < $3 AND {WANTS}
         ORDER BY v.seq"
    )))
    .bind(endpoint)
    .bind(from)
    .bind(to)
    .execute(db)
    .await?;
    Ok(queued.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(name: &str, details: Value) -> StoredEvent {
        StoredEvent {
            id: Uuid::nil(),
            event: name.into(),
            details,
            created_at: DateTime::from_timestamp(1_800_000_000, 0).unwrap(),
        }
    }

    #[test]
    fn secrets_are_long_random_and_recognisable() {
        let a = generate_secret();
        let b = generate_secret();
        assert_ne!(a.expose(), b.expose());
        assert!(a.expose().starts_with("mfwh_"));
        assert_eq!(a.expose().len(), 5 + 64);
        assert!(!format!("{a:?}").contains(&a.expose()[5..]));
    }

    #[test]
    fn urls_are_http_without_credentials() {
        assert_eq!(
            normalize_url(" https://psa.example/hook ").unwrap(),
            "https://psa.example/hook"
        );
        assert!(normalize_url("ftp://psa.example/").is_err());
        assert!(normalize_url("psa.example").is_err());
        assert!(normalize_url("https://u:p@psa.example/").is_err());
    }

    #[test]
    fn events_are_known_ordered_and_default_to_all() {
        assert_eq!(normalize_events(None).unwrap().len(), EVENTS.len());
        assert_eq!(normalize_events(Some(vec![])).unwrap().len(), EVENTS.len());
        assert_eq!(
            normalize_events(Some(vec![
                "server.added".into(),
                "incident.opened".into(),
                "server.added".into()
            ]))
            .unwrap(),
            ["incident.opened", "server.added"]
        );
        let error = normalize_events(Some(vec!["backup.failed".into()])).unwrap_err();
        assert!(error.contains("backup.failed"), "{error}");
    }

    #[test]
    fn the_payload_carries_the_id_places_and_object() {
        let incident = Uuid::new_v4();
        let body = payload(
            &event(
                "incident.opened",
                json!({"customer": {"id": "c", "name": "acme"},
                       "incident": {"id": incident, "severity": "critical"}}),
            ),
            "https://mf.example",
        );
        assert_eq!(body["version"], 1);
        assert_eq!(body["id"], Uuid::nil().to_string());
        assert_eq!(body["event"], "incident.opened");
        assert_eq!(body["customer"]["name"], "acme");
        assert_eq!(body["site"], Value::Null);
        assert_eq!(body["relay"], Value::Null);
        assert_eq!(body["incident"]["severity"], "critical");
        assert_eq!(body["occurred_at"], "2027-01-15T08:00:00Z");
        assert_eq!(
            body["url"],
            format!("https://mf.example/incidents/view/?id={incident}")
        );
    }

    #[test]
    fn the_published_schema_lists_every_event() {
        let schema: Value =
            serde_json::from_str(include_str!("../../../../docs/events/v1.schema.json")).unwrap();
        let listed: Vec<&str> = schema["properties"]["event"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let mut expected = EVENTS.to_vec();
        expected.push("test");
        assert_eq!(listed, expected);
        assert_eq!(schema["properties"]["version"]["const"], PAYLOAD_VERSION);
    }

    #[test]
    fn links_open_the_page_of_the_subject() {
        let link = |name: &str, details: Value| path(name, &details);
        assert_eq!(
            link("server.added", json!({"server": {"id": "s"}})),
            "/servers/view/?id=s"
        );
        assert_eq!(
            link("campaign.failed", json!({"campaign": {"id": "k"}})),
            "/campaigns/view/?id=k"
        );
        assert_eq!(
            link("update.failed", json!({"relay": {"id": "r"}})),
            "/admin/relays/view/?id=r"
        );
        assert_eq!(
            link(
                "update.started",
                json!({"server": {"id": "s"}, "relay": null})
            ),
            "/servers/view/?id=s&tab=updates"
        );
        assert_eq!(
            link("approval.decided", json!({})),
            "/remediation/approvals/"
        );
        assert_eq!(link("test", json!({})), PAGE);
    }
}
