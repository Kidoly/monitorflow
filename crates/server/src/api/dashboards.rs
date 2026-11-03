//! Dashboards (ADR-0095): pages of panels built by picking from lists,
//! kept for one user or shared with a site or a customer.
//!
//! A dashboard's panels are a versioned JSON [`Spec`], validated on every
//! write. Who sees and edits one follows the roles: its owner for "only
//! me", readers and operators of the site or of the customer as a whole
//! for shared ones. On save, everything a panel names must be readable by
//! the editor and lie within the sharing; on display, `dashboard_data`
//! resolves the servers again for the viewer, so a stored dashboard never
//! shows more than the viewer may read.

use std::collections::BTreeSet;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use monitorflow_sensors::validate;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgExecutor;
use uuid::Uuid;

use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::metrics::check_metric;
use super::sensors::{body, parse_id};
use super::{ApiError, ApiState};

/// The spec version this server writes and reads.
pub const SCHEMA_VERSION: u32 = 1;
/// Most panels on one dashboard.
pub const MAX_PANELS: usize = 24;
/// Longest dashboard name and panel title, in characters.
pub const MAX_NAME: usize = 80;
/// Most servers a panel names one by one.
pub const MAX_NAMED_SERVERS: usize = 50;
/// Most rows of a top list or alerts panel.
pub const MAX_COUNT: u32 = 20;

/// Time ranges, as on the sensor pages (ADR-0020).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeRange {
    #[serde(rename = "1h")]
    Hour,
    #[serde(rename = "6h")]
    SixHours,
    #[serde(rename = "24h")]
    Day,
    #[serde(rename = "7d")]
    Week,
    #[serde(rename = "30d")]
    Month,
}

impl TimeRange {
    pub fn seconds(self) -> i64 {
        match self {
            Self::Hour => 3_600,
            Self::SixHours => 21_600,
            Self::Day => 86_400,
            Self::Week => 604_800,
            Self::Month => 2_592_000,
        }
    }

    /// Seconds between points, a few hundred points per range.
    pub fn step(self) -> i64 {
        match self {
            Self::Hour => 30,
            Self::SixHours => 120,
            Self::Day => 300,
            Self::Week => 1_800,
            Self::Month => 7_200,
        }
    }

    /// Long ranges summarize each step instead of sampling it.
    pub fn rolls_up(self) -> bool {
        matches!(self, Self::Day | Self::Week | Self::Month)
    }
}

/// Which servers a panel shows, within the dashboard's sharing.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Servers {
    /// Every server of the dashboard.
    #[default]
    All,
    Site {
        id: Uuid,
    },
    Customer {
        id: Uuid,
    },
    /// Chosen servers.
    List {
        ids: Vec<Uuid>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PanelKind {
    Chart,
    Figure,
    Top,
    Alerts,
}

/// How the servers' series become what a chart or figure shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Combine {
    /// One line per series, the 8 worst (charts only).
    Each,
    Avg,
    Max,
    Min,
    Sum,
    /// How many servers report the metric (figures only).
    Count,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Order {
    Highest,
    Lowest,
}

/// One panel. Fields that do not apply to its kind must be absent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Panel {
    /// Unique within the dashboard, chosen by the client.
    pub id: String,
    pub title: String,
    pub kind: PanelKind,
    /// Columns of the three-column grid.
    #[serde(default = "one")]
    pub width: u8,
    #[serde(default)]
    pub servers: Servers,
    /// Full name, `monitorflow_…`; not for alerts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metric: Option<String>,
    /// Only this instance (a disk, a core); every one when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// Charts and figures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub combine: Option<Combine>,
    /// Top lists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<Order>,
    /// Rows of a top list or alerts panel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
}

fn one() -> u8 {
    1
}

/// The stored content of a dashboard.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub version: u32,
    /// What the dashboard opens with.
    pub range: TimeRange,
    pub panels: Vec<Panel>,
}

impl Spec {
    /// Reads and validates a spec. Older versions would be upgraded here;
    /// newer ones are refused.
    pub fn parse(value: Value) -> Result<Self, String> {
        match value.get("version").and_then(Value::as_u64) {
            Some(v) if v == u64::from(SCHEMA_VERSION) => {}
            Some(v) if v > u64::from(SCHEMA_VERSION) => {
                return Err(
                    "This dashboard was saved by a newer version of MonitorFlow.".to_owned(),
                )
            }
            _ => return Err(format!("version must be {SCHEMA_VERSION}")),
        }
        let spec: Spec = serde_json::from_value(value).map_err(|e| e.to_string())?;
        check_panels(&spec.panels)?;
        Ok(spec)
    }
}

/// Checks panels on their own, without the database: kinds, fields,
/// lengths and counts. The error is a sentence for the form.
pub fn check_panels(panels: &[Panel]) -> Result<(), String> {
    if panels.len() > MAX_PANELS {
        return Err(format!("A dashboard has at most {MAX_PANELS} panels."));
    }
    let mut ids = BTreeSet::new();
    for panel in panels {
        let ok_id = (1..=36).contains(&panel.id.len())
            && panel
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !ok_id || !ids.insert(panel.id.as_str()) {
            return Err("Each panel needs its own id of up to 36 letters and digits.".into());
        }
        check_panel(panel).map_err(|e| format!("{} {e}", describe(panel)))?;
    }
    Ok(())
}

fn describe(panel: &Panel) -> String {
    let title = panel.title.trim();
    if title.is_empty() {
        "A panel:".to_owned()
    } else {
        format!(
            "Panel “{}”:",
            title.chars().take(MAX_NAME).collect::<String>()
        )
    }
}

fn check_panel(panel: &Panel) -> Result<(), String> {
    let title = panel.title.trim();
    if title.is_empty() || title.chars().count() > MAX_NAME {
        return Err(format!("give it a title of up to {MAX_NAME} characters."));
    }
    if !(1..=3).contains(&panel.width) {
        return Err("its width is 1, 2 or 3 columns.".into());
    }
    if let Servers::List { ids } = &panel.servers {
        if ids.is_empty() || ids.len() > MAX_NAMED_SERVERS {
            return Err(format!("choose 1 to {MAX_NAMED_SERVERS} servers."));
        }
    }
    let count_ok = |count: Option<u32>| count.is_some_and(|c| (1..=MAX_COUNT).contains(&c));
    match panel.kind {
        PanelKind::Alerts => {
            if panel.metric.is_some()
                || panel.instance.is_some()
                || panel.combine.is_some()
                || panel.order.is_some()
            {
                return Err("an alerts panel takes servers and a count only.".into());
            }
            if !count_ok(panel.count) {
                return Err(format!("show 1 to {MAX_COUNT} alerts."));
            }
            return Ok(());
        }
        PanelKind::Chart => {
            if panel.order.is_some() || panel.count.is_some() {
                return Err("a chart takes no order or count.".into());
            }
            if matches!(panel.combine, None | Some(Combine::Count)) {
                return Err("say how servers combine: each, avg, max, min or sum.".into());
            }
        }
        PanelKind::Figure => {
            if panel.order.is_some() || panel.count.is_some() {
                return Err("a figure takes no order or count.".into());
            }
            if matches!(panel.combine, None | Some(Combine::Each)) {
                return Err("say how servers combine: avg, max, min, sum or count.".into());
            }
        }
        PanelKind::Top => {
            if panel.combine.is_some() || panel.order.is_none() {
                return Err("a top list takes an order, highest or lowest.".into());
            }
            if !count_ok(panel.count) {
                return Err(format!("show 1 to {MAX_COUNT} rows."));
            }
        }
    }
    let metric = panel.metric.as_deref().ok_or("choose a metric.")?;
    check_metric(metric).map_err(|e| e.to_string())?;
    if let Some(instance) = &panel.instance {
        validate::instance(instance).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Who a dashboard is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Sharing {
    /// Only me: its owner.
    Personal,
    Site {
        id: Uuid,
    },
    Customer {
        id: Uuid,
    },
}

/// [`Sharing`] with the name of the site or customer, for display.
#[derive(Debug, Serialize)]
pub struct SharingView {
    #[serde(flatten)]
    pub sharing: Sharing,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// One row of `GET /api/dashboards`.
#[derive(Debug, Serialize)]
pub struct DashboardSummary {
    pub id: Uuid,
    pub name: String,
    pub shared_with: SharingView,
    pub panels: usize,
    pub updated_at: DateTime<Utc>,
    /// `null` once the user is deleted.
    pub updated_by_username: Option<String>,
    pub can_edit: bool,
}

/// Body of `GET /api/dashboards`, by name.
#[derive(Debug, Serialize)]
pub struct DashboardList {
    pub dashboards: Vec<DashboardSummary>,
}

/// `GET /api/dashboards/{id}` and the answer to writes.
#[derive(Debug, Serialize)]
pub struct DashboardView {
    #[serde(flatten)]
    pub summary: DashboardSummary,
    pub spec: Spec,
    /// Pass back on `PUT`.
    pub revision: i64,
}

/// Body of `POST /api/dashboards` and `PUT /api/dashboards/{id}`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DashboardRequest {
    pub name: String,
    pub shared_with: Sharing,
    pub spec: Value,
    /// The revision the edit started from; `PUT` only.
    #[serde(default)]
    pub revision: Option<i64>,
}

#[derive(Debug, sqlx::FromRow)]
struct Row {
    id: Uuid,
    name: String,
    owner_id: Option<Uuid>,
    site_id: Option<Uuid>,
    customer_id: Option<Uuid>,
    place_name: Option<String>,
    spec: Value,
    revision: i64,
    updated_at: DateTime<Utc>,
    updated_by_username: Option<String>,
}

impl Row {
    fn sharing(&self) -> Sharing {
        match (self.site_id, self.customer_id) {
            (Some(id), _) => Sharing::Site { id },
            (None, Some(id)) => Sharing::Customer { id },
            (None, None) => Sharing::Personal,
        }
    }
}

const SELECT: &str = "SELECT d.id, d.name, d.owner_id, d.site_id, d.customer_id,
        COALESCE(si.name, c.name) AS place_name, d.spec, d.revision, d.updated_at,
        u.username AS updated_by_username
    FROM dashboards d
    LEFT JOIN sites si ON si.id = d.site_id
    LEFT JOIN customers c ON c.id = d.customer_id
    LEFT JOIN users u ON u.id = d.updated_by";

/// The dashboards a user sees: its own, its sites', and its customers'
/// as a whole. Binds `$1` to `$5`.
const VISIBLE: &str = "(d.owner_id = $1
        OR (d.site_id IS NOT NULL AND ($2 OR d.site_id = ANY($3)))
        OR (d.customer_id IS NOT NULL AND ($4 OR d.customer_id = ANY($5))))";

fn may_see(user: &CurrentUser, owner: Option<Uuid>, sharing: Sharing) -> bool {
    user.can_read()
        && match sharing {
            Sharing::Personal => owner == Some(user.id),
            Sharing::Site { id } => user.readable_sites().contains(id),
            Sharing::Customer { id } => user.visible_customers().contains(id),
        }
}

/// Whether `user` may create, change or delete a dashboard so shared.
fn may_edit(user: &CurrentUser, owner: Option<Uuid>, sharing: Sharing) -> bool {
    user.can_read()
        && match sharing {
            Sharing::Personal => owner.is_none_or(|o| o == user.id),
            Sharing::Site { id } => user.modifiable_sites().contains(id),
            Sharing::Customer { id } => user.modifiable_customers().contains(id),
        }
}

fn summary(user: &CurrentUser, row: &Row, panels: usize) -> DashboardSummary {
    let sharing = row.sharing();
    DashboardSummary {
        id: row.id,
        name: row.name.clone(),
        shared_with: SharingView {
            sharing,
            name: row.place_name.clone(),
        },
        panels,
        updated_at: row.updated_at,
        updated_by_username: row.updated_by_username.clone(),
        can_edit: may_edit(user, row.owner_id, sharing),
    }
}

fn view(user: &CurrentUser, row: Row) -> Result<DashboardView, ApiError> {
    let spec = Spec::parse(row.spec.clone()).map_err(ApiError::Internal)?;
    Ok(DashboardView {
        summary: summary(user, &row, spec.panels.len()),
        revision: row.revision,
        spec,
    })
}

/// The dashboard `id` if `user` sees it; 404 otherwise.
async fn visible_row(
    db: impl PgExecutor<'_>,
    user: &CurrentUser,
    id: Uuid,
) -> Result<Row, ApiError> {
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    // SQL-safe: interpolates only the constant SELECT.
    let row: Row = sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} WHERE d.id = $1")))
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(ApiError::NotFound)?;
    if !may_see(user, row.owner_id, row.sharing()) {
        return Err(ApiError::NotFound);
    }
    Ok(row)
}

/// The sites, customers and servers the panels name.
fn named(spec: &Spec) -> (Vec<Uuid>, Vec<Uuid>, Vec<Uuid>) {
    let (mut sites, mut customers, mut servers) = (Vec::new(), Vec::new(), Vec::new());
    for panel in &spec.panels {
        match &panel.servers {
            Servers::All => {}
            Servers::Site { id } => sites.push(*id),
            Servers::Customer { id } => customers.push(*id),
            Servers::List { ids } => servers.extend(ids),
        }
    }
    for list in [&mut sites, &mut customers, &mut servers] {
        list.sort();
        list.dedup();
    }
    (sites, customers, servers)
}

/// Refuses a spec naming a site, customer or server that the editor
/// cannot read or that the sharing does not cover, with one message for
/// both so it reveals nothing.
async fn check_targets(
    db: &sqlx::PgPool,
    user: &CurrentUser,
    sharing: Sharing,
    spec: &Spec,
) -> Result<(), ApiError> {
    let (sites, customers, servers) = named(spec);
    // `(site, customer)` of each named site and of each named server's site.
    let site_rows: Vec<(Uuid, Uuid)> =
        sqlx::query_as("SELECT id, customer_id FROM sites WHERE id = ANY($1)")
            .bind(&sites)
            .fetch_all(db)
            .await?;
    let server_rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT s.site_id, si.customer_id FROM servers s
         JOIN sites si ON si.id = s.site_id WHERE s.id = ANY($1)",
    )
    .bind(&servers)
    .fetch_all(db)
    .await?;
    // Customers of which the user reads at least one site.
    let (all, readable) = user.readable_sites().bind();
    let customer_rows: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT customer_id FROM sites
         WHERE customer_id = ANY($1) AND ($2 OR id = ANY($3))",
    )
    .bind(&customers)
    .bind(all)
    .bind(&readable)
    .fetch_all(db)
    .await?;
    let place_ok = |site: Uuid, customer: Uuid| {
        user.readable_sites().contains(site)
            && match sharing {
                Sharing::Personal => true,
                Sharing::Site { id } => site == id,
                Sharing::Customer { id } => customer == id,
            }
    };
    let ok = site_rows.len() == sites.len()
        && server_rows.len() == servers.len()
        && customer_rows.len() == customers.len()
        && site_rows.iter().all(|&(s, c)| place_ok(s, c))
        && server_rows.iter().all(|&(s, c)| place_ok(s, c))
        && customers.iter().all(|&c| match sharing {
            Sharing::Personal => true,
            Sharing::Site { .. } => false,
            Sharing::Customer { id } => c == id,
        });
    if ok {
        Ok(())
    } else {
        Err(ApiError::Invalid(
            "A panel names a site, customer or server that does not exist, or that this dashboard's sharing does not cover.".into(),
        ))
    }
}

/// The name trimmed and the spec, or why not.
fn validate_request(request: &DashboardRequest) -> Result<(String, Spec), ApiError> {
    let name = request.name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME {
        return Err(ApiError::Invalid(format!(
            "Give the dashboard a name of up to {MAX_NAME} characters."
        )));
    }
    let spec = Spec::parse(request.spec.clone()).map_err(ApiError::Invalid)?;
    Ok((name.to_owned(), spec))
}

/// 404 for a site or customer the user cannot see, 403 for one it may
/// only read: the same answers as elsewhere.
fn authorize_sharing(user: &CurrentUser, sharing: Sharing) -> Result<(), ApiError> {
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let seen = match sharing {
        Sharing::Personal => true,
        Sharing::Site { id } => user.readable_sites().contains(id),
        // A customer with a readable site is known to the user; the
        // refusal is then 403 rather than 404.
        Sharing::Customer { id } => user.visible_customers().contains(id),
    };
    if !seen {
        return Err(ApiError::NotFound);
    }
    if !may_edit(user, None, sharing) {
        return Err(ApiError::Forbidden);
    }
    Ok(())
}

fn columns(user: &CurrentUser, sharing: Sharing) -> (Option<Uuid>, Option<Uuid>, Option<Uuid>) {
    match sharing {
        Sharing::Personal => (Some(user.id), None, None),
        Sharing::Site { id } => (None, Some(id), None),
        Sharing::Customer { id } => (None, None, Some(id)),
    }
}

async fn record(
    db: impl PgExecutor<'_>,
    user: &CurrentUser,
    action: &str,
    id: Uuid,
    name: &str,
    sharing: Sharing,
    panels: usize,
) -> Result<(), sqlx::Error> {
    let actor = user.id.to_string();
    audit::record(
        db,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action,
            target: Some(("dashboard", &id.to_string())),
            details: json!({ "name": name, "shared_with": sharing, "panels": panels }),
        },
    )
    .await
}

/// `GET /api/dashboards`: those the user sees, by name.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<DashboardList>, ApiError> {
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let (all_sites, sites) = user.readable_sites().bind();
    let (all_customers, customers) = user.visible_customers().bind();
    // SQL-safe: interpolates only the constants SELECT and VISIBLE.
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SELECT} WHERE {VISIBLE} ORDER BY lower(d.name), d.id"
    )))
    .bind(user.id)
    .bind(all_sites)
    .bind(&sites)
    .bind(all_customers)
    .bind(&customers)
    .fetch_all(&state.pool)
    .await?;
    let dashboards = rows
        .iter()
        .map(|row| {
            let panels = row.spec["panels"].as_array().map_or(0, Vec::len);
            summary(&user, row, panels)
        })
        .collect();
    Ok(Json(DashboardList { dashboards }))
}

/// `GET /api/dashboards/{id}`.
pub async fn detail(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<DashboardView>, ApiError> {
    let id = parse_id(&id)?;
    let row = visible_row(&state.pool, &user, id).await?;
    Ok(Json(view(&user, row)?))
}

/// `POST /api/dashboards`.
pub async fn create(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<DashboardRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<DashboardView>), ApiError> {
    let request = body(request)?;
    authorize_sharing(&user, request.shared_with)?;
    let (name, spec) = validate_request(&request)?;
    check_targets(&state.pool, &user, request.shared_with, &spec).await?;
    let (owner, site, customer) = columns(&user, request.shared_with);
    let mut tx = state.pool.begin().await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO dashboards (name, owner_id, site_id, customer_id, schema_version, spec,
                                 created_by, updated_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $7) RETURNING id",
    )
    .bind(&name)
    .bind(owner)
    .bind(site)
    .bind(customer)
    .bind(SCHEMA_VERSION as i32)
    .bind(serde_json::to_value(&spec).map_err(|e| ApiError::Internal(e.to_string()))?)
    .bind(user.id)
    .fetch_one(&mut *tx)
    .await?;
    let panels = spec.panels.len();
    record(
        &mut *tx,
        &user,
        "dashboard.create",
        id,
        &name,
        request.shared_with,
        panels,
    )
    .await?;
    let row = visible_row(&mut *tx, &user, id).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(view(&user, row)?)))
}

/// `PUT /api/dashboards/{id}`: replaces name, sharing and spec; 409 when
/// someone saved since `revision`.
pub async fn update(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<DashboardRequest>, JsonRejection>,
) -> Result<Json<DashboardView>, ApiError> {
    let id = parse_id(&id)?;
    let request = body(request)?;
    let current = visible_row(&state.pool, &user, id).await?;
    if !may_edit(&user, current.owner_id, current.sharing()) {
        return Err(ApiError::Forbidden);
    }
    if request.shared_with != current.sharing() {
        authorize_sharing(&user, request.shared_with)?;
    }
    let revision = request
        .revision
        .ok_or_else(|| ApiError::Invalid("Say which revision this edit started from.".into()))?;
    let (name, spec) = validate_request(&request)?;
    check_targets(&state.pool, &user, request.shared_with, &spec).await?;
    let (owner, site, customer) = columns(&user, request.shared_with);
    let mut tx = state.pool.begin().await?;
    let done = sqlx::query(
        "UPDATE dashboards SET name = $3, owner_id = $4, site_id = $5, customer_id = $6,
                schema_version = $7, spec = $8, revision = revision + 1,
                updated_by = $9, updated_at = now()
         WHERE id = $1 AND revision = $2",
    )
    .bind(id)
    .bind(revision)
    .bind(&name)
    .bind(owner)
    .bind(site)
    .bind(customer)
    .bind(SCHEMA_VERSION as i32)
    .bind(serde_json::to_value(&spec).map_err(|e| ApiError::Internal(e.to_string()))?)
    .bind(user.id)
    .execute(&mut *tx)
    .await?;
    if done.rows_affected() == 0 {
        return Err(ApiError::Conflict(
            "Someone changed this dashboard since you opened it. Reload it to see their changes."
                .into(),
        ));
    }
    let panels = spec.panels.len();
    record(
        &mut *tx,
        &user,
        "dashboard.update",
        id,
        &name,
        request.shared_with,
        panels,
    )
    .await?;
    let row = visible_row(&mut *tx, &user, id).await?;
    tx.commit().await?;
    Ok(Json(view(&user, row)?))
}

/// `DELETE /api/dashboards/{id}`.
pub async fn delete(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    let current = visible_row(&state.pool, &user, id).await?;
    let sharing = current.sharing();
    if !may_edit(&user, current.owner_id, sharing) {
        return Err(ApiError::Forbidden);
    }
    let mut tx = state.pool.begin().await?;
    sqlx::query("DELETE FROM dashboards WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let panels = current.spec["panels"].as_array().map_or(0, Vec::len);
    record(
        &mut *tx,
        &user,
        "dashboard.delete",
        id,
        &current.name,
        sharing,
        panels,
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel(value: Value) -> Result<(), String> {
        let spec = json!({ "version": 1, "range": "6h", "panels": [value] });
        Spec::parse(spec).map(|_| ())
    }

    #[test]
    fn panels_of_each_kind_validate() {
        let id = Uuid::from_u128(1);
        for ok in [
            json!({"id": "a", "title": "CPU", "kind": "chart", "metric": "monitorflow_cpu_usage_ratio",
                   "instance": "total", "combine": "each", "width": 2}),
            json!({"id": "a", "title": "Servers", "kind": "figure", "metric": "monitorflow_cpu_usage_ratio",
                   "combine": "count", "servers": {"kind": "site", "id": id}}),
            json!({"id": "a", "title": "Disks", "kind": "top", "metric": "monitorflow_disk_used_ratio",
                   "order": "highest", "count": 5, "servers": {"kind": "list", "ids": [id]}}),
            json!({"id": "a", "title": "Alerts", "kind": "alerts", "count": 10,
                   "servers": {"kind": "customer", "id": id}}),
        ] {
            assert_eq!(panel(ok.clone()), Ok(()), "{ok}");
        }
        let parsed: Panel =
            serde_json::from_value(json!({"id": "a", "title": "x", "kind": "alerts", "count": 1}))
                .unwrap();
        assert_eq!((parsed.width, parsed.servers), (1, Servers::All));
    }

    #[test]
    fn wrong_panels_are_refused_with_a_sentence() {
        let chart = |extra: Value| {
            let mut base = json!({"id": "a", "title": "CPU", "kind": "chart",
                                  "metric": "monitorflow_cpu_usage_ratio", "combine": "avg"});
            for (k, v) in extra.as_object().unwrap() {
                base[k] = v.clone();
            }
            base
        };
        for bad in [
            chart(json!({"title": " "})),
            chart(json!({"title": "x".repeat(81)})),
            chart(json!({"width": 4})),
            chart(json!({"combine": "count"})),
            chart(json!({"combine": null})),
            chart(json!({"count": 5})),
            chart(json!({"metric": "up"})),
            chart(json!({"metric": "sum(monitorflow_cpu_usage_ratio)"})),
            chart(json!({"metric": null})),
            chart(json!({"instance": "a\nb"})),
            chart(json!({"id": "a b"})),
            chart(json!({"query": "up"})),
            chart(json!({"kind": "gauge"})),
            chart(json!({"servers": {"kind": "list", "ids": []}})),
            chart(json!({"servers": {"kind": "list", "ids": vec![Uuid::nil(); 51]}})),
            chart(json!({"servers": {"kind": "tag", "name": "db"}})),
            chart(json!({"servers": {"kind": "site", "id": Uuid::nil(), "extra": 1}})),
            json!({"id": "a", "title": "x", "kind": "figure", "metric": "monitorflow_cpu_usage_ratio", "combine": "each"}),
            json!({"id": "a", "title": "x", "kind": "top", "metric": "monitorflow_cpu_usage_ratio", "count": 5}),
            json!({"id": "a", "title": "x", "kind": "top", "metric": "monitorflow_cpu_usage_ratio", "order": "lowest", "count": 21}),
            json!({"id": "a", "title": "x", "kind": "alerts", "count": 0}),
            json!({"id": "a", "title": "x", "kind": "alerts", "count": 5, "metric": "monitorflow_cpu_usage_ratio"}),
        ] {
            assert!(panel(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn specs_have_a_version_unique_ids_and_a_panel_limit() {
        let alert = |id: &str| json!({"id": id, "title": "x", "kind": "alerts", "count": 1});
        assert!(Spec::parse(json!({"version": 1, "range": "7d", "panels": []})).is_ok());
        assert!(Spec::parse(json!({"range": "7d", "panels": []})).is_err());
        assert!(Spec::parse(json!({"version": 0, "range": "7d", "panels": []})).is_err());
        let newer = Spec::parse(json!({"version": 2, "range": "7d", "panels": []})).unwrap_err();
        assert!(newer.contains("newer"), "{newer}");
        assert!(Spec::parse(json!({"version": 1, "range": "2h", "panels": []})).is_err());
        assert!(Spec::parse(json!({"version": 1, "range": "1h", "panels": [], "x": 1})).is_err());
        let twice = json!({"version": 1, "range": "1h", "panels": [alert("a"), alert("a")]});
        assert!(Spec::parse(twice).is_err());
        let many: Vec<Value> = (0..=MAX_PANELS).map(|i| alert(&i.to_string())).collect();
        assert!(Spec::parse(json!({"version": 1, "range": "1h", "panels": many})).is_err());
        // What is stored is what was sent, absent fields left out.
        let spec = json!({"version": 1, "range": "1h", "panels": [
            {"id": "a", "title": "x", "kind": "alerts", "count": 1, "width": 1, "servers": {"kind": "all"}}
        ]});
        assert_eq!(
            serde_json::to_value(Spec::parse(spec.clone()).unwrap()).unwrap(),
            spec
        );
    }

    #[test]
    fn ranges_match_the_sensor_pages() {
        for (range, seconds, step, rollup) in [
            ("1h", 3_600, 30, false),
            ("6h", 21_600, 120, false),
            ("24h", 86_400, 300, true),
            ("7d", 604_800, 1_800, true),
            ("30d", 2_592_000, 7_200, true),
        ] {
            let parsed: TimeRange = serde_json::from_value(json!(range)).unwrap();
            assert_eq!(
                (parsed.seconds(), parsed.step(), parsed.rolls_up()),
                (seconds, step, rollup)
            );
        }
    }
}
