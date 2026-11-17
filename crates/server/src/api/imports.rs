//! Moving from another monitoring tool (ADR-0146): the sources, a preview
//! from an upload or a connection, its confirmation and its undo.
//!
//! Administrators of the target site only. Nothing but the run is written
//! until the confirmation; a connection's token is used for the one
//! request and never stored or logged.

use std::collections::{BTreeMap, HashSet};

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use base64::Engine;
use chrono::{DateTime, TimeDelta, Utc};
use monitorflow_import::source::{self, Connection, File, Input, SourceInfo, MAX_HOSTS};
use monitorflow_import::{Context, ReadError};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::notifications::is_global_admin;
use super::roles::Role;
use super::sensors::{body, parse_id};
use super::{ApiError, ApiState};
use crate::import::store::{self, Outcome, Run, StoreError};
use crate::import::{self, Item, List, PREVIEW_HOURS};

/// Largest request body of a preview: files up to 32 MB, in base64.
pub const MAX_BODY: usize = 48 * 1024 * 1024;

/// A reader the server offers.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = ImportSource)]
pub struct SourceView {
    /// What `source` takes: `zabbix`, `prometheus`, …
    pub id: String,
    /// The product's name.
    pub name: String,
    /// File extensions an upload accepts, without the dot; empty when it
    /// reads no files.
    pub extensions: Vec<String>,
    /// Whether an upload takes several files at once.
    pub many_files: bool,
    /// The fields of `connection`; empty when it cannot connect.
    pub connect: Vec<FieldView>,
}

/// One field of a connection.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = ImportField)]
pub struct FieldView {
    pub key: String,
    /// A token or password: never stored, never shown back.
    pub secret: bool,
    pub optional: bool,
    pub placeholder: String,
}

impl From<SourceInfo> for SourceView {
    fn from(info: SourceInfo) -> Self {
        Self {
            id: info.id.into(),
            name: info.name.into(),
            extensions: info.extensions.iter().map(|e| (*e).to_owned()).collect(),
            many_files: info.many_files,
            connect: info
                .connect
                .into_iter()
                .map(|f| FieldView {
                    key: f.key.into(),
                    secret: f.secret,
                    optional: f.optional,
                    placeholder: f.placeholder.into(),
                })
                .collect(),
        }
    }
}

/// Body of `GET /api/imports/sources`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = ImportSourceList)]
pub struct SourceList {
    pub sources: Vec<SourceView>,
}

/// An uploaded file.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[schema(as = ImportFile)]
#[serde(deny_unknown_fields)]
pub struct FileRequest {
    /// Its name; readers use the extension and match names a
    /// configuration refers to.
    pub name: String,
    /// Its content: text, or base64 with `base64`.
    pub content: String,
    #[serde(default)]
    pub base64: bool,
}

/// Body of `POST /api/imports`: files or a connection.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[schema(as = ImportRequest)]
#[serde(deny_unknown_fields)]
pub struct PreviewRequest {
    /// The reader, from `GET /api/imports/sources`.
    pub source: String,
    /// The site servers go to; may be left out when the user administers
    /// one site only.
    #[serde(default)]
    pub site_id: Option<Uuid>,
    /// Files to read.
    #[serde(default)]
    pub files: Vec<FileRequest>,
    /// The connection's fields by key, as the source lists them. Used for
    /// this request only: never stored, never logged.
    #[serde(default)]
    pub connection: Option<BTreeMap<String, String>>,
}

/// Body of `POST /api/imports/{id}/confirm`.
#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
#[schema(as = ImportConfirm)]
#[serde(deny_unknown_fields)]
pub struct ConfirmRequest {
    /// IDs of items left out. Items that need one of them are left out
    /// too.
    #[serde(default)]
    pub skip: Vec<String>,
}

/// How many items fall in each list.
#[derive(Debug, Default, Serialize, utoipa::ToSchema)]
#[schema(as = ImportCounts)]
pub struct Counts {
    pub exact: u32,
    pub approximated: u32,
    pub not_movable: u32,
    /// Items MonitorFlow has already.
    pub present: u32,
    /// Items a confirmation with nothing left out would create.
    pub to_create: u32,
}

/// One import.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = Import)]
pub struct RunView {
    pub id: Uuid,
    pub source: String,
    /// The source's product name.
    pub source_name: String,
    pub site_id: Uuid,
    pub site_name: String,
    /// What was read: file names, or the address connected to.
    pub input: String,
    /// `preview`, `confirmed` or `undone`.
    pub state: String,
    pub created_at: DateTime<Utc>,
    /// Who read the source; `null` once the account is removed.
    pub created_by: Option<String>,
    /// Until when a preview can be confirmed.
    pub expires_at: Option<DateTime<Utc>>,
    pub confirmed_at: Option<DateTime<Utc>>,
    pub undone_at: Option<DateTime<Utc>>,
    pub counts: Counts,
    /// The items, in the order they are created; absent in lists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<Item>>,
    /// What the confirmation and the undo did.
    pub result: Option<Outcome>,
}

/// Body of `GET /api/imports`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = ImportList)]
pub struct RunList {
    /// The latest 50, newest first, without their items.
    pub imports: Vec<RunView>,
}

impl From<StoreError> for ApiError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Database(e) => e.into(),
            StoreError::State(s) => ApiError::Conflict(s),
            StoreError::Secrets(s) => ApiError::Internal(s),
        }
    }
}

fn counts(items: &[Item]) -> Counts {
    let mut counts = Counts::default();
    for item in items {
        match item.list {
            List::Exact => counts.exact += 1,
            List::Approximated => counts.approximated += 1,
            List::NotMovable => counts.not_movable += 1,
        }
        if item.present {
            counts.present += 1;
        }
        if item.creates() {
            counts.to_create += 1;
        }
    }
    counts
}

async fn view(state: &ApiState, run: Run, with_items: bool) -> Result<RunView, ApiError> {
    let (site_name, created_by): (String, Option<String>) = sqlx::query_as(
        "SELECT s.name, (SELECT username FROM users WHERE id = $2) FROM sites s WHERE s.id = $1",
    )
    .bind(run.site_id)
    .bind(run.created_by)
    .fetch_one(&state.pool)
    .await?;
    let source_name = source::source(&run.source)
        .map(|s| s.info().name.to_owned())
        .unwrap_or_else(|| run.source.clone());
    let items = run.items.0;
    Ok(RunView {
        id: run.id,
        source: run.source,
        source_name,
        site_id: run.site_id,
        site_name,
        input: run.input,
        expires_at: (run.state == "preview")
            .then(|| run.created_at + TimeDelta::hours(PREVIEW_HOURS)),
        state: run.state,
        created_at: run.created_at,
        created_by,
        confirmed_at: run.confirmed_at,
        undone_at: run.undone_at,
        counts: counts(&items),
        items: with_items.then_some(items),
        result: run.result.map(|r| r.0),
    })
}

/// Refuses who does not administer `site`: 404 when it cannot see it.
fn require_site_admin(user: &CurrentUser, site: Uuid) -> Result<(), ApiError> {
    if user.sites_with(Role::Administrator).contains(site) {
        Ok(())
    } else if user.readable_sites().contains(site) {
        Err(ApiError::Forbidden)
    } else {
        Err(ApiError::NotFound)
    }
}

/// `GET /api/imports/sources`: the readers, for any administrator.
pub async fn sources(user: CurrentUser) -> Result<Json<SourceList>, ApiError> {
    if !user.is_administrator() {
        return Err(ApiError::Forbidden);
    }
    Ok(Json(SourceList {
        sources: source::sources()
            .into_iter()
            .map(|s| s.info().into())
            .collect(),
    }))
}

/// The site of a preview: the one asked, or the user's only one.
async fn site_of(
    state: &ApiState,
    user: &CurrentUser,
    asked: Option<Uuid>,
) -> Result<Uuid, ApiError> {
    let site = match asked {
        Some(site) => site,
        None => {
            let (all, sites) = user.sites_with(Role::Administrator).bind();
            let ids: Vec<Uuid> = sqlx::query_scalar(
                "SELECT id FROM sites WHERE $1 OR id = ANY($2) ORDER BY name LIMIT 2",
            )
            .bind(all)
            .bind(&sites)
            .fetch_all(&state.pool)
            .await?;
            match ids.as_slice() {
                [one] => *one,
                [] => return Err(ApiError::Forbidden),
                _ => {
                    return Err(ApiError::Invalid(
                        "Choose the site the servers go to.".into(),
                    ))
                }
            }
        }
    };
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM sites WHERE id = $1)")
        .bind(site)
        .fetch_one(&state.pool)
        .await?;
    if !exists {
        return Err(ApiError::NotFound);
    }
    require_site_admin(user, site)?;
    Ok(site)
}

/// What a request reads, and how the run names it.
fn input_of(request: PreviewRequest) -> Result<(Input, String), ApiError> {
    match (request.files.is_empty(), request.connection) {
        (false, None) => {
            let names = request
                .files
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let files = request
                .files
                .into_iter()
                .map(|f| {
                    let content = if f.base64 {
                        base64::engine::general_purpose::STANDARD
                            .decode(f.content.trim())
                            .map_err(|_| {
                                ApiError::Invalid(format!("{}: not valid base64", f.name))
                            })?
                    } else {
                        f.content.into_bytes()
                    };
                    Ok(File {
                        name: f.name,
                        content,
                    })
                })
                .collect::<Result<_, ApiError>>()?;
            Ok((Input::Files(files), names))
        }
        (true, Some(fields)) => {
            // Only the address is kept, never a token.
            let shown = fields
                .get("url")
                .map(|u| {
                    u.trim()
                        .split(['?', '#'])
                        .next()
                        .unwrap_or_default()
                        .to_owned()
                })
                .unwrap_or_default();
            Ok((Input::Connect(Connection(fields)), shown))
        }
        _ => Err(ApiError::Invalid(
            "Give either files or a connection.".into(),
        )),
    }
}

/// `POST /api/imports`: reads the source and stores a preview; 201 with
/// it. Creates nothing else.
pub async fn preview(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<PreviewRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<RunView>), ApiError> {
    let request = body(request)?;
    let site = site_of(&state, &user, request.site_id).await?;
    let reader = source::source(&request.source)
        .ok_or_else(|| ApiError::Invalid(format!("Unknown source {}.", request.source)))?;
    let info = reader.info();
    let source_id = info.id;
    let (input, shown) = input_of(request)?;
    source::check_input(&info, &input).map_err(read_error)?;
    let now = Utc::now();
    let inventory = reader
        .read(input, &Context::new(now))
        .await
        .map_err(read_error)?;
    if inventory.hosts.len() > MAX_HOSTS {
        return Err(ApiError::Invalid(format!(
            "More than {MAX_HOSTS} hosts: import them in parts."
        )));
    }
    let administered = user.sites_with(Role::Administrator);
    let existing =
        store::existing(&state.pool, &inventory, site, |s| administered.contains(s)).await?;
    let planned = import::plan(&inventory, site, is_global_admin(&user), &existing, now);
    let id = store::create(
        &state.pool,
        source_id,
        site,
        &shown,
        &planned.items,
        &planned.secrets,
        user.id,
    )
    .await?;
    let counts = counts(&planned.items);
    audit::record(
        &state.pool,
        Event {
            actor_kind: "user",
            actor_id: &user.id.to_string(),
            action: "import.preview",
            target: Some(("import", &id.to_string())),
            details: json!({
                "source": source_id,
                "site_id": site,
                "input": shown,
                "hosts": inventory.hosts.len(),
                "exact": counts.exact,
                "approximated": counts.approximated,
                "not_movable": counts.not_movable,
            }),
        },
    )
    .await?;
    tracing::info!(user = %user.id, import = %id, source = source_id, items = planned.items.len(), "import previewed");
    let mut conn = state.pool.acquire().await?;
    let run = store::get(&mut conn, id, false)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok((StatusCode::CREATED, Json(view(&state, run, true).await?)))
}

fn read_error(error: ReadError) -> ApiError {
    ApiError::Invalid(error.to_string())
}

/// The run `id`, if the user administers its site.
async fn run_for(state: &ApiState, user: &CurrentUser, id: &str) -> Result<Run, ApiError> {
    let id = parse_id(id)?;
    let mut conn = state.pool.acquire().await?;
    let run = store::get(&mut conn, id, false)
        .await?
        .ok_or(ApiError::NotFound)?;
    if !user.sites_with(Role::Administrator).contains(run.site_id) {
        return Err(ApiError::NotFound);
    }
    Ok(run)
}

/// `GET /api/imports`: the latest imports of the sites the user
/// administers.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<RunList>, ApiError> {
    if !user.is_administrator() {
        return Err(ApiError::Forbidden);
    }
    let (all, sites) = user.sites_with(Role::Administrator).bind();
    let runs = store::list(&state.pool, all, &sites).await?;
    let mut imports = Vec::with_capacity(runs.len());
    for run in runs {
        imports.push(view(&state, run, false).await?);
    }
    Ok(Json(RunList { imports }))
}

/// `GET /api/imports/{id}`: one import with its items.
pub async fn detail(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<RunView>, ApiError> {
    let run = run_for(&state, &user, &id).await?;
    Ok(Json(view(&state, run, true).await?))
}

/// `POST /api/imports/{id}/confirm`: creates what the preview planned,
/// but what `skip` names.
pub async fn confirm(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: axum::body::Bytes,
) -> Result<Json<RunView>, ApiError> {
    let run = run_for(&state, &user, &id).await?;
    // The body may be left out: nothing is skipped.
    let request: ConfirmRequest = if request.iter().all(u8::is_ascii_whitespace) {
        ConfirmRequest::default()
    } else {
        serde_json::from_slice(&request).map_err(|e| ApiError::Invalid(e.to_string()))?
    };
    let skip: HashSet<String> = request.skip.into_iter().collect();
    let (run, outcome) = store::confirm(
        &state.pool,
        run.id,
        &skip,
        user.id,
        !is_global_admin(&user),
        Utc::now(),
    )
    .await?;
    tracing::info!(user = %user.id, import = %run.id, created = ?outcome.created, failed = outcome.failed.len(), "import confirmed");
    Ok(Json(view(&state, run, true).await?))
}

/// `POST /api/imports/{id}/undo`: removes what the import created.
pub async fn undo(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<RunView>, ApiError> {
    let run = run_for(&state, &user, &id).await?;
    let (run, outcome) = store::undo(&state.pool, run.id, user.id).await?;
    tracing::info!(user = %user.id, import = %run.id, removed = ?outcome.removed, "import undone");
    Ok(Json(view(&state, run, true).await?))
}
