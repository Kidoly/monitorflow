//! API keys of the public API (ADR-0101): made by administrators, shown
//! once, stored as a SHA-256, and acting as their maker narrowed to the
//! key's reach, access and areas on every request under `/api/v1`.
//!
//! | Method | Path | Auth | Response |
//! | --- | --- | --- | --- |
//! | GET | `/api/api-keys` | administrator | [`KeyList`]: the keys the user could have made |
//! | POST | `/api/api-keys` | administrator of the reach | body [`CreateRequest`]; 201 [`Created`], the only time `secret` is shown |
//! | DELETE | `/api/api-keys/{id}` | administrator of the reach | 204; the key stops at its next request |

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use axum::extract::rejection::JsonRejection;
use axum::extract::{MatchedPath, Path, State};
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::Json;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use monitorflow_common::redact::REDACTED;
use ring::digest::{digest, SHA256};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgExecutor;
use uuid::Uuid;

use super::access::{authorize, Access, Scope};
use super::audit::{self, Event};
use super::auth::{self, CurrentUser};
use super::roles::{Role, RoleGrant};
use super::sensors::{body, parse_id};
use super::{v1, ApiError, ApiState};

/// What every key starts with, for secret scanners and people.
pub const PREFIX: &str = "mfk_";
const SECRET_BYTES: usize = 32;
/// Characters after [`PREFIX`] kept to tell keys apart.
const SHOWN: usize = 8;
const MAX_NAME: usize = 80;
/// `last_used_at` is written at most this often per key.
const LAST_USED_EVERY: Duration = Duration::from_secs(60);

/// Parts of MonitorFlow a key may be limited to, read from the route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Area {
    Servers,
    Alerts,
    Updates,
    Remediation,
    Dashboards,
    Administration,
}

impl Area {
    pub const ALL: [Area; 6] = [
        Self::Servers,
        Self::Alerts,
        Self::Updates,
        Self::Remediation,
        Self::Dashboards,
        Self::Administration,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Servers => "servers",
            Self::Alerts => "alerts",
            Self::Updates => "updates",
            Self::Remediation => "remediation",
            Self::Dashboards => "dashboards",
            Self::Administration => "administration",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.as_str() == s)
    }

    /// The area of the route `path` (a template such as
    /// `/api/servers/{id}/commands`), or `None` for the listings any key
    /// may read, which turn names into IDs, and for health.
    pub fn of(method: &Method, path: &str) -> Option<Self> {
        let segments: Vec<&str> = path
            .trim_start_matches("/api/")
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();
        let first = segments.first().copied().unwrap_or("");
        // What follows `/<first>/{id}/`, if anything.
        let sub = segments.get(2).copied();
        let read = method == Method::GET;
        Some(match (first, sub) {
            ("health", _) => return None,
            ("servers" | "relays", None) if read => return None,
            ("sites" | "customers", None) if read && segments.len() == 1 => return None,
            ("servers" | "relays" | "sites", Some(sub)) => match sub {
                "sensors" | "sensor_configs" | "thresholds" | "offline-alerts"
                | "check-interval" => Self::Alerts,
                "commands" | "packages" | "sources" | "rollback" | "update"
                | "agent-auto-update" => Self::Updates,
                "ai-commands" => Self::Remediation,
                _ if first == "sites" => Self::Administration,
                _ => Self::Servers,
            },
            ("servers" | "relays" | "metrics", _) => Self::Servers,
            (
                "incidents" | "silences" | "on-call" | "follow-up" | "sensor_configs"
                | "thresholds" | "notifications",
                _,
            ) => Self::Alerts,
            (
                "commands" | "campaigns" | "auto-update" | "agent-releases" | "server-update"
                | "mirrors",
                _,
            ) => Self::Updates,
            ("settings", _) if segments.get(1) == Some(&"auto-update") => Self::Updates,
            ("remediation", _) => Self::Remediation,
            ("dashboards" | "dashboard-data" | "dashboard-metrics" | "display-links", _) => {
                Self::Dashboards
            }
            _ => Self::Administration,
        })
    }
}

/// An API key's secret: [`PREFIX`] and 256 random bits in base64url.
/// `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct KeySecret(String);

impl fmt::Debug for KeySecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeySecret({REDACTED})")
    }
}

impl KeySecret {
    fn generate() -> Self {
        let mut bytes = [0u8; SECRET_BYTES];
        SystemRandom::new()
            .fill(&mut bytes)
            .expect("the system random generator failed");
        Self(format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes)))
    }

    /// Accepts only the exact format [`KeySecret::generate`] produces.
    pub fn parse(value: &str) -> Option<Self> {
        let rest = value.strip_prefix(PREFIX)?;
        let valid = URL_SAFE_NO_PAD
            .decode(rest)
            .is_ok_and(|b| b.len() == SECRET_BYTES);
        valid.then(|| Self(value.to_owned()))
    }

    pub fn hash(&self) -> Vec<u8> {
        digest(&SHA256, self.0.as_bytes()).as_ref().to_vec()
    }

    fn shown(&self) -> &str {
        &self.0[PREFIX.len()..PREFIX.len() + SHOWN]
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

/// The secret of `Authorization: Bearer <secret>`, if well-formed.
pub fn bearer(headers: &HeaderMap) -> Option<KeySecret> {
    let value = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, secret) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    KeySecret::parse(secret.trim())
}

/// What a key reaches: everything, one customer or one site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reach {
    All,
    Customer(Uuid),
    /// A site and its customer.
    Site(Uuid, Uuid),
}

/// The maker's `grants` that fall within `reach`, each role lowered to
/// `cap`. `customer_of` gives the customer of a site.
fn narrow(
    grants: &[RoleGrant],
    cap: Role,
    reach: Reach,
    customer_of: impl Fn(Uuid) -> Option<Uuid>,
) -> Vec<RoleGrant> {
    let grant = |role: Role, customer_id, site_id| RoleGrant {
        role: role.min(cap),
        customer_id,
        site_id,
    };
    grants
        .iter()
        .filter_map(|g| match (reach, g.customer_id, g.site_id) {
            (Reach::All, c, s) => Some(grant(g.role, c, s)),
            (Reach::Customer(c), None, None) => Some(grant(g.role, Some(c), None)),
            (Reach::Customer(c), Some(gc), _) => (gc == c).then(|| grant(g.role, Some(c), None)),
            (Reach::Customer(c), None, Some(s)) => {
                (customer_of(s) == Some(c)).then(|| grant(g.role, None, Some(s)))
            }
            (Reach::Site(s, _), None, None) => Some(grant(g.role, None, Some(s))),
            (Reach::Site(s, c), Some(gc), _) => (gc == c).then(|| grant(g.role, None, Some(s))),
            (Reach::Site(s, _), None, Some(gs)) => (gs == s).then(|| grant(g.role, None, Some(s))),
        })
        .collect()
}

#[derive(sqlx::FromRow)]
struct AuthRow {
    id: Uuid,
    role: String,
    customer_id: Option<Uuid>,
    site_id: Option<Uuid>,
    site_customer: Option<Uuid>,
    areas: Option<Vec<String>>,
    created_by: Uuid,
    expired: bool,
    disabled: bool,
    touch: bool,
}

/// The user behind the request's key, narrowed to it (ADR-0101): 401 for
/// a missing, unknown or expired key or a disabled maker, 429 over the
/// key's rate limit, 403 `api_key_scope` outside its areas.
pub(super) async fn authenticate(parts: &Parts, state: &ApiState) -> Result<CurrentUser, ApiError> {
    let secret = bearer(&parts.headers).ok_or(ApiError::Unauthorized)?;
    let row: AuthRow = sqlx::query_as(
        "SELECT k.id, k.role, k.customer_id, k.site_id, s.customer_id AS site_customer,
                k.areas, k.created_by,
                k.expires_at IS NOT NULL AND k.expires_at <= now() AS expired,
                u.disabled_at IS NOT NULL AS disabled,
                k.last_used_at IS NULL OR k.last_used_at <= now() - $2 AS touch
         FROM api_keys k
         JOIN users u ON u.id = k.created_by
         LEFT JOIN sites s ON s.id = k.site_id
         WHERE k.token_hash = $1",
    )
    .bind(secret.hash())
    .bind(LAST_USED_EVERY)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(ApiError::Unauthorized)?;
    if row.expired || row.disabled {
        return Err(ApiError::Unauthorized);
    }
    v1::admit(parts, state, row.id)?;
    if let Some(areas) = &row.areas {
        let path = parts
            .extensions
            .get::<MatchedPath>()
            .map_or(parts.uri.path(), MatchedPath::as_str);
        if let Some(area) = Area::of(&parts.method, path) {
            if !areas.iter().any(|a| a == area.as_str()) {
                return Err(ApiError::KeyScope);
            }
        }
    }
    if row.touch {
        sqlx::query("UPDATE api_keys SET last_used_at = now() WHERE id = $1")
            .bind(row.id)
            .execute(&state.pool)
            .await?;
    }

    let cap: Role = row
        .role
        .parse()
        .map_err(|e: String| ApiError::Internal(e))?;
    let reach = match (row.customer_id, row.site_id, row.site_customer) {
        (Some(c), _, _) => Reach::Customer(c),
        (None, Some(s), Some(c)) => Reach::Site(s, c),
        _ => Reach::All,
    };
    let maker = auth::load_user(&state.pool, row.created_by, None).await?;
    let sites: Vec<Uuid> = maker.roles.iter().filter_map(|g| g.site_id).collect();
    let customers: HashMap<Uuid, Uuid> =
        sqlx::query_as::<_, (Uuid, Uuid)>("SELECT id, customer_id FROM sites WHERE id = ANY($1)")
            .bind(&sites)
            .fetch_all(&state.pool)
            .await?
            .into_iter()
            .collect();
    let roles = narrow(&maker.roles, cap, reach, |s| customers.get(&s).copied());
    let access = Access::load(&state.pool, &roles).await?;
    Ok(maker.through_key(row.id, roles, access))
}

/// One key, without its secret.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct KeyView {
    pub id: Uuid,
    pub name: String,
    /// The first characters after `mfk_`.
    pub prefix: String,
    pub role: String,
    /// Reach: both null for everything.
    pub customer_id: Option<Uuid>,
    pub customer_name: Option<String>,
    pub site_id: Option<Uuid>,
    pub site_name: Option<String>,
    /// `null` for every area.
    pub areas: Option<Vec<String>>,
    pub created_by: Uuid,
    pub created_by_username: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
}

const SELECT: &str = "SELECT k.id, k.name, k.prefix, k.role, k.customer_id,
        coalesce(c.name, sc.name) AS customer_name, k.site_id, s.name AS site_name,
        k.areas, k.created_by, u.username AS created_by_username, k.created_at,
        k.expires_at, k.last_used_at
    FROM api_keys k
    JOIN users u ON u.id = k.created_by
    LEFT JOIN customers c ON c.id = k.customer_id
    LEFT JOIN sites s ON s.id = k.site_id
    LEFT JOIN customers sc ON sc.id = s.customer_id";

#[derive(Debug, Serialize)]
pub struct KeyList {
    pub keys: Vec<KeyView>,
}

/// `GET /api/api-keys`: the keys the user could have made, newest first.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<KeyList>, ApiError> {
    if !user.is_administrator() {
        return Err(ApiError::Forbidden);
    }
    let (all, customers) = user.administered_customers().bind();
    let (_, sites) = user.sites_with(Role::Administrator).bind();
    // SQL-safe: interpolates only the constant SELECT.
    let keys = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SELECT} WHERE $1 OR k.customer_id = ANY($2) OR k.site_id = ANY($3)
         ORDER BY k.created_at DESC, k.id"
    )))
    .bind(all)
    .bind(&customers)
    .bind(&sites)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(KeyList { keys }))
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub enum Expiry {
    #[serde(rename = "30d")]
    Days30,
    #[serde(rename = "90d")]
    Days90,
    #[default]
    #[serde(rename = "1y")]
    Year,
    #[serde(rename = "never")]
    Never,
}

impl Expiry {
    fn at(self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let days = match self {
            Self::Days30 => 30,
            Self::Days90 => 90,
            Self::Year => 365,
            Self::Never => return None,
        };
        Some(now + chrono::Duration::days(days))
    }
}

/// Body of `POST /api/api-keys`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRequest {
    pub name: String,
    /// The highest role the key acts with; `viewer` when absent.
    #[serde(default = "viewer")]
    pub role: Role,
    /// At most one of the two; neither for everything.
    pub customer_id: Option<Uuid>,
    pub site_id: Option<Uuid>,
    /// Absent or null for every area.
    pub areas: Option<Vec<String>>,
    #[serde(default)]
    pub expires: Expiry,
}

fn viewer() -> Role {
    Role::Viewer
}

/// Answer of `POST /api/api-keys`.
#[derive(Debug, Serialize)]
pub struct Created {
    pub key: KeyView,
    /// The key itself, shown this once.
    pub secret: String,
}

/// Whether `user` may make or turn off a key reaching `customer` or
/// `site`: 404 for a reach it does not see, 403 for one it sees without
/// administering it.
async fn may_manage(
    db: impl PgExecutor<'_>,
    user: &CurrentUser,
    customer: Option<Uuid>,
    site: Option<Uuid>,
) -> Result<(), ApiError> {
    match (customer, site) {
        (None, None) if user.administered_customers() == Scope::All => Ok(()),
        (None, None) => Err(ApiError::Forbidden),
        (Some(customer), None) => {
            if user.administered_customers().contains(customer) {
                return Ok(());
            }
            let sites: Vec<Uuid> =
                sqlx::query_scalar("SELECT id FROM sites WHERE customer_id = $1")
                    .bind(customer)
                    .fetch_all(db)
                    .await?;
            let readable = user.readable_sites();
            let seen = user.visible_customers().contains(customer)
                || sites.iter().any(|s| readable.contains(*s));
            Err(if seen {
                ApiError::Forbidden
            } else {
                ApiError::NotFound
            })
        }
        (None, Some(site)) => {
            authorize(user, Some(site), false)?;
            if user.sites_with(Role::Administrator).contains(site) {
                Ok(())
            } else {
                Err(ApiError::Forbidden)
            }
        }
        (Some(_), Some(_)) => Err(ApiError::Invalid(
            "A key reaches everything, one customer or one site, not both.".into(),
        )),
    }
}

/// `POST /api/api-keys`. Audited as `api_key.create`, without the secret.
pub async fn create(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<CreateRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Created>), ApiError> {
    let request = body(request)?;
    let name = request.name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME {
        return Err(ApiError::Invalid(format!(
            "Say what the key is for, in up to {MAX_NAME} characters."
        )));
    }
    let areas = match &request.areas {
        None => None,
        Some(given) => {
            let mut areas = Vec::new();
            for area in given {
                let area = Area::parse(area).ok_or_else(|| {
                    ApiError::Invalid(format!(
                        "unknown area `{area}`, expected one of {}",
                        Area::ALL.map(Area::as_str).join(", ")
                    ))
                })?;
                areas.push(area);
            }
            areas.sort();
            areas.dedup();
            if areas.is_empty() {
                return Err(ApiError::Invalid(
                    "Choose at least one area, or leave them out for all.".into(),
                ));
            }
            Some(
                areas
                    .iter()
                    .map(|a| a.as_str().to_owned())
                    .collect::<Vec<_>>(),
            )
        }
    };
    let site = match request.site_id {
        Some(site) => Some(
            sqlx::query_scalar::<_, Uuid>("SELECT id FROM sites WHERE id = $1")
                .bind(site)
                .fetch_optional(&state.pool)
                .await?
                .ok_or(ApiError::NotFound)?,
        ),
        None => None,
    };
    may_manage(&state.pool, &user, request.customer_id, site).await?;

    let secret = KeySecret::generate();
    let expires_at = request.expires.at(Utc::now());
    let mut tx = state.pool.begin().await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO api_keys (token_hash, prefix, name, role, customer_id, site_id, areas,
                               created_by, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
    )
    .bind(secret.hash())
    .bind(secret.shown())
    .bind(name)
    .bind(request.role.as_str())
    .bind(request.customer_id)
    .bind(site)
    .bind(&areas)
    .bind(user.id)
    .bind(expires_at)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(d) if d.is_foreign_key_violation() => ApiError::NotFound,
        _ => e.into(),
    })?;
    // SQL-safe: interpolates only the constant SELECT.
    let key: KeyView = sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} WHERE k.id = $1")))
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    record(&mut *tx, &user, "api_key.create", &key).await?;
    tx.commit().await?;
    tracing::info!(key = %id, user = %user.id, "API key created");
    Ok((
        StatusCode::CREATED,
        Json(Created {
            key,
            secret: secret.expose().to_owned(),
        }),
    ))
}

/// `DELETE /api/api-keys/{id}`: turns a key off for good. Audited as
/// `api_key.revoke`.
pub async fn revoke(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    let mut tx = state.pool.begin().await?;
    // SQL-safe: interpolates only the constant SELECT.
    let key: KeyView = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SELECT} WHERE k.id = $1 FOR UPDATE OF k"
    )))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiError::NotFound)?;
    may_manage(&mut *tx, &user, key.customer_id, key.site_id)
        .await
        .map_err(|e| match e {
            // A key of a reach the user does not administer is not shown
            // to it, so it does not exist for it either.
            ApiError::Forbidden => ApiError::NotFound,
            other => other,
        })?;
    sqlx::query("DELETE FROM api_keys WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    record(&mut *tx, &user, "api_key.revoke", &key).await?;
    tx.commit().await?;
    tracing::info!(key = %id, user = %user.id, "API key turned off");
    Ok(StatusCode::NO_CONTENT)
}

async fn record(
    db: impl PgExecutor<'_>,
    user: &CurrentUser,
    action: &str,
    key: &KeyView,
) -> Result<(), sqlx::Error> {
    let actor_id = user.id.to_string();
    let target_id = key.id.to_string();
    audit::record(
        db,
        Event {
            actor_kind: "user",
            actor_id: &actor_id,
            action,
            target: Some(("api_key", &target_id)),
            details: json!({
                "name": key.name,
                "role": key.role,
                "customer_id": key.customer_id,
                "site_id": key.site_id,
                "areas": key.areas,
                "expires_at": key.expires_at,
            }),
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    const C1: Uuid = Uuid::from_u128(0xc1);
    const C2: Uuid = Uuid::from_u128(0xc2);
    const S1: Uuid = Uuid::from_u128(1);
    const S2: Uuid = Uuid::from_u128(2);
    const S3: Uuid = Uuid::from_u128(3);

    /// Sites 1 and 2 belong to C1, site 3 to C2.
    fn customer_of(site: Uuid) -> Option<Uuid> {
        match site.as_u128() {
            1 | 2 => Some(C1),
            3 => Some(C2),
            _ => None,
        }
    }

    fn g(role: Role, customer_id: Option<Uuid>, site_id: Option<Uuid>) -> RoleGrant {
        RoleGrant {
            role,
            customer_id,
            site_id,
        }
    }

    #[test]
    fn secrets_are_random_and_parse_strictly() {
        let secret = KeySecret::generate();
        assert!(secret.0.starts_with("mfk_") && secret.0.len() == 4 + 43);
        // Plain booleans: a failure prints nothing about the secrets.
        assert!(secret != KeySecret::generate());
        assert!(KeySecret::parse(&secret.0) == Some(secret.clone()));
        let short = &secret.0[..secret.0.len() - 1];
        let padded = format!("{}=", secret.0);
        // Each case named by a fixed label, so a failure prints no part of
        // the secret.
        for (what, bad) in [
            ("empty", ""),
            ("prefix only", "mfk_"),
            ("one character short", short),
            ("without the prefix", &secret.0[4..]),
            ("padded", &padded),
        ] {
            assert!(KeySecret::parse(bad).is_none(), "{what} was accepted");
        }
        assert_eq!(secret.hash().len(), 32);
        assert_eq!(secret.shown().len(), SHOWN);
        // `Debug` never shows a secret; checked on a fixed, made-up one.
        let made_up = KeySecret(format!("{PREFIX}{}", "A".repeat(43)));
        assert_eq!(format!("{made_up:?}"), format!("KeySecret({REDACTED})"));
    }

    #[test]
    fn a_key_with_everything_only_lowers_roles() {
        let grants = [
            g(Role::Administrator, None, None),
            g(Role::Viewer, None, Some(S3)),
        ];
        assert_eq!(
            narrow(&grants, Role::Operator, Reach::All, customer_of),
            [
                g(Role::Operator, None, None),
                g(Role::Viewer, None, Some(S3))
            ]
        );
    }

    #[test]
    fn a_customer_key_keeps_only_that_customer() {
        let grants = [
            g(Role::Administrator, None, None),
            g(Role::Administrator, Some(C2), None),
            g(Role::Operator, None, Some(S1)),
            g(Role::Operator, None, Some(S3)),
        ];
        assert_eq!(
            narrow(
                &grants,
                Role::Administrator,
                Reach::Customer(C1),
                customer_of
            ),
            [
                g(Role::Administrator, Some(C1), None),
                g(Role::Operator, None, Some(S1)),
            ]
        );
        // A maker with nothing on the customer gives the key nothing.
        let grants = [g(Role::Administrator, Some(C2), None)];
        assert!(narrow(&grants, Role::Viewer, Reach::Customer(C1), customer_of).is_empty());
    }

    #[test]
    fn a_site_key_keeps_only_that_site() {
        let grants = [
            g(Role::Approver, Some(C1), None),
            g(Role::Administrator, None, Some(S2)),
            g(Role::Administrator, Some(C2), None),
        ];
        assert_eq!(
            narrow(&grants, Role::Viewer, Reach::Site(S1, C1), customer_of),
            [g(Role::Viewer, None, Some(S1))]
        );
        assert_eq!(
            narrow(
                &grants,
                Role::Administrator,
                Reach::Site(S2, C1),
                customer_of
            ),
            [
                g(Role::Approver, None, Some(S2)),
                g(Role::Administrator, None, Some(S2)),
            ]
        );
        let global = [g(Role::Viewer, None, None)];
        assert_eq!(
            narrow(
                &global,
                Role::Administrator,
                Reach::Site(S3, C2),
                customer_of
            ),
            [g(Role::Viewer, None, Some(S3))]
        );
    }

    #[test]
    fn routes_fall_in_areas() {
        let get = Method::GET;
        let post = Method::POST;
        let cases = [
            (&get, "/api/servers", None),
            (&get, "/api/servers/{id}", None),
            (&get, "/api/sites", None),
            (&get, "/api/customers", None),
            (&get, "/api/relays/{id}", None),
            (&get, "/api/health", None),
            (&post, "/api/customers", Some(Area::Administration)),
            (
                &post,
                "/api/customers/{id}/sites",
                Some(Area::Administration),
            ),
            (&post, "/api/servers/{id}/revoke", Some(Area::Servers)),
            (&get, "/api/servers/{id}/logs", Some(Area::Servers)),
            (&get, "/api/metrics/query", Some(Area::Servers)),
            (&get, "/api/incidents", Some(Area::Alerts)),
            (&post, "/api/incidents/acknowledge", Some(Area::Alerts)),
            (&post, "/api/silences", Some(Area::Alerts)),
            (&post, "/api/sites/{id}/thresholds", Some(Area::Alerts)),
            (&get, "/api/servers/{id}/sensor_configs", Some(Area::Alerts)),
            (&get, "/api/sites/{id}/sensors", Some(Area::Alerts)),
            (&post, "/api/relays/{id}/update", Some(Area::Updates)),
            (&post, "/api/servers/{id}/commands", Some(Area::Updates)),
            (&get, "/api/commands/{id}", Some(Area::Updates)),
            (&post, "/api/campaigns/{id}/start", Some(Area::Updates)),
            (&get, "/api/settings/auto-update", Some(Area::Updates)),
            (
                &post,
                "/api/sites/{id}/ai-commands",
                Some(Area::Remediation),
            ),
            (
                &post,
                "/api/remediation/approvals/{id}/approve",
                Some(Area::Remediation),
            ),
            (&post, "/api/dashboard-data", Some(Area::Dashboards)),
            (&post, "/api/display-links", Some(Area::Dashboards)),
            (&post, "/api/tokens", Some(Area::Administration)),
            (&get, "/api/settings/server", Some(Area::Administration)),
            (&get, "/api/users", Some(Area::Administration)),
            (&get, "/api/backups", Some(Area::Administration)),
        ];
        for (method, path, area) in cases {
            assert_eq!(Area::of(method, path), area, "{method} {path}");
        }
    }
}
