//! Display links (ADR-0099): a dashboard, or several in turn, on a wall
//! screen without signing in.
//!
//! An administrator makes a link for dashboards they may link
//! ([`may_link`]). Its secret is in the URL fragment, which browsers never
//! send; the screen trades it for a short session in a cookie
//! (`POST /api/display/start`), so the secret crosses the network once an
//! hour. Every request acts as the person who made the link, loaded again
//! with their roles of now: a disabled person's links stop, and a
//! dashboard they may no longer link is not shown. A screen reads only its
//! link's dashboards, whose specs come from the database, never from the
//! screen.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::header::{COOKIE, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgExecutor;
use uuid::Uuid;

use super::audit::{self, Event};
use super::auth::{self, CurrentUser};
use super::client_addr::Peer;
use super::dashboard_data::{self, DataRequest, DataResponse, Within};
use super::dashboards::{may_link, may_see, Row, Sharing, Spec, MAX_NAME, SELECT};
use super::one_time::OneTimeToken;
use super::sensors::{body, parse_id};
use super::{ApiError, ApiSettings, ApiState};
use crate::enroll::limiter::FailureLimiter;

/// The page a link opens; the secret follows `#`.
pub const PAGE_PATH: &str = "/display/";
/// How often a screen asks for new data. The answers it gets are cached
/// 15 s (ADR-0095), so screens showing the same dashboard share them.
pub const REFRESH_SECONDS: u32 = 30;
/// How long a screen's session lasts; the screen then opens its link again.
pub const SESSION_LIFETIME: Duration = Duration::from_secs(3_600);
/// Most dashboards one link shows in turn.
pub const MAX_DASHBOARDS: usize = 12;
/// Bounds of the time each dashboard stays on screen.
pub const SECONDS_PER_PAGE: std::ops::RangeInclusive<u32> = 10..=3_600;
/// Times one link may be opened per hour, by all its screens.
pub const STARTS_PER_HOUR: u32 = 30;
/// Requests one screen may send per minute: a refresh every 30 s and a
/// new page every 10 s at most need 8.
pub const REQUESTS_PER_MINUTE: u32 = 30;
/// `last_used_at` is written at most this often.
const TOUCH_EVERY_SECONDS: f64 = 60.0;

const COOKIE_SECURE: &str = "__Host-mf_display";
const COOKIE_PLAIN: &str = "mf_display";

const LINK_GONE: &str =
    "This display link was turned off or has expired. Ask an administrator for a new one.";

/// How long a link works, chosen by the administrator who makes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Expiry {
    #[serde(rename = "30d")]
    Month,
    #[serde(rename = "1y")]
    Year,
    #[serde(rename = "never")]
    Never,
}

impl Expiry {
    fn at(self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match self {
            Self::Month => Some(now + chrono::Duration::days(30)),
            Self::Year => Some(now + chrono::Duration::days(365)),
            Self::Never => None,
        }
    }
}

/// A fixed-window counter per key, in memory: per instance of the central
/// server, reset on restart. When full, keys share one bucket.
#[derive(Debug)]
struct Counter<K> {
    max: u32,
    window: Duration,
    entries: Mutex<HashMap<K, (u32, Instant)>>,
}

/// Keys tracked at once by each [`Counter`].
const MAX_KEYS: usize = 10_000;

impl<K: Hash + Eq + Clone> Counter<K> {
    fn new(max: u32, window: Duration) -> Self {
        Self {
            max,
            window,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Counts one use of `key` at `now`; false once over the limit.
    fn allow(&self, key: &K, overflow: &K, now: Instant) -> bool {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let mut key = key;
        if !entries.contains_key(key) && entries.len() >= MAX_KEYS {
            entries.retain(|_, (_, start)| now - *start < self.window);
            if entries.len() >= MAX_KEYS {
                key = overflow;
            }
        }
        let entry = entries.entry(key.clone()).or_insert((0, now));
        if now - entry.1 >= self.window {
            *entry = (0, now);
        }
        entry.0 = entry.0.saturating_add(1);
        entry.0 <= self.max
    }
}

/// The rate limits of screens. Secrets have 256 bits, so these do not
/// stop guessing; they keep a leaked link or a broken screen from loading
/// the server.
#[derive(Debug)]
pub struct Limits {
    /// Links that do not work, per source address.
    failures: FailureLimiter,
    /// Openings per link.
    starts: Counter<Uuid>,
    /// Requests per screen session, by the hash of its token.
    requests: Counter<Vec<u8>>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            failures: FailureLimiter::default(),
            starts: Counter::new(STARTS_PER_HOUR, Duration::from_secs(3_600)),
            requests: Counter::new(REQUESTS_PER_MINUTE, Duration::from_secs(60)),
        }
    }
}

fn too_many() -> ApiError {
    ApiError::TooManyRequests(
        "This screen asked too often. It tries again by itself in a few minutes.".into(),
    )
}

fn cookie_name(settings: &ApiSettings) -> &'static str {
    if settings.cookie_secure {
        COOKIE_SECURE
    } else {
        COOKIE_PLAIN
    }
}

fn set_cookie(settings: &ApiSettings, token: &OneTimeToken) -> HeaderValue {
    let secure = if settings.cookie_secure {
        "; Secure"
    } else {
        ""
    };
    let cookie = format!(
        "{}={}; Path=/; Max-Age={}; HttpOnly; SameSite=Strict{secure}",
        cookie_name(settings),
        token.expose(),
        SESSION_LIFETIME.as_secs()
    );
    HeaderValue::from_str(&cookie).expect("cookie is ASCII")
}

fn session_token(headers: &HeaderMap, settings: &ApiSettings) -> Option<OneTimeToken> {
    let name = cookie_name(settings);
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .and_then(|(_, v)| OneTimeToken::parse(v))
}

/// A dashboard a link shows, by name.
#[derive(Debug, Serialize)]
pub struct LinkDashboard {
    pub id: Uuid,
    pub name: String,
}

/// A display link, without its secret.
#[derive(Debug, Serialize)]
pub struct LinkView {
    pub id: Uuid,
    pub name: String,
    /// Those the reader sees, in the order shown.
    pub dashboards: Vec<LinkDashboard>,
    /// How many more it shows that the reader does not see.
    pub other_dashboards: usize,
    pub seconds_per_page: u32,
    /// Deleting that person deletes the link.
    pub created_by_username: String,
    pub created_at: DateTime<Utc>,
    /// `null`: never.
    pub expires_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
}

/// Body of `GET /api/dashboards/{id}/display-links`.
#[derive(Debug, Serialize)]
pub struct LinkList {
    pub links: Vec<LinkView>,
}

/// Body of `POST /api/display-links`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRequest {
    pub name: String,
    pub dashboard_ids: Vec<Uuid>,
    pub seconds_per_page: u32,
    pub expires: Expiry,
}

/// Answer to `POST /api/display-links`: the only time the URL is shown.
#[derive(Debug, Serialize)]
pub struct Created {
    #[serde(flatten)]
    pub link: LinkView,
    pub url: String,
}

/// Body of `POST /api/display/start`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartRequest {
    pub token: String,
}

/// One dashboard on a screen.
#[derive(Debug, Serialize)]
pub struct ScreenDashboard {
    pub id: Uuid,
    pub name: String,
    pub spec: Spec,
}

/// What a screen shows: `POST /api/display/start` and `GET /api/display`.
#[derive(Debug, Serialize)]
pub struct ScreenView {
    pub name: String,
    pub seconds_per_page: u32,
    pub refresh_seconds: u32,
    pub expires_at: Option<DateTime<Utc>>,
    pub dashboards: Vec<ScreenDashboard>,
}

/// Body of `POST /api/display/data`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenDataRequest {
    pub dashboard_id: Uuid,
}

/// The dashboard rows `ids`, in that order, skipping missing ones.
async fn dashboard_rows(db: impl PgExecutor<'_>, ids: &[Uuid]) -> Result<Vec<Row>, ApiError> {
    // SQL-safe: interpolates only the constant SELECT.
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SELECT} WHERE d.id = ANY($1)"
    )))
    .bind(ids)
    .fetch_all(db)
    .await?;
    let mut by_id: HashMap<Uuid, Row> = rows.into_iter().map(|r| (r.id, r)).collect();
    Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
}

/// The dashboards of `link`, in order.
async fn link_dashboards(db: impl PgExecutor<'_>, link: Uuid) -> Result<Vec<Uuid>, ApiError> {
    Ok(sqlx::query_scalar(
        "SELECT dashboard_id FROM display_link_dashboards WHERE link_id = $1 ORDER BY position",
    )
    .bind(link)
    .fetch_all(db)
    .await?)
}

#[derive(sqlx::FromRow)]
struct LinkRow {
    id: Uuid,
    name: String,
    created_by: Uuid,
    created_by_username: String,
    created_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
    seconds_per_page: i32,
    last_used_at: Option<DateTime<Utc>>,
    dashboard_ids: Vec<Uuid>,
}

const LINKS: &str = "SELECT l.id, l.name, l.created_by, u.username AS created_by_username,
        l.created_at, l.expires_at, l.seconds_per_page, l.last_used_at,
        array(SELECT dashboard_id FROM display_link_dashboards
              WHERE link_id = l.id ORDER BY position) AS dashboard_ids
    FROM display_links l JOIN users u ON u.id = l.created_by";

/// `row` as `user` may read it: the names of the dashboards they see.
async fn link_view(
    state: &ApiState,
    user: &CurrentUser,
    row: LinkRow,
) -> Result<LinkView, ApiError> {
    let rows = dashboard_rows(&state.pool, &row.dashboard_ids).await?;
    let dashboards: Vec<LinkDashboard> = rows
        .into_iter()
        .filter(|d| may_see(user, d.owner_id, d.sharing()))
        .map(|d| LinkDashboard {
            id: d.id,
            name: d.name,
        })
        .collect();
    Ok(LinkView {
        id: row.id,
        name: row.name,
        other_dashboards: row.dashboard_ids.len() - dashboards.len(),
        dashboards,
        seconds_per_page: u32::try_from(row.seconds_per_page).unwrap_or_default(),
        created_by_username: row.created_by_username,
        created_at: row.created_at,
        expires_at: row.expires_at,
        last_used_at: row.last_used_at,
    })
}

/// The dashboard `id` if `user` may link it: 404 when it is missing or
/// unseen, 403 when seen but not theirs to link.
async fn linkable(state: &ApiState, user: &CurrentUser, id: Uuid) -> Result<Row, ApiError> {
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let row = dashboard_rows(&state.pool, &[id])
        .await?
        .pop()
        .filter(|d| may_see(user, d.owner_id, d.sharing()))
        .ok_or(ApiError::NotFound)?;
    if !may_link(user, row.owner_id, row.sharing()) {
        return Err(ApiError::Forbidden);
    }
    Ok(row)
}

/// `GET /api/dashboards/{id}/display-links`: the links that show this
/// dashboard, newest first, for those who may link it.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<LinkList>, ApiError> {
    user.in_person()?;
    let id = parse_id(&id)?;
    linkable(&state, &user, id).await?;
    // SQL-safe: interpolates only the constant LINKS.
    let rows: Vec<LinkRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{LINKS} WHERE l.id IN (SELECT link_id FROM display_link_dashboards WHERE dashboard_id = $1)
         ORDER BY l.created_at DESC, l.id"
    )))
    .bind(id)
    .fetch_all(&state.pool)
    .await?;
    let mut links = Vec::with_capacity(rows.len());
    for row in rows {
        links.push(link_view(&state, &user, row).await?);
    }
    Ok(Json(LinkList { links }))
}

async fn record(
    db: impl PgExecutor<'_>,
    user: &CurrentUser,
    action: &str,
    link: &LinkView,
    dashboards: &[Uuid],
) -> Result<(), sqlx::Error> {
    let actor = user.id.to_string();
    audit::record(
        db,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action,
            target: Some(("display_link", &link.id.to_string())),
            details: json!({
                "name": link.name,
                "dashboards": dashboards,
                "seconds_per_page": link.seconds_per_page,
                "expires_at": link.expires_at,
                "created_by": link.created_by_username,
            }),
        },
    )
    .await
}

/// `POST /api/display-links`: a new link for dashboards the user may
/// link. Audited as `display_link.create`; the URL with its secret is in
/// this answer only. Never through an API key: the link reads as its
/// maker with all their roles, beyond the key's reach, and would outlive
/// the key.
pub async fn create(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<CreateRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Created>), ApiError> {
    user.in_person()?;
    let request = body(request)?;
    let name = request.name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME {
        return Err(ApiError::Invalid(format!(
            "Say where the screen is, in up to {MAX_NAME} characters."
        )));
    }
    let ids = &request.dashboard_ids;
    if ids.is_empty() || ids.len() > MAX_DASHBOARDS {
        return Err(ApiError::Invalid(format!(
            "A display link shows 1 to {MAX_DASHBOARDS} dashboards."
        )));
    }
    if (1..ids.len()).any(|i| ids[..i].contains(&ids[i])) {
        return Err(ApiError::Invalid(
            "Each dashboard is shown once per link.".into(),
        ));
    }
    if !SECONDS_PER_PAGE.contains(&request.seconds_per_page) {
        return Err(ApiError::Invalid(format!(
            "Each dashboard stays {} to {} seconds.",
            SECONDS_PER_PAGE.start(),
            SECONDS_PER_PAGE.end()
        )));
    }
    for id in ids {
        linkable(&state, &user, *id).await?;
    }

    let token = OneTimeToken::generate();
    let expires_at = request.expires.at(Utc::now());
    let mut tx = state.pool.begin().await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO display_links (token_hash, name, created_by, expires_at, seconds_per_page)
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(token.hash())
    .bind(name)
    .bind(user.id)
    .bind(expires_at)
    .bind(request.seconds_per_page as i32)
    .fetch_one(&mut *tx)
    .await?;
    for (position, dashboard) in ids.iter().enumerate() {
        sqlx::query(
            "INSERT INTO display_link_dashboards (link_id, position, dashboard_id)
             VALUES ($1, $2, $3)",
        )
        .bind(id)
        .bind(position as i16)
        .bind(dashboard)
        .execute(&mut *tx)
        .await?;
    }
    // SQL-safe: interpolates only the constant LINKS.
    let row: LinkRow = sqlx::query_as(sqlx::AssertSqlSafe(format!("{LINKS} WHERE l.id = $1")))
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    let link = link_view(&state, &user, row).await?;
    record(&mut *tx, &user, "display_link.create", &link, ids).await?;
    tx.commit().await?;
    tracing::info!(link = %id, user = %user.id, "display link created");
    let url = token.url(&state.settings.web_url, PAGE_PATH);
    Ok((StatusCode::CREATED, Json(Created { link, url })))
}

/// `DELETE /api/display-links/{id}`: turns a link off for good, and its
/// screens with it. Its maker may, and anyone who may link one of its
/// dashboards. Audited as `display_link.revoke`.
pub async fn revoke(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    user.in_person()?;
    let id = parse_id(&id)?;
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    // SQL-safe: interpolates only the constant LINKS.
    let row: LinkRow = sqlx::query_as(sqlx::AssertSqlSafe(format!("{LINKS} WHERE l.id = $1")))
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(ApiError::NotFound)?;
    let dashboards = dashboard_rows(&state.pool, &row.dashboard_ids).await?;
    let mine = row.created_by == user.id;
    if !mine {
        if !dashboards
            .iter()
            .any(|d| may_see(&user, d.owner_id, d.sharing()))
        {
            return Err(ApiError::NotFound);
        }
        if !dashboards
            .iter()
            .any(|d| may_link(&user, d.owner_id, d.sharing()))
        {
            return Err(ApiError::Forbidden);
        }
    }
    let ids = row.dashboard_ids.clone();
    let link = link_view(&state, &user, row).await?;
    let mut tx = state.pool.begin().await?;
    let done = sqlx::query("DELETE FROM display_links WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    if done.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    record(&mut *tx, &user, "display_link.revoke", &link, &ids).await?;
    tx.commit().await?;
    tracing::info!(link = %id, user = %user.id, "display link turned off");
    Ok(StatusCode::NO_CONTENT)
}

/// A link that works now: not expired, its maker not disabled.
#[derive(sqlx::FromRow)]
struct Live {
    id: Uuid,
    name: String,
    created_by: Uuid,
    expires_at: Option<DateTime<Utc>>,
    seconds_per_page: i32,
}

const LIVE: &str = "SELECT l.id, l.name, l.created_by, l.expires_at, l.seconds_per_page
    FROM display_links l JOIN users u ON u.id = l.created_by
    WHERE (l.expires_at IS NULL OR l.expires_at > now()) AND u.disabled_at IS NULL";

async fn touch(db: impl PgExecutor<'_>, link: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE display_links SET last_used_at = now()
         WHERE id = $1
           AND (last_used_at IS NULL OR last_used_at <= now() - make_interval(secs => $2))",
    )
    .bind(link)
    .bind(TOUCH_EVERY_SECONDS)
    .execute(db)
    .await?;
    Ok(())
}

/// The maker of `link` with their roles of now, and the dashboards of the
/// link they may still link, in order. 403 when none is left.
async fn screen(
    state: &ApiState,
    link: &Live,
) -> Result<(CurrentUser, Vec<(Row, Spec)>), ApiError> {
    let maker = auth::load_user(&state.pool, link.created_by, None).await?;
    let ids = link_dashboards(&state.pool, link.id).await?;
    let mut shown = Vec::new();
    for row in dashboard_rows(&state.pool, &ids).await? {
        if may_link(&maker, row.owner_id, row.sharing()) {
            let spec = Spec::parse(row.spec.clone()).map_err(ApiError::Internal)?;
            shown.push((row, spec));
        }
    }
    if shown.is_empty() {
        return Err(ApiError::Forbidden);
    }
    Ok((maker, shown))
}

fn screen_view(link: &Live, shown: Vec<(Row, Spec)>) -> ScreenView {
    ScreenView {
        name: link.name.clone(),
        seconds_per_page: u32::try_from(link.seconds_per_page).unwrap_or(60),
        refresh_seconds: REFRESH_SECONDS,
        expires_at: link.expires_at,
        dashboards: shown
            .into_iter()
            .map(|(row, spec)| ScreenDashboard {
                id: row.id,
                name: row.name,
                spec,
            })
            .collect(),
    }
}

/// `POST /api/display/start`, without a session: trades a link's secret
/// for a screen session in a cookie, and says what to show. 410 for a link
/// that does not work, 403 when its maker may show none of its dashboards,
/// 429 over the limits.
pub async fn start(
    State(state): State<ApiState>,
    peer: Peer,
    headers: HeaderMap,
    request: Result<Json<StartRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let request = body(request)?;
    let from = state.settings.trusted_proxies.source(&peer, &headers).await;
    let limits = &state.display_limits;
    if limits.failures.is_blocked(from) {
        return Err(too_many());
    }
    let found = match OneTimeToken::parse(&request.token) {
        // SQL-safe: interpolates only the constant LIVE.
        Some(token) => {
            sqlx::query_as::<_, Live>(sqlx::AssertSqlSafe(format!("{LIVE} AND l.token_hash = $1")))
                .bind(token.hash())
                .fetch_optional(&state.pool)
                .await?
        }
        None => None,
    };
    let Some(link) = found else {
        limits.failures.record_failure(from);
        return Err(ApiError::Gone(LINK_GONE.into()));
    };
    if !limits.starts.allow(&link.id, &Uuid::nil(), Instant::now()) {
        return Err(too_many());
    }
    let (_, shown) = screen(&state, &link).await?;

    let session = OneTimeToken::generate();
    let mut tx = state.pool.begin().await?;
    sqlx::query("DELETE FROM display_sessions WHERE expires_at <= now()")
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO display_sessions (token_hash, link_id, expires_at)
         VALUES ($1, $2, now() + make_interval(secs => $3))",
    )
    .bind(session.hash())
    .bind(link.id)
    .bind(SESSION_LIFETIME.as_secs_f64())
    .execute(&mut *tx)
    .await?;
    touch(&mut *tx, link.id).await?;
    tx.commit().await?;
    let cookie = set_cookie(&state.settings, &session);
    Ok(([(SET_COOKIE, cookie)], Json(screen_view(&link, shown))).into_response())
}

/// The link of the screen session in the request's cookie. 401 without a
/// live session or once the link stops working, so the screen opens its
/// link again and learns why; 429 over the request limit.
async fn session_link(state: &ApiState, headers: &HeaderMap) -> Result<Live, ApiError> {
    let token = session_token(headers, &state.settings).ok_or(ApiError::Unauthorized)?;
    let hash = token.hash();
    if !state
        .display_limits
        .requests
        .allow(&hash, &Vec::new(), Instant::now())
    {
        return Err(too_many());
    }
    // SQL-safe: interpolates only the constant LIVE.
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{LIVE} AND l.id = (SELECT link_id FROM display_sessions
                            WHERE token_hash = $1 AND expires_at > now())"
    )))
    .bind(hash)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(ApiError::Unauthorized)
}

/// `GET /api/display`, with a screen session: what the screen shows now,
/// so a changed dashboard or link shows without reloading.
pub async fn view(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<ScreenView>, ApiError> {
    let link = session_link(&state, &headers).await?;
    let (_, shown) = screen(&state, &link).await?;
    Ok(Json(screen_view(&link, shown)))
}

/// The dashboard's sharing as the data request's `within`, as its page
/// sends it.
fn within(sharing: Sharing) -> Within {
    match sharing {
        Sharing::Personal => Within::All,
        Sharing::Site { id } => Within::Site { id },
        Sharing::Customer { id } => Within::Customer { id },
    }
}

/// `POST /api/display/data`, with a screen session: the panels of one of
/// the link's dashboards, read as the link's maker with their roles of
/// now, over the dashboard's own range. 404 for a dashboard the link does
/// not show.
pub async fn data(
    State(state): State<ApiState>,
    headers: HeaderMap,
    request: Result<Json<ScreenDataRequest>, JsonRejection>,
) -> Result<Json<DataResponse>, ApiError> {
    let request = body(request)?;
    let link = session_link(&state, &headers).await?;
    let (maker, shown) = screen(&state, &link).await?;
    let (row, spec) = shown
        .into_iter()
        .find(|(row, _)| row.id == request.dashboard_id)
        .ok_or(ApiError::NotFound)?;
    let data = DataRequest {
        range: spec.range,
        within: within(row.sharing()),
        panels: spec.panels,
    };
    let answer = dashboard_data::compute(&state, &maker, &data).await?;
    touch(&state.pool, link.id).await?;
    Ok(Json(answer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_allow_up_to_the_limit_per_window() {
        let counter = Counter::new(2, Duration::from_secs(60));
        let t0 = Instant::now();
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        assert!(counter.allow(&a, &Uuid::nil(), t0));
        assert!(counter.allow(&a, &Uuid::nil(), t0));
        assert!(!counter.allow(&a, &Uuid::nil(), t0));
        assert!(counter.allow(&b, &Uuid::nil(), t0));
        assert!(counter.allow(&a, &Uuid::nil(), t0 + Duration::from_secs(60)));
    }

    #[test]
    fn expiry_choices() {
        let now = Utc::now();
        assert_eq!(Expiry::Never.at(now), None);
        assert_eq!(
            Expiry::Month.at(now),
            Some(now + chrono::Duration::days(30))
        );
        let parsed: Expiry = serde_json::from_value(json!("1y")).unwrap();
        assert_eq!(parsed, Expiry::Year);
        assert!(serde_json::from_value::<Expiry>(json!("2y")).is_err());
    }

    #[test]
    fn the_session_cookie_is_http_only_and_strict() {
        let token = OneTimeToken::generate();
        let settings = ApiSettings::default();
        let set = set_cookie(&settings, &token);
        let set = set.to_str().unwrap();
        assert!(set.starts_with(&format!("__Host-mf_display={}; Path=/;", token.expose())));
        for attr in ["HttpOnly", "SameSite=Strict", "Secure", "Max-Age=3600"] {
            assert!(set.contains(attr), "{set}");
        }
        let mut headers = HeaderMap::new();
        headers.insert(
            COOKIE,
            HeaderValue::from_str(&format!("a=b; __Host-mf_display={}", token.expose())).unwrap(),
        );
        assert_eq!(session_token(&headers, &settings), Some(token));
    }

    #[test]
    fn a_screen_reads_a_dashboard_within_its_sharing() {
        let id = Uuid::from_u128(7);
        assert_eq!(within(Sharing::Personal), Within::All);
        assert_eq!(within(Sharing::Site { id }), Within::Site { id });
        assert_eq!(within(Sharing::Customer { id }), Within::Customer { id });
    }
}
