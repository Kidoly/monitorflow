//! Integration endpoints (ADR-0102): where signed events go for machines
//! such as a PSA. Each covers everything, one customer or one site, and is
//! managed by the administrators of what it covers, exactly like a
//! notification channel (ADR-0078): others do not exist for them. The
//! signing secret is made here and returned once, at creation, when it
//! is renewed and when the URL changes. Private addresses need a global
//! administrator.

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgExecutor;
use uuid::Uuid;

use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::notifications::{is_global_admin, manages, require_administrator};
use super::roles::Role;
use super::sensors::{body, parse_id};
use super::{ApiError, ApiState};
use crate::integrations::{
    self, deliver, normalize_events, normalize_url, Clients, Endpoint, MAX_NAME, ON, PAUSED,
};
use crate::notify::Coverage;

/// Deliveries per page of the log.
const PAGE_SIZE: i64 = 50;

/// An endpoint as the web interface shows it; never its secret.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct EndpointView {
    pub id: Uuid,
    pub name: String,
    pub url: String,
    pub events: Vec<String>,
    /// The customer it covers, or the customer of the site it covers;
    /// `null` for everything.
    pub customer_id: Option<Uuid>,
    pub customer_name: Option<String>,
    pub site_id: Option<Uuid>,
    pub site_name: Option<String>,
    pub allow_private: bool,
    /// `on`, `paused` or `disabled` (after a day of failures).
    pub state: String,
    pub created_at: DateTime<Utc>,
    /// The first failure since the last success; `null` while it works.
    pub failing_since: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    /// Deliveries waiting to be sent or retried.
    pub pending: i64,
    /// The most recent failure, in plain words.
    pub last_error: Option<String>,
}

/// Body of `GET /api/integrations`.
#[derive(Debug, Serialize)]
pub struct EndpointList {
    pub endpoints: Vec<EndpointView>,
    /// Every event an endpoint can ask for, in order.
    pub events: Vec<&'static str>,
}

/// Body of `POST /api/integrations` and of `PUT` on one.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointRequest {
    pub name: String,
    pub url: String,
    /// Absent, `null` or empty: every event.
    #[serde(default)]
    pub events: Option<Vec<String>>,
    /// The customer to cover; with `site_id` also `null`, everything.
    #[serde(default)]
    pub customer_id: Option<Uuid>,
    /// The site to cover. Not together with `customer_id`.
    #[serde(default)]
    pub site_id: Option<Uuid>,
    /// Deliver to private addresses; global administrators only.
    #[serde(default)]
    pub allow_private: bool,
}

/// Body of `POST /api/integrations` and of
/// `POST /api/integrations/{id}/secret`: the secret, this once.
#[derive(Debug, Serialize)]
pub struct WithSecret {
    pub endpoint: EndpointView,
    pub secret: String,
}

/// Body of `PUT /api/integrations/{id}`: the secret only when the URL
/// changed, as a new one, this once.
#[derive(Debug, Serialize)]
pub struct Updated {
    pub endpoint: EndpointView,
    pub secret: Option<String>,
}

/// Body of `POST /api/integrations/{id}/state`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateRequest {
    /// `on` or `paused`.
    pub state: String,
}

/// Body of `POST /api/integrations/{id}/test`.
#[derive(Debug, Serialize)]
pub struct TestResult {
    pub ok: bool,
    /// The answer's HTTP status, when there was one.
    pub status: Option<u16>,
    /// Why it failed, in plain words.
    pub error: Option<String>,
}

/// Body of `POST /api/integrations/{id}/replay`: one event, or every event
/// of a time range.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayRequest {
    #[serde(default)]
    pub event_id: Option<Uuid>,
    #[serde(default)]
    pub from: Option<DateTime<Utc>>,
    #[serde(default)]
    pub to: Option<DateTime<Utc>>,
}

/// Body of `POST /api/integrations/{id}/replay`.
#[derive(Debug, Serialize)]
pub struct ReplayResult {
    /// Deliveries queued.
    pub queued: u64,
}

/// Query string of `GET /api/integrations/{id}/deliveries`.
#[derive(Debug, Default, Deserialize)]
pub struct DeliveryParams {
    /// Only deliveries older than this one, for the next page.
    pub before: Option<i64>,
}

/// One delivery of the log.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct DeliveryView {
    pub id: i64,
    pub event_id: Uuid,
    pub event: String,
    pub occurred_at: DateTime<Utc>,
    /// `pending`, `delivered` or `failed`.
    pub state: String,
    pub replay: bool,
    pub attempts: i32,
    pub last_status: Option<i32>,
    pub last_error: Option<String>,
    pub last_attempt_at: Option<DateTime<Utc>>,
    /// When the next try is due, while pending.
    pub next_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// Body of `GET /api/integrations/{id}/deliveries`.
#[derive(Debug, Serialize)]
pub struct DeliveryList {
    pub deliveries: Vec<DeliveryView>,
    /// Pass as `before` for older ones; `null` at the end.
    pub next_before: Option<i64>,
}

const VIEW: &str = "SELECT n.id, n.name, n.url, n.events,
        coalesce(n.customer_id, s.customer_id) AS customer_id, c.name AS customer_name,
        n.site_id, s.name AS site_name, n.allow_private, n.state, n.created_at,
        n.failing_since, n.last_success_at,
        (SELECT count(*) FROM integration_deliveries d
         WHERE d.endpoint_id = n.id AND d.state = 'pending') AS pending,
        (SELECT d.last_error FROM integration_deliveries d
         WHERE d.endpoint_id = n.id AND d.last_error IS NOT NULL
         ORDER BY d.last_attempt_at DESC LIMIT 1) AS last_error
    FROM integration_endpoints n
    LEFT JOIN sites s ON s.id = n.site_id
    LEFT JOIN customers c ON c.id = coalesce(n.customer_id, s.customer_id)";

async fn view(db: impl PgExecutor<'_>, id: Uuid) -> Result<EndpointView, ApiError> {
    // SQL-safe: interpolates only the constant VIEW.
    sqlx::query_as(sqlx::AssertSqlSafe(format!("{VIEW} WHERE n.id = $1")))
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(ApiError::NotFound)
}

/// A valid request.
struct Valid {
    name: String,
    url: String,
    events: Vec<String>,
    coverage: Coverage,
    allow_private: bool,
}

/// Checks a request `user` may make; `current` is the endpoint it edits.
/// Whether `user` manages what it asks to cover comes first, without a
/// lookup, as for channels. Only a global administrator allows private
/// addresses; anyone else keeps the current choice only for the same URL.
async fn validate(
    db: impl PgExecutor<'_>,
    user: &CurrentUser,
    request: EndpointRequest,
    current: Option<&Endpoint>,
) -> Result<Valid, ApiError> {
    let coverage = Coverage::from_columns(request.customer_id, request.site_id)
        .ok_or_else(|| ApiError::Invalid("choose a customer or a site, not both".into()))?;
    if !manages(user, coverage) {
        return Err(ApiError::Forbidden);
    }
    let name = request.name.trim().to_owned();
    if name.is_empty() || name.chars().count() > MAX_NAME {
        return Err(ApiError::Invalid(format!(
            "Enter a name of at most {MAX_NAME} characters, like the tool it sends to."
        )));
    }
    let url = normalize_url(&request.url).map_err(ApiError::Invalid)?;
    let events = normalize_events(request.events).map_err(ApiError::Invalid)?;
    let allow_private = if is_global_admin(user) {
        request.allow_private
    } else {
        let kept = current.is_some_and(|c| c.allow_private && c.url == url);
        if request.allow_private && !kept {
            return Err(ApiError::Forbidden);
        }
        kept && request.allow_private
    };
    let unknown = match coverage {
        Coverage::Everything => None,
        Coverage::Customer(id) => Some((
            sqlx::query_scalar("SELECT id FROM customers WHERE id = $1").bind(id),
            "unknown customer",
        )),
        Coverage::Site(id) => Some((
            sqlx::query_scalar("SELECT id FROM sites WHERE id = $1").bind(id),
            "unknown site",
        )),
    };
    if let Some((query, error)) = unknown {
        let exists: Option<Uuid> = query.fetch_optional(db).await?;
        exists.ok_or_else(|| ApiError::Invalid(error.into()))?;
    }
    Ok(Valid {
        name,
        url,
        events,
        coverage,
        allow_private,
    })
}

/// What the audit keeps of an endpoint: the host, not the URL, whose path
/// may hold a token, and never the secret.
fn audited(valid: &Valid) -> Value {
    let (customer_id, site_id) = valid.coverage.columns();
    json!({
        "name": valid.name,
        "host": integrations::host(&valid.url),
        "events": valid.events,
        "customer_id": customer_id,
        "site_id": site_id,
        "allow_private": valid.allow_private,
    })
}

async fn record(
    db: impl PgExecutor<'_>,
    user: &CurrentUser,
    action: &str,
    id: Uuid,
    details: Value,
) -> Result<(), ApiError> {
    let actor = user.id.to_string();
    let target = id.to_string();
    audit::record(
        db,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action,
            target: Some(("integration_endpoint", &target)),
            details,
        },
    )
    .await?;
    Ok(())
}

/// The endpoint `id` if `user` manages it, locked in `db`'s transaction.
async fn managed(
    db: impl PgExecutor<'_>,
    user: &CurrentUser,
    id: &str,
) -> Result<Endpoint, ApiError> {
    let id = parse_id(id)?;
    integrations::lock(db, id)
        .await?
        .filter(|e| manages(user, e.coverage))
        .ok_or(ApiError::NotFound)
}

/// `GET /api/integrations`: the endpoints `user` manages.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<EndpointList>, ApiError> {
    require_administrator(&user)?;
    let (all, customers) = user.administered_customers().bind();
    let (_, sites) = user.sites_with(Role::Administrator).bind();
    // SQL-safe: interpolates only the constant VIEW.
    let endpoints = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{VIEW} WHERE $1 OR n.customer_id = ANY($2) OR n.site_id = ANY($3)
         ORDER BY n.created_at, n.id"
    )))
    .bind(all)
    .bind(&customers)
    .bind(&sites)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(EndpointList {
        endpoints,
        events: integrations::EVENTS.to_vec(),
    }))
}

/// `POST /api/integrations`: administrators of what it covers; 201 with
/// the secret, this once; audited as `integration.create`.
pub async fn create(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<EndpointRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<WithSecret>), ApiError> {
    require_administrator(&user)?;
    let request = body(request)?;
    let valid = validate(&state.pool, &user, request, None).await?;
    let secret = integrations::generate_secret();
    let (customer_id, site_id) = valid.coverage.columns();
    let mut tx = state.pool.begin().await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO integration_endpoints (name, url, secret, events, customer_id, site_id,
                                            allow_private)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(&valid.name)
    .bind(&valid.url)
    .bind(integrations::sealed(&secret)?)
    .bind(&valid.events)
    .bind(customer_id)
    .bind(site_id)
    .bind(valid.allow_private)
    .fetch_one(&mut *tx)
    .await?;
    record(&mut *tx, &user, "integration.create", id, audited(&valid)).await?;
    let endpoint = view(&mut *tx, id).await?;
    tx.commit().await?;
    tracing::info!(user = %user.id, endpoint = %id, "added an integration endpoint");
    Ok((
        StatusCode::CREATED,
        Json(WithSecret {
            endpoint,
            secret: secret.expose().to_owned(),
        }),
    ))
}

/// `PUT /api/integrations/{id}`: administrators of what it covers now and
/// of what it will cover; replaces the name, URL, events, coverage and the
/// private address choice; audited as `integration.update`.
///
/// The signing secret stays only with the URL it was made for, or a
/// customer administrator could point an endpoint an MSP set up at their
/// own server and collect events signed with the MSP's secret, replays
/// included. A new URL gets a new secret, returned this once, whoever
/// saves it, as a notification channel's typed secret must be typed
/// again (ADR-0078).
pub async fn update(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<EndpointRequest>, JsonRejection>,
) -> Result<Json<Updated>, ApiError> {
    require_administrator(&user)?;
    let request = body(request)?;
    let mut tx = state.pool.begin().await?;
    let current = managed(&mut *tx, &user, &id).await?;
    let valid = validate(&mut *tx, &user, request, Some(&current)).await?;
    let secret = (valid.url != current.url).then(integrations::generate_secret);
    let (customer_id, site_id) = valid.coverage.columns();
    sqlx::query(
        "UPDATE integration_endpoints SET name = $2, url = $3, events = $4, customer_id = $5,
             site_id = $6, allow_private = $7, secret = coalesce($8, secret)
         WHERE id = $1",
    )
    .bind(current.id)
    .bind(&valid.name)
    .bind(&valid.url)
    .bind(&valid.events)
    .bind(customer_id)
    .bind(site_id)
    .bind(valid.allow_private)
    .bind(secret.as_ref().map(|s| s.expose()))
    .execute(&mut *tx)
    .await?;
    let mut details = audited(&valid);
    details["secret_renewed"] = secret.is_some().into();
    record(&mut *tx, &user, "integration.update", current.id, details).await?;
    let endpoint = view(&mut *tx, current.id).await?;
    tx.commit().await?;
    if secret.is_some() {
        tracing::info!(user = %user.id, endpoint = %current.id, "moved an integration endpoint to a new URL with a new secret");
    }
    Ok(Json(Updated {
        endpoint,
        secret: secret.map(|s| s.expose().to_owned()),
    }))
}

/// `DELETE /api/integrations/{id}`: administrators of what it covers; 204;
/// its pending deliveries go with it; audited as `integration.delete`.
pub async fn delete(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_administrator(&user)?;
    let mut tx = state.pool.begin().await?;
    let endpoint = managed(&mut *tx, &user, &id).await?;
    sqlx::query("DELETE FROM integration_endpoints WHERE id = $1")
        .bind(endpoint.id)
        .execute(&mut *tx)
        .await?;
    record(
        &mut *tx,
        &user,
        "integration.delete",
        endpoint.id,
        json!({ "name": endpoint.name, "host": integrations::host(&endpoint.url) }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/integrations/{id}/state`: `on` resumes a paused or disabled
/// endpoint, `paused` stops new events; audited as `integration.state`.
pub async fn set_state(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<StateRequest>, JsonRejection>,
) -> Result<Json<EndpointView>, ApiError> {
    require_administrator(&user)?;
    let request = body(request)?;
    if ![ON, PAUSED].contains(&request.state.as_str()) {
        return Err(ApiError::Invalid("state is `on` or `paused`".into()));
    }
    let mut tx = state.pool.begin().await?;
    let endpoint = managed(&mut *tx, &user, &id).await?;
    // Turning it on starts the day of failures again.
    sqlx::query(
        "UPDATE integration_endpoints SET state = $2,
             failing_since = CASE WHEN $2 = 'on' THEN NULL ELSE failing_since END
         WHERE id = $1",
    )
    .bind(endpoint.id)
    .bind(&request.state)
    .execute(&mut *tx)
    .await?;
    record(
        &mut *tx,
        &user,
        "integration.state",
        endpoint.id,
        json!({ "from": endpoint.state, "to": request.state }),
    )
    .await?;
    let view = view(&mut *tx, endpoint.id).await?;
    tx.commit().await?;
    Ok(Json(view))
}

/// `POST /api/integrations/{id}/secret`: a new signing secret, returned
/// this once; the old one stops working at once; audited as
/// `integration.secret_renew`.
pub async fn renew_secret(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<WithSecret>, ApiError> {
    require_administrator(&user)?;
    let mut tx = state.pool.begin().await?;
    let endpoint = managed(&mut *tx, &user, &id).await?;
    let secret = integrations::generate_secret();
    sqlx::query("UPDATE integration_endpoints SET secret = $2 WHERE id = $1")
        .bind(endpoint.id)
        .bind(integrations::sealed(&secret)?)
        .execute(&mut *tx)
        .await?;
    record(
        &mut *tx,
        &user,
        "integration.secret_renew",
        endpoint.id,
        json!({}),
    )
    .await?;
    let view = view(&mut *tx, endpoint.id).await?;
    tx.commit().await?;
    Ok(Json(WithSecret {
        endpoint: view,
        secret: secret.expose().to_owned(),
    }))
}

/// `POST /api/integrations/{id}/test`: sends a `test` event now, signed,
/// and answers how it went. Not stored: it neither enters the delivery log
/// nor counts as a failure.
pub async fn test(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<TestResult>, ApiError> {
    require_administrator(&user)?;
    let id = parse_id(&id)?;
    let endpoint = integrations::load(&state.pool, id)
        .await?
        .filter(|e| manages(&user, e.coverage))
        .ok_or(ApiError::NotFound)?;
    let web_url = state.settings.web_url.trim_end_matches('/');
    let outcome = Clients::default()
        .post(&endpoint, &integrations::test_event(), web_url, false)
        .await;
    tracing::info!(user = %user.id, endpoint = %id, ok = outcome.is_ok(), "tested an integration endpoint");
    Ok(Json(match outcome {
        Ok(status) => TestResult {
            ok: true,
            status: Some(status),
            error: None,
        },
        Err(failure) => TestResult {
            ok: false,
            status: failure.status,
            error: Some(failure.message),
        },
    }))
}

/// `GET /api/integrations/{id}/deliveries`: the newest deliveries first,
/// [`PAGE_SIZE`] at a time.
pub async fn deliveries(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    params: Result<Query<DeliveryParams>, QueryRejection>,
) -> Result<Json<DeliveryList>, ApiError> {
    require_administrator(&user)?;
    let Query(params) = params.map_err(|e| ApiError::Invalid(e.body_text()))?;
    let id = parse_id(&id)?;
    integrations::load(&state.pool, id)
        .await?
        .filter(|e| manages(&user, e.coverage))
        .ok_or(ApiError::NotFound)?;
    let mut deliveries: Vec<DeliveryView> = sqlx::query_as(
        "SELECT d.id, v.id AS event_id, v.event, v.created_at AS occurred_at, d.state, d.replay,
                d.attempts, d.last_status, d.last_error, d.last_attempt_at,
                CASE WHEN d.state = 'pending' THEN d.next_at END AS next_at, d.finished_at
         FROM integration_deliveries d JOIN integration_events v ON v.seq = d.event_seq
         WHERE d.endpoint_id = $1 AND ($2::bigint IS NULL OR d.id < $2)
         ORDER BY d.id DESC LIMIT $3",
    )
    .bind(id)
    .bind(params.before)
    .bind(PAGE_SIZE + 1)
    .fetch_all(&state.pool)
    .await?;
    let next_before = if deliveries.len() > PAGE_SIZE as usize {
        deliveries.truncate(PAGE_SIZE as usize);
        deliveries.last().map(|d| d.id)
    } else {
        None
    };
    Ok(Json(DeliveryList {
        deliveries,
        next_before,
    }))
}

/// Which events a replay asks for: one, or a range within what is kept.
#[derive(Debug, PartialEq)]
enum Replay {
    One(Uuid),
    Range(DateTime<Utc>, DateTime<Utc>),
}

fn replay_of(request: ReplayRequest) -> Result<Replay, ApiError> {
    match request {
        ReplayRequest {
            event_id: Some(id),
            from: None,
            to: None,
        } => Ok(Replay::One(id)),
        ReplayRequest {
            event_id: None,
            from: Some(from),
            to,
        } => {
            let to = to.unwrap_or_else(Utc::now);
            if from >= to {
                return Err(ApiError::Invalid("`from` must be before `to`".into()));
            }
            if to - from > TimeDelta::days(i64::from(deliver::KEEP_DAYS)) {
                return Err(ApiError::Invalid(format!(
                    "Events are kept {} days: choose a shorter range.",
                    deliver::KEEP_DAYS
                )));
            }
            Ok(Replay::Range(from, to))
        }
        _ => Err(ApiError::Invalid(
            "give either `event_id`, or `from` and optionally `to`".into(),
        )),
    }
}

/// `POST /api/integrations/{id}/replay`: sends one event, or every event
/// of a time range that the endpoint wants and covers, again, with the
/// same IDs; audited as `integration.replay`.
pub async fn replay(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<ReplayRequest>, JsonRejection>,
) -> Result<Json<ReplayResult>, ApiError> {
    require_administrator(&user)?;
    let asked = replay_of(body(request)?)?;
    let mut tx = state.pool.begin().await?;
    let endpoint = managed(&mut *tx, &user, &id).await?;
    let (queued, details) = match asked {
        Replay::One(event) => {
            let found = integrations::replay_event(&mut *tx, endpoint.id, event).await?;
            if !found {
                return Err(ApiError::NotFound);
            }
            (1, json!({ "event_id": event }))
        }
        Replay::Range(from, to) => {
            let queued = integrations::replay_range(&mut *tx, endpoint.id, from, to).await?;
            (queued, json!({ "from": from, "to": to, "queued": queued }))
        }
    };
    record(&mut *tx, &user, "integration.replay", endpoint.id, details).await?;
    tx.commit().await?;
    Ok(Json(ReplayResult { queued }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_audit_keeps_the_host_only() {
        let valid = Valid {
            name: "HaloPSA".into(),
            url: "https://psa.example/hooks/T0KEN".into(),
            events: vec!["incident.opened".into()],
            coverage: Coverage::Site(Uuid::nil()),
            allow_private: false,
        };
        let details = audited(&valid);
        assert_eq!(details["host"], "psa.example");
        assert!(!details.to_string().contains("T0KEN"));
        assert_eq!(details["site_id"], Uuid::nil().to_string());
    }

    #[test]
    fn a_replay_is_one_event_or_a_kept_range() {
        let id = Uuid::new_v4();
        let one = ReplayRequest {
            event_id: Some(id),
            from: None,
            to: None,
        };
        assert_eq!(replay_of(one).unwrap(), Replay::One(id));
        let now = Utc::now();
        let range = |from: DateTime<Utc>, to| ReplayRequest {
            event_id: None,
            from: Some(from),
            to: Some(to),
        };
        assert!(matches!(
            replay_of(range(now - TimeDelta::hours(1), now)),
            Ok(Replay::Range(..))
        ));
        assert!(replay_of(range(now, now)).is_err());
        assert!(replay_of(range(now - TimeDelta::days(31), now)).is_err());
        let both = ReplayRequest {
            event_id: Some(id),
            from: Some(now),
            to: None,
        };
        assert!(replay_of(both).is_err());
    }
}
