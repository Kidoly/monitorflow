//! Silences (ADR-0079): no notifications about a server, relay, site,
//! customer or everything for a while. The dispatcher applies them; nothing
//! here touches incidents.
//!
//! Silencing a server, relay or site needs an operator role on the site, a
//! customer a role of operator or above on the customer as a whole, and
//! everything a global administrator: what the user could change anyway.
//! The same goes for ending one. A user sees what covers its sites, so
//! never another customer's silences.

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgExecutor;
use uuid::Uuid;

use super::access::{authorize, Scope};
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::sensors::{body, parse_id};
use super::{ApiError, ApiState};

/// Longest silence: every silence ends on its own, soon enough that
/// nobody forgets one.
pub const MAX_DAYS: i64 = 30;
/// Longest reason, in characters.
pub const MAX_REASON: usize = 200;

/// What a silence covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    Server,
    Relay,
    Site,
    Customer,
    All,
}

/// Query string of `GET /api/silences`: at most one, for what covers it.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListParams {
    pub server_id: Option<Uuid>,
    pub relay_id: Option<Uuid>,
    pub site_id: Option<Uuid>,
}

/// Body of `POST /api/silences`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRequest {
    pub scope: Target,
    /// The server, relay, site or customer; absent for `all`.
    #[serde(default)]
    pub target_id: Option<Uuid>,
    /// Now when absent or past.
    #[serde(default)]
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: DateTime<Utc>,
    pub reason: String,
}

/// One silence.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct SilenceView {
    pub id: Uuid,
    /// `server`, `relay`, `site`, `customer` or `all`.
    pub scope: String,
    /// `null` for `all`.
    pub target_id: Option<Uuid>,
    pub target_name: Option<String>,
    /// The target's site; `null` for `customer` and `all`.
    pub site_id: Option<Uuid>,
    /// The target's customer; `null` for `all`.
    pub customer_id: Option<Uuid>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    /// Started, as opposed to upcoming.
    pub active: bool,
    pub reason: String,
    /// `null` once the user is deleted.
    pub created_by_username: Option<String>,
    pub created_at: DateTime<Utc>,
    /// Whether this user may end it.
    #[sqlx(default)]
    pub can_end: bool,
}

/// Body of `GET /api/silences`.
#[derive(Debug, Serialize)]
pub struct SilenceList {
    pub silences: Vec<SilenceView>,
}

const SELECT: &str = "SELECT x.id,
        CASE WHEN x.server_id IS NOT NULL THEN 'server'
             WHEN x.relay_id IS NOT NULL THEN 'relay'
             WHEN x.site_id IS NOT NULL THEN 'site'
             WHEN x.customer_id IS NOT NULL THEN 'customer'
             ELSE 'all' END AS scope,
        COALESCE(x.server_id, x.relay_id, x.site_id, x.customer_id) AS target_id,
        COALESCE(s.hostname, r.name, si.name, c.name) AS target_name,
        si.id AS site_id, c.id AS customer_id, x.starts_at, x.ends_at,
        x.starts_at <= now() AS active, x.reason,
        u.username AS created_by_username, x.created_at
    FROM silences x
    LEFT JOIN servers s ON s.id = x.server_id
    LEFT JOIN relays r ON r.id = x.relay_id
    LEFT JOIN sites si ON si.id = COALESCE(x.site_id, s.site_id, r.site_id)
    LEFT JOIN customers c ON c.id = COALESCE(x.customer_id, si.customer_id)
    LEFT JOIN users u ON u.id = x.created_by";

/// The silences a user sees: on its sites, on a customer it has a site or
/// a grant of, and everything, which covers its sites too. Binds `$1` to
/// `$3`.
const VISIBLE: &str = "($1 OR si.id = ANY($2)
        OR (si.id IS NULL AND (c.id IS NULL OR c.id = ANY($3)
            OR c.id IN (SELECT customer_id FROM sites WHERE id = ANY($2)))))";

/// `(all, sites, customers)` to bind to [`VISIBLE`].
fn visibility(user: &CurrentUser) -> (bool, Vec<Uuid>, Vec<Uuid>) {
    let (all, sites) = user.readable_sites().bind();
    (all, sites, user.visible_customers().bind().1)
}

fn may_end(user: &CurrentUser, silence: &SilenceView) -> bool {
    match (silence.site_id, silence.customer_id) {
        (Some(site), _) => user.modifiable_sites().contains(site),
        (None, Some(customer)) => user.modifiable_customers().contains(customer),
        (None, None) => user.administered_customers() == Scope::All,
    }
}

/// The site and customer of a server, relay or site.
async fn place(
    db: impl PgExecutor<'_>,
    target: Target,
    id: Uuid,
) -> Result<Option<(Uuid, Uuid)>, sqlx::Error> {
    let query = match target {
        Target::Server => {
            "SELECT si.id, si.customer_id FROM servers s
             JOIN sites si ON si.id = s.site_id WHERE s.id = $1"
        }
        Target::Relay => {
            "SELECT si.id, si.customer_id FROM relays r
             JOIN sites si ON si.id = r.site_id WHERE r.id = $1"
        }
        _ => "SELECT id, customer_id FROM sites WHERE id = $1",
    };
    sqlx::query_as(query).bind(id).fetch_optional(db).await
}

/// `GET /api/silences`: active and upcoming, first to start first.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
    params: Result<Query<ListParams>, QueryRejection>,
) -> Result<Json<SilenceList>, ApiError> {
    let Query(params) = params.map_err(|r| ApiError::Invalid(r.body_text()))?;
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let wanted = match (params.server_id, params.relay_id, params.site_id) {
        (None, None, None) => None,
        (Some(id), None, None) => Some((Target::Server, id)),
        (None, Some(id), None) => Some((Target::Relay, id)),
        (None, None, Some(id)) => Some((Target::Site, id)),
        _ => {
            return Err(ApiError::Invalid(
                "give at most one of server_id, relay_id and site_id".into(),
            ))
        }
    };
    let (mut site, mut customer) = (None, None);
    if let Some((target, id)) = wanted {
        let found = place(&state.pool, target, id).await?;
        authorize(&user, found.map(|p| p.0), false)?;
        site = found.map(|p| p.0);
        customer = found.map(|p| p.1);
    }
    let (all, sites, customers) = visibility(&user);
    // SQL-safe: interpolates only the constants SELECT and VISIBLE.
    let mut silences: Vec<SilenceView> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SELECT}
         WHERE coalesce(x.ended_at, x.ends_at) > now() AND {VISIBLE}
           AND (NOT $4
                OR num_nonnulls(x.server_id, x.relay_id, x.site_id, x.customer_id) = 0
                OR x.server_id = $5 OR x.relay_id = $6
                OR x.site_id = $7 OR x.customer_id = $8)
         ORDER BY x.starts_at, x.ends_at, x.id"
    )))
    .bind(all)
    .bind(&sites)
    .bind(&customers)
    .bind(wanted.is_some())
    .bind(params.server_id)
    .bind(params.relay_id)
    .bind(site)
    .bind(customer)
    .fetch_all(&state.pool)
    .await?;
    for silence in &mut silences {
        silence.can_end = may_end(&user, silence);
    }
    Ok(Json(SilenceList { silences }))
}

/// Refuses what the user may not silence, like [`authorize`]: 404 for what
/// it cannot see, 403 for what it may only read.
async fn authorize_target(
    state: &ApiState,
    user: &CurrentUser,
    target: Target,
    id: Option<Uuid>,
) -> Result<(), ApiError> {
    match (target, id) {
        (Target::All, None) => {
            if user.administered_customers() == Scope::All {
                Ok(())
            } else {
                Err(ApiError::Forbidden)
            }
        }
        (Target::All, Some(_)) => Err(ApiError::Invalid(
            "Everything takes no target_id.".to_owned(),
        )),
        (_, None) => Err(ApiError::Invalid("Say what to silence.".to_owned())),
        (Target::Customer, Some(id)) => {
            if !user.can_read() {
                return Err(ApiError::Forbidden);
            }
            let (all, sites, customers) = visibility(user);
            let seen: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM customers c WHERE c.id = $1
                    AND ($2 OR c.id = ANY($4)
                         OR c.id IN (SELECT customer_id FROM sites WHERE id = ANY($3))))",
            )
            .bind(id)
            .bind(all)
            .bind(&sites)
            .bind(&customers)
            .fetch_one(&state.pool)
            .await?;
            if !seen {
                Err(ApiError::NotFound)
            } else if user.modifiable_customers().contains(id) {
                Ok(())
            } else {
                Err(ApiError::Forbidden)
            }
        }
        (target, Some(id)) => {
            let found = place(&state.pool, target, id).await?;
            authorize(user, found.map(|p| p.0), true).map(|_| ())
        }
    }
}

/// The reason trimmed and the period, or why not; the error is a sentence
/// for the form.
fn validate(
    request: &CreateRequest,
    now: DateTime<Utc>,
) -> Result<(String, DateTime<Utc>), String> {
    let reason = request.reason.trim();
    if reason.is_empty() {
        return Err("Say why, so others know what this silence is for.".into());
    }
    if reason.chars().count() > MAX_REASON {
        return Err(format!("Keep the reason under {MAX_REASON} characters."));
    }
    let starts_at = request.starts_at.filter(|s| *s > now).unwrap_or(now);
    if request.ends_at <= starts_at {
        return Err("The end must be after the start, and in the future.".into());
    }
    if request.ends_at - starts_at > TimeDelta::days(MAX_DAYS) {
        return Err(format!(
            "A silence lasts at most {MAX_DAYS} days. To stop watching a machine for good, turn its offline alerts off or remove it."
        ));
    }
    Ok((reason.to_owned(), starts_at))
}

/// `POST /api/silences`.
pub async fn create(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<CreateRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<SilenceView>), ApiError> {
    let request = body(request)?;
    authorize_target(&state, &user, request.scope, request.target_id).await?;
    let (reason, starts_at) = validate(&request, Utc::now()).map_err(ApiError::Invalid)?;
    let column = |target| request.target_id.filter(|_| request.scope == target);
    let mut tx = state.pool.begin().await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO silences (server_id, relay_id, site_id, customer_id, starts_at, ends_at,
                               reason, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(column(Target::Server))
    .bind(column(Target::Relay))
    .bind(column(Target::Site))
    .bind(column(Target::Customer))
    .bind(starts_at)
    .bind(request.ends_at)
    .bind(&reason)
    .bind(user.id)
    .fetch_one(&mut *tx)
    .await?;
    let actor = user.id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: "silence.create",
            target: Some(("silence", &id.to_string())),
            details: json!({
                "scope": request.scope,
                "target_id": request.target_id,
                "starts_at": starts_at,
                "ends_at": request.ends_at,
                "reason": reason,
            }),
        },
    )
    .await?;
    // SQL-safe: interpolates only the constant SELECT.
    let mut view: SilenceView =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} WHERE x.id = $1")))
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    tx.commit().await?;
    view.can_end = may_end(&user, &view);
    Ok((StatusCode::CREATED, Json(view)))
}

/// `POST /api/silences/{id}/end`: over now, or never, if not started.
/// What it held goes out at the dispatcher's next look.
pub async fn end(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let (all, sites, customers) = visibility(&user);
    // SQL-safe: interpolates only the constants SELECT and VISIBLE.
    let silence: SilenceView = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SELECT} WHERE {VISIBLE} AND x.id = $4"
    )))
    .bind(all)
    .bind(&sites)
    .bind(&customers)
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(ApiError::NotFound)?;
    if !may_end(&user, &silence) {
        return Err(ApiError::Forbidden);
    }
    let mut tx = state.pool.begin().await?;
    let done = sqlx::query(
        "UPDATE silences SET ended_at = now(), ended_by = $2
         WHERE id = $1 AND ended_at IS NULL AND ends_at > now()",
    )
    .bind(id)
    .bind(user.id)
    .execute(&mut *tx)
    .await?;
    if done.rows_affected() == 0 {
        return Err(ApiError::Conflict("This silence is already over.".into()));
    }
    let actor = user.id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: "silence.end",
            target: Some(("silence", &id.to_string())),
            details: json!({
                "scope": silence.scope,
                "target_id": silence.target_id,
                "planned_end": silence.ends_at,
                "started": silence.active,
            }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(minute: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + minute * 60, 0).unwrap()
    }

    fn request(starts_at: Option<i64>, ends_at: i64, reason: &str) -> CreateRequest {
        CreateRequest {
            scope: Target::All,
            target_id: None,
            starts_at: starts_at.map(at),
            ends_at: at(ends_at),
            reason: reason.into(),
        }
    }

    #[test]
    fn the_period_starts_now_at_the_earliest_and_ends_on_its_own() {
        assert_eq!(
            validate(&request(None, 60, " patching "), at(0)),
            Ok(("patching".to_owned(), at(0)))
        );
        // A start already past is now; a future one stays.
        assert_eq!(
            validate(&request(Some(-5), 60, "x"), at(0)).unwrap().1,
            at(0)
        );
        assert_eq!(
            validate(&request(Some(5), 60, "x"), at(0)).unwrap().1,
            at(5)
        );
        let days = MAX_DAYS * 24 * 60;
        assert!(validate(&request(None, days, "x"), at(0)).is_ok());
        assert!(validate(&request(Some(10), days + 10, "x"), at(0)).is_ok());
        for bad in [
            request(None, days + 1, "x"),
            request(None, 0, "x"),
            request(None, -1, "x"),
            request(Some(30), 30, "x"),
            request(None, 60, "  "),
            request(None, 60, &"x".repeat(MAX_REASON + 1)),
        ] {
            assert!(validate(&bad, at(0)).is_err(), "{bad:?}");
        }
    }
}
