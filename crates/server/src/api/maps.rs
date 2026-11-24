//! Network maps (ADR-0185): maps of a site or a customer, what they show
//! now, their background image, and reading devices' neighbours.
//!
//! Who sees and edits a map follows the roles, as for a dashboard shared
//! with the same place (ADR-0095): readers of the site, or of the
//! customer as a whole, see it; operators change it. What a map shows is
//! resolved for the viewer on every read, so a customer's map shows a
//! site's viewer nothing of the other sites, whatever it stores.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::IpAddr;
use std::sync::Arc;

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::Engine;
use chrono::{DateTime, Utc};
use monitorflow_common::tls::PeerRole;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgExecutor;
use uuid::Uuid;

use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::dashboard_data::{regex_escape, Within};
use super::sensors::{body, parse_id};
use super::servers::ServerState;
use super::{ApiError, ApiState};
use crate::dependencies::Node;
use crate::maps::{
    self, neighbors, BackgroundPlacement, InterfaceRef, Label, LinkState, MapDocument, NodeKey,
    NodeState, Traffic, Zone,
};

/// Most nodes one map shows; the rest are left out and `truncated` says so.
pub const MAX_NODES: usize = 2_000;
/// Most addresses from scans, and most neighbours that are nothing
/// monitored, one map shows.
pub const MAX_EXTRA: usize = 500;
/// Largest background image, in bytes.
pub const MAX_BACKGROUND: usize = 1 << 20;
/// Most devices one read of neighbours asks.
pub const MAX_DEVICES: usize = 50;
/// Devices asked at once.
const PARALLEL: usize = 8;

/// Where a map belongs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = MapPlace)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Place {
    Site { id: Uuid },
    Customer { id: Uuid },
}

/// [`Place`] with its name.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[schema(as = MapPlaceView)]
pub struct PlaceView {
    #[serde(flatten)]
    pub place: Place,
    /// The site's or customer's name.
    pub name: String,
}

/// One row of `GET /api/maps`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct MapSummary {
    pub id: Uuid,
    pub name: String,
    pub place: PlaceView,
    pub updated_at: DateTime<Utc>,
    /// `null` once the user is deleted.
    pub updated_by_username: Option<String>,
    /// When its background image was set; `null` without one.
    pub background_updated_at: Option<DateTime<Utc>>,
    /// Whether the caller may change and delete it.
    pub can_edit: bool,
}

/// Body of `GET /api/maps`, by name.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct MapList {
    pub maps: Vec<MapSummary>,
}

/// `GET /api/maps/{id}` and the answer to writes.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct MapView {
    #[serde(flatten)]
    pub summary: MapSummary,
    pub document: MapDocument,
    /// Pass back on `PUT`.
    pub revision: i64,
}

/// Body of `POST /api/maps` and `PUT /api/maps/{id}`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MapRequest {
    /// 1 to 80 characters.
    pub name: String,
    pub place: Place,
    /// What was placed by hand; an empty map when absent.
    #[serde(default)]
    pub document: Option<MapDocument>,
    /// The revision the edit started from; `PUT` only.
    #[serde(default)]
    pub revision: Option<i64>,
}

/// Query string of `GET /api/maps`.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ListParams {
    /// Only the maps of this site.
    pub site_id: Option<Uuid>,
    /// Only the maps of this customer and of its sites.
    pub customer_id: Option<Uuid>,
}

/// What a node is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[schema(as = MapNodeKind)]
#[serde(rename_all = "lowercase")]
pub enum NodeKind {
    /// The central server.
    Central,
    /// A server with the agent.
    Server,
    /// A device watched by a probe (ADR-0148).
    Device,
    Relay,
    /// An address a network scan found that nothing watches.
    Host,
    /// A neighbour a device reported that nothing on the map is.
    Neighbor,
}

/// One node as the map shows it now.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[schema(as = MapNode)]
pub struct LiveNode {
    /// What a placement, a link or a click names it by.
    pub key: String,
    pub kind: NodeKind,
    pub name: String,
    /// `null` for the central server and neighbours.
    pub site_id: Option<Uuid>,
    pub state: NodeState,
    /// A server's or device's ID: its page.
    pub server_id: Option<Uuid>,
    /// A relay's ID: its page.
    pub relay_id: Option<Uuid>,
    /// A device's or found host's address.
    pub address: Option<String>,
    /// A device's kind: `switch`, `router`, `firewall`, `printer`, `ups`,
    /// `nas` or `generic`.
    pub template: Option<String>,
    /// `debian`, `rhel`, `windows`, `linux` or `device`.
    pub os_family: Option<String>,
    /// Where it was placed; `null` lets the client lay it out.
    pub x: Option<f64>,
    pub y: Option<f64>,
    /// Left off the map by hand.
    pub hidden: bool,
}

/// Where a link comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[schema(as = MapLinkSource)]
#[serde(rename_all = "lowercase")]
pub enum LinkSource {
    /// A relay and the central server, or what the relay carries or
    /// checks.
    Relay,
    /// A dependency (ADR-0152), from the parent to the child.
    Dependency,
    /// LLDP or CDP, as a device reported it.
    Neighbor,
    /// Drawn by hand.
    Manual,
}

/// One link as the map shows it now.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[schema(as = MapLiveLink)]
pub struct LiveLink {
    /// A hand-drawn link's id, else made of its ends.
    pub id: String,
    /// Key of one end: the parent of a dependency or relay link.
    pub from: String,
    pub to: String,
    pub source: LinkSource,
    /// As bad as the worse of its ends and of its interface.
    pub state: LinkState,
    pub label: Option<String>,
    /// The port at `from`, when a neighbour said it.
    pub from_port: Option<String>,
    /// The port at `to`.
    pub to_port: Option<String>,
    /// The interface whose traffic it shows.
    pub interface: Option<InterfaceRef>,
    /// That interface's traffic now; `null` without one or without data.
    pub traffic: Option<Traffic>,
}

/// The background of a map as it shows now.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[schema(as = MapLiveBackground)]
pub struct LiveBackground {
    /// Changes when the image does; read the image again then.
    pub updated_at: DateTime<Utc>,
    /// Where it lies; `null` from the origin at the image's own size.
    pub placement: Option<BackgroundPlacement>,
}

/// `GET /api/maps/{id}/live`, and a map panel's data: everything needed
/// to draw the map now.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct MapLive {
    pub id: Uuid,
    pub name: String,
    /// Whether to draw a zone around each site's nodes.
    pub group_by_site: bool,
    /// The sites of its nodes, for those zones.
    pub sites: Vec<MapSite>,
    pub nodes: Vec<LiveNode>,
    pub links: Vec<LiveLink>,
    pub zones: Vec<Zone>,
    pub labels: Vec<Label>,
    pub background: Option<LiveBackground>,
    /// When a device's neighbours were last read; `null` when never.
    pub neighbors_read_at: Option<DateTime<Utc>>,
    /// More than 2,000 nodes: the rest are left out.
    pub truncated: bool,
    /// The server's clock, unix seconds (ADR-0099).
    pub now: i64,
}

/// A site of a map's nodes.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct MapSite {
    pub id: Uuid,
    pub name: String,
}

/// Body of `PUT /api/maps/{id}/background`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BackgroundRequest {
    /// A PNG, JPEG or WebP image of at most 1 MiB, in base64.
    pub image: String,
}

/// A device whose neighbours could not be read.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct NeighborFailure {
    pub server_id: Uuid,
    pub name: String,
    pub error: String,
}

/// Body of `POST /api/maps/{id}/neighbors`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct NeighborRead {
    /// Devices asked: the SNMP devices of the map's place the caller may
    /// change, at most 50.
    pub devices: usize,
    /// Neighbours they reported.
    pub neighbors: usize,
    pub failed: Vec<NeighborFailure>,
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct Row {
    id: Uuid,
    name: String,
    site_id: Option<Uuid>,
    customer_id: Option<Uuid>,
    place_name: String,
    document: Value,
    revision: i64,
    updated_at: DateTime<Utc>,
    updated_by_username: Option<String>,
    background_updated_at: Option<DateTime<Utc>>,
}

impl Row {
    fn place(&self) -> Place {
        match (self.site_id, self.customer_id) {
            (Some(id), _) => Place::Site { id },
            (None, Some(id)) => Place::Customer { id },
            // The table's check makes one of them set.
            (None, None) => Place::Customer { id: Uuid::nil() },
        }
    }

    fn document(&self) -> Result<MapDocument, ApiError> {
        serde_json::from_value(self.document.clone()).map_err(|e| ApiError::Internal(e.to_string()))
    }
}

const SELECT: &str = "SELECT m.id, m.name, m.site_id, m.customer_id,
        COALESCE(si.name, c.name, '') AS place_name, m.document, m.revision, m.updated_at,
        u.username AS updated_by_username, b.updated_at AS background_updated_at
    FROM network_maps m
    LEFT JOIN sites si ON si.id = m.site_id
    LEFT JOIN customers c ON c.id = m.customer_id
    LEFT JOIN users u ON u.id = m.updated_by
    LEFT JOIN network_map_backgrounds b ON b.map_id = m.id";

/// Whether `user` sees maps of `place`.
pub(super) fn may_see(user: &CurrentUser, place: Place) -> bool {
    user.can_read()
        && match place {
            Place::Site { id } => user.readable_sites().contains(id),
            Place::Customer { id } => user.visible_customers().contains(id),
        }
}

/// Whether `user` may create, change or delete maps of `place`.
fn may_edit(user: &CurrentUser, place: Place) -> bool {
    user.can_read()
        && match place {
            Place::Site { id } => user.modifiable_sites().contains(id),
            Place::Customer { id } => user.modifiable_customers().contains(id),
        }
}

/// 404 for a place the user cannot see, 403 for one it may only read.
fn authorize_place(user: &CurrentUser, place: Place) -> Result<(), ApiError> {
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    if !may_see(user, place) {
        return Err(ApiError::NotFound);
    }
    if !may_edit(user, place) {
        return Err(ApiError::Forbidden);
    }
    Ok(())
}

fn summary(user: &CurrentUser, row: &Row) -> MapSummary {
    let place = row.place();
    MapSummary {
        id: row.id,
        name: row.name.clone(),
        place: PlaceView {
            place,
            name: row.place_name.clone(),
        },
        updated_at: row.updated_at,
        updated_by_username: row.updated_by_username.clone(),
        background_updated_at: row.background_updated_at,
        can_edit: may_edit(user, place),
    }
}

fn view(user: &CurrentUser, row: Row) -> Result<MapView, ApiError> {
    Ok(MapView {
        summary: summary(user, &row),
        document: row.document()?,
        revision: row.revision,
    })
}

/// The map `id` if `user` sees it; 404 otherwise.
pub(super) async fn visible_row(
    db: impl PgExecutor<'_>,
    user: &CurrentUser,
    id: Uuid,
) -> Result<Row, ApiError> {
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    // SQL-safe: interpolates only the constant SELECT.
    let row: Row = sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} WHERE m.id = $1")))
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(ApiError::NotFound)?;
    if !may_see(user, row.place()) {
        return Err(ApiError::NotFound);
    }
    Ok(row)
}

/// The place of the map `id`, without access checks; `None` when it does
/// not exist.
pub(super) async fn place_of(
    db: impl PgExecutor<'_>,
    id: Uuid,
) -> Result<Option<(Place, Uuid)>, sqlx::Error> {
    let row: Option<(Option<Uuid>, Option<Uuid>, Uuid)> = sqlx::query_as(
        "SELECT m.site_id, m.customer_id, COALESCE(si.customer_id, m.customer_id)
         FROM network_maps m LEFT JOIN sites si ON si.id = m.site_id WHERE m.id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|(site, customer, owner)| {
        let place = match (site, customer) {
            (Some(id), _) => Place::Site { id },
            (None, customer) => Place::Customer {
                id: customer.unwrap_or(owner),
            },
        };
        (place, owner)
    }))
}

fn validate(request: &MapRequest) -> Result<(String, MapDocument), ApiError> {
    let name = request.name.trim();
    if name.is_empty() || name.chars().count() > maps::MAX_NAME {
        return Err(ApiError::Invalid(format!(
            "Give the map a name of up to {} characters.",
            maps::MAX_NAME
        )));
    }
    let document = request.document.clone().unwrap_or_default();
    maps::check(&document).map_err(ApiError::Invalid)?;
    Ok((name.to_owned(), document))
}

/// Refuses a hand-drawn link whose interface belongs to a server the
/// editor cannot read or outside the map's place, with one message for
/// both so it reveals nothing.
async fn check_interfaces(
    db: &sqlx::PgPool,
    user: &CurrentUser,
    place: Place,
    document: &MapDocument,
) -> Result<(), ApiError> {
    let servers: BTreeSet<Uuid> = document
        .links
        .iter()
        .filter_map(|l| l.interface.as_ref().map(|i| i.server_id))
        .collect();
    if servers.is_empty() {
        return Ok(());
    }
    let ids: Vec<Uuid> = servers.iter().copied().collect();
    let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT s.site_id, si.customer_id FROM servers s
         JOIN sites si ON si.id = s.site_id WHERE s.id = ANY($1)",
    )
    .bind(&ids)
    .fetch_all(db)
    .await?;
    let ok = rows.len() == ids.len()
        && rows.iter().all(|&(site, customer)| {
            user.readable_sites().contains(site)
                && match place {
                    Place::Site { id } => site == id,
                    Place::Customer { id } => customer == id,
                }
        });
    if ok {
        Ok(())
    } else {
        Err(ApiError::Invalid(
            "A link shows the interface of a server that does not exist or is not on this map's site or customer.".into(),
        ))
    }
}

fn columns(place: Place) -> (Option<Uuid>, Option<Uuid>) {
    match place {
        Place::Site { id } => (Some(id), None),
        Place::Customer { id } => (None, Some(id)),
    }
}

async fn record(
    db: impl PgExecutor<'_>,
    user: &CurrentUser,
    action: &str,
    id: Uuid,
    details: Value,
) -> Result<(), sqlx::Error> {
    let actor = user.id.to_string();
    audit::record(
        db,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action,
            target: Some(("network_map", &id.to_string())),
            details,
        },
    )
    .await
}

fn details(name: &str, place: Place, document: &MapDocument) -> Value {
    json!({
        "name": name,
        "place": place,
        "placed": document.nodes.len(),
        "zones": document.zones.len(),
        "links": document.links.len(),
        "labels": document.labels.len(),
    })
}

/// `GET /api/maps`: those the user sees, by name.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
    params: Result<Query<ListParams>, QueryRejection>,
) -> Result<Json<MapList>, ApiError> {
    let Query(params) = params.map_err(|r| ApiError::Invalid(r.body_text()))?;
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    if let Some(site) = params.site_id {
        if !user.readable_sites().contains(site) {
            return Err(ApiError::NotFound);
        }
    }
    let (all_sites, sites) = user.readable_sites().bind();
    let (all_customers, customers) = user.visible_customers().bind();
    // SQL-safe: interpolates only the constant SELECT.
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SELECT}
         WHERE ((m.site_id IS NOT NULL AND ($1 OR m.site_id = ANY($2)))
             OR (m.customer_id IS NOT NULL AND ($3 OR m.customer_id = ANY($4))))
           AND ($5::uuid IS NULL OR m.site_id = $5)
           AND ($6::uuid IS NULL OR m.customer_id = $6 OR si.customer_id = $6)
         ORDER BY lower(m.name), m.id"
    )))
    .bind(all_sites)
    .bind(&sites)
    .bind(all_customers)
    .bind(&customers)
    .bind(params.site_id)
    .bind(params.customer_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(MapList {
        maps: rows.iter().map(|row| summary(&user, row)).collect(),
    }))
}

/// `GET /api/maps/{id}`.
pub async fn get(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<MapView>, ApiError> {
    let id = parse_id(&id)?;
    let row = visible_row(&state.pool, &user, id).await?;
    Ok(Json(view(&user, row)?))
}

/// `POST /api/maps`.
pub async fn create(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<MapRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<MapView>), ApiError> {
    let request = body(request)?;
    authorize_place(&user, request.place)?;
    let (name, document) = validate(&request)?;
    check_interfaces(&state.pool, &user, request.place, &document).await?;
    let (site, customer) = columns(request.place);
    let mut tx = state.pool.begin().await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO network_maps (name, site_id, customer_id, document, created_by, updated_by)
         VALUES ($1, $2, $3, $4, $5, $5) RETURNING id",
    )
    .bind(&name)
    .bind(site)
    .bind(customer)
    .bind(serde_json::to_value(&document).map_err(|e| ApiError::Internal(e.to_string()))?)
    .bind(user.id)
    .fetch_one(&mut *tx)
    .await?;
    record(
        &mut *tx,
        &user,
        "network_map.create",
        id,
        details(&name, request.place, &document),
    )
    .await?;
    let row = visible_row(&mut *tx, &user, id).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(view(&user, row)?)))
}

/// `PUT /api/maps/{id}`: replaces name, place and document; 409 when
/// someone saved since `revision`.
pub async fn update(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<MapRequest>, JsonRejection>,
) -> Result<Json<MapView>, ApiError> {
    let id = parse_id(&id)?;
    let request = body(request)?;
    let current = visible_row(&state.pool, &user, id).await?;
    if !may_edit(&user, current.place()) {
        return Err(ApiError::Forbidden);
    }
    if request.place != current.place() {
        authorize_place(&user, request.place)?;
    }
    let revision = request
        .revision
        .ok_or_else(|| ApiError::Invalid("Say which revision this edit started from.".into()))?;
    let (name, document) = validate(&request)?;
    check_interfaces(&state.pool, &user, request.place, &document).await?;
    let (site, customer) = columns(request.place);
    let mut tx = state.pool.begin().await?;
    let done = sqlx::query(
        "UPDATE network_maps SET name = $3, site_id = $4, customer_id = $5, document = $6,
                revision = revision + 1, updated_by = $7, updated_at = now()
         WHERE id = $1 AND revision = $2",
    )
    .bind(id)
    .bind(revision)
    .bind(&name)
    .bind(site)
    .bind(customer)
    .bind(serde_json::to_value(&document).map_err(|e| ApiError::Internal(e.to_string()))?)
    .bind(user.id)
    .execute(&mut *tx)
    .await?;
    if done.rows_affected() == 0 {
        return Err(ApiError::Conflict(
            "Someone changed this map since you opened it. Reload it to see their changes.".into(),
        ));
    }
    record(
        &mut *tx,
        &user,
        "network_map.update",
        id,
        details(&name, request.place, &document),
    )
    .await?;
    let row = visible_row(&mut *tx, &user, id).await?;
    tx.commit().await?;
    Ok(Json(view(&user, row)?))
}

/// `DELETE /api/maps/{id}`. Dashboard panels showing it say it is gone.
pub async fn delete(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    let current = visible_row(&state.pool, &user, id).await?;
    if !may_edit(&user, current.place()) {
        return Err(ApiError::Forbidden);
    }
    let mut tx = state.pool.begin().await?;
    sqlx::query("DELETE FROM network_maps WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    record(
        &mut *tx,
        &user,
        "network_map.delete",
        id,
        json!({ "name": current.name, "place": current.place() }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The image type of `bytes` by its first bytes: PNG, JPEG or WebP.
pub fn image_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// `PUT /api/maps/{id}/background`: sets the image under the map.
pub async fn set_background(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<BackgroundRequest>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    let request = body(request)?;
    let current = visible_row(&state.pool, &user, id).await?;
    if !may_edit(&user, current.place()) {
        return Err(ApiError::Forbidden);
    }
    let image = base64::engine::general_purpose::STANDARD
        .decode(request.image.trim())
        .map_err(|_| ApiError::Invalid("`image` must be base64.".into()))?;
    if image.len() > MAX_BACKGROUND {
        return Err(ApiError::Invalid(
            "The background image is at most 1 MiB.".into(),
        ));
    }
    let content_type = image_type(&image)
        .ok_or_else(|| ApiError::Invalid("The background is a PNG, JPEG or WebP image.".into()))?;
    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "INSERT INTO network_map_backgrounds (map_id, content_type, image) VALUES ($1, $2, $3)
         ON CONFLICT (map_id) DO UPDATE SET content_type = $2, image = $3, updated_at = now()",
    )
    .bind(id)
    .bind(content_type)
    .bind(&image)
    .execute(&mut *tx)
    .await?;
    record(
        &mut *tx,
        &user,
        "network_map.background",
        id,
        json!({ "name": current.name, "content_type": content_type, "bytes": image.len() }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/maps/{id}/background`.
pub async fn remove_background(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    let current = visible_row(&state.pool, &user, id).await?;
    if !may_edit(&user, current.place()) {
        return Err(ApiError::Forbidden);
    }
    let mut tx = state.pool.begin().await?;
    let done = sqlx::query("DELETE FROM network_map_backgrounds WHERE map_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    if done.rows_affected() > 0 {
        record(
            &mut *tx,
            &user,
            "network_map.background",
            id,
            json!({ "name": current.name, "removed": true }),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The background of the map `id` as an answer; 404 without one.
pub(super) async fn background_response(
    db: impl PgExecutor<'_>,
    id: Uuid,
) -> Result<Response, ApiError> {
    let row: Option<(String, Vec<u8>)> =
        sqlx::query_as("SELECT content_type, image FROM network_map_backgrounds WHERE map_id = $1")
            .bind(id)
            .fetch_optional(db)
            .await?;
    let (content_type, image) = row.ok_or(ApiError::NotFound)?;
    let mut response = image.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&content_type).map_err(|e| ApiError::Internal(e.to_string()))?,
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=300"),
    );
    Ok(response)
}

/// `GET /api/maps/{id}/background`: the image, as it was given.
pub async fn background(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let id = parse_id(&id)?;
    visible_row(&state.pool, &user, id).await?;
    background_response(&state.pool, id).await
}

/// `GET /api/maps/{id}/live`: what the map shows now.
pub async fn live(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<MapLive>, ApiError> {
    let id = parse_id(&id)?;
    let row = visible_row(&state.pool, &user, id).await?;
    Ok(Json(compute(&state, &user, &row, Within::All).await?))
}

/// What a dashboard's map panel shows of the map `id` to `user`, within
/// the dashboard's sharing; an error sentence when it cannot.
pub(super) async fn panel(
    state: &ApiState,
    user: &CurrentUser,
    id: Uuid,
    within: Within,
) -> Result<Result<MapLive, String>, ApiError> {
    let row = match visible_row(&state.pool, user, id).await {
        Ok(row) => row,
        Err(ApiError::NotFound | ApiError::Forbidden) => {
            return Ok(Err("This map was deleted, or you may not see it.".into()))
        }
        Err(other) => return Err(other),
    };
    Ok(Ok(compute(state, user, &row, within).await?))
}

fn node_state(state: ServerState) -> NodeState {
    match state {
        ServerState::Critical => NodeState::Critical,
        ServerState::Warning => NodeState::Warning,
        ServerState::Acknowledged => NodeState::Acknowledged,
        ServerState::Silenced => NodeState::Silenced,
        ServerState::Offline => NodeState::Offline,
        ServerState::Ok => NodeState::Ok,
        ServerState::Waiting => NodeState::Waiting,
        ServerState::Unreachable => NodeState::Unreachable,
    }
}

#[derive(sqlx::FromRow)]
struct ServerRow {
    id: Uuid,
    peer_id: String,
    hostname: String,
    kind: String,
    address: Option<String>,
    os_family: String,
    device_template: Option<String>,
    site_id: Uuid,
}

/// The links of a map while it is computed, one per pair of ends.
#[derive(Default)]
struct Links {
    list: Vec<LiveLink>,
    by_pair: HashMap<(String, String), usize>,
}

impl Links {
    fn pair(a: &str, b: &str) -> (String, String) {
        if a <= b {
            (a.to_owned(), b.to_owned())
        } else {
            (b.to_owned(), a.to_owned())
        }
    }

    /// Adds a link, or the ports and interface of `link` to the one
    /// already between its ends.
    fn add(&mut self, link: LiveLink) {
        match self.by_pair.get(&Self::pair(&link.from, &link.to)) {
            Some(&i) => {
                let existing = &mut self.list[i];
                let same_way = existing.from == link.from;
                let (from_port, to_port) = if same_way {
                    (link.from_port, link.to_port)
                } else {
                    (link.to_port, link.from_port)
                };
                existing.from_port = existing.from_port.take().or(from_port);
                existing.to_port = existing.to_port.take().or(to_port);
                existing.interface = existing.interface.take().or(link.interface);
            }
            None => {
                self.by_pair
                    .insert(Self::pair(&link.from, &link.to), self.list.len());
                self.list.push(link);
            }
        }
    }
}

fn link(from: &NodeKey, to: &NodeKey, source: LinkSource) -> LiveLink {
    LiveLink {
        id: format!("{from}|{to}"),
        from: from.to_string(),
        to: to.to_string(),
        source,
        state: LinkState::Unknown,
        label: None,
        from_port: None,
        to_port: None,
        interface: None,
        traffic: None,
    }
}

fn node(key: &NodeKey, kind: NodeKind, name: String, state: NodeState) -> LiveNode {
    LiveNode {
        key: key.to_string(),
        kind,
        name,
        site_id: None,
        state,
        server_id: None,
        relay_id: None,
        address: None,
        template: None,
        os_family: None,
        x: None,
        y: None,
        hidden: false,
    }
}

/// Everything the map `row` shows to `user` now, within `within`.
pub(super) async fn compute(
    state: &ApiState,
    user: &CurrentUser,
    row: &Row,
    within: Within,
) -> Result<MapLive, ApiError> {
    let document = row.document()?;
    let sources = document.sources;
    let mut conn = state.pool.acquire().await?;
    let (place_site, place_customer) = columns(row.place());

    // The map's sites the viewer reads, within the dashboard's sharing.
    let sites: Vec<(Uuid, String, Uuid)> = sqlx::query_as(
        "SELECT id, name, customer_id FROM sites
         WHERE ($1::uuid IS NULL OR id = $1) AND ($2::uuid IS NULL OR customer_id = $2)
         ORDER BY name, id",
    )
    .bind(place_site)
    .bind(place_customer)
    .fetch_all(&mut *conn)
    .await?;
    let readable = user.readable_sites();
    let sites: Vec<(Uuid, String)> = sites
        .into_iter()
        .filter(|(id, _, customer)| {
            readable.contains(*id)
                && match within {
                    Within::All => true,
                    Within::Site { id: site } => *id == site,
                    Within::Customer { id: c } => *customer == c,
                }
        })
        .map(|(id, name, _)| (id, name))
        .collect();
    let site_ids: Vec<Uuid> = sites.iter().map(|(id, _)| *id).collect();

    let mut servers: Vec<ServerRow> = sqlx::query_as(
        "SELECT id, peer_id, hostname, kind, address, os_family, device_template, site_id
         FROM servers WHERE site_id = ANY($1) AND revoked_at IS NULL
         ORDER BY hostname, id LIMIT $2",
    )
    .bind(&site_ids)
    .bind((MAX_NODES + 1) as i64)
    .fetch_all(&mut *conn)
    .await?;
    let relays: Vec<(Uuid, String, String, Uuid)> = sqlx::query_as(
        "SELECT id, peer_id, name, site_id FROM relays
         WHERE site_id = ANY($1) AND revoked_at IS NULL ORDER BY name, id",
    )
    .bind(&site_ids)
    .fetch_all(&mut *conn)
    .await?;
    let mut truncated = servers.len() + relays.len() > MAX_NODES;
    servers.truncate(MAX_NODES.saturating_sub(relays.len()));

    let ids: Vec<Uuid> = servers.iter().map(|s| s.id).collect();
    drop(conn);
    let states = super::servers::states_of(state, &ids).await?;
    let mut conn = state.pool.acquire().await?;

    let mut nodes: BTreeMap<NodeKey, LiveNode> = BTreeMap::new();
    for server in &servers {
        let key = NodeKey::Server(server.id);
        let device = server.kind == "device";
        let mut live = node(
            &key,
            if device {
                NodeKind::Device
            } else {
                NodeKind::Server
            },
            server.hostname.clone(),
            states
                .get(&server.id)
                .copied()
                .map_or(NodeState::Unknown, node_state),
        );
        live.site_id = Some(server.site_id);
        live.server_id = Some(server.id);
        live.address = server.address.clone();
        live.template = server.device_template.clone();
        live.os_family = Some(server.os_family.clone());
        nodes.insert(key, live);
    }
    for (id, peer, name, site) in &relays {
        let key = NodeKey::Relay(*id);
        let online = state.registry.live(PeerRole::Relay, peer).is_some();
        let mut live = node(
            &key,
            NodeKind::Relay,
            name.clone(),
            if online {
                NodeState::Ok
            } else {
                NodeState::Offline
            },
        );
        live.site_id = Some(*site);
        live.relay_id = Some(*id);
        nodes.insert(key, live);
    }

    let mut links = Links::default();
    if sources.relays && !relays.is_empty() {
        nodes.insert(
            NodeKey::Central,
            node(
                &NodeKey::Central,
                NodeKind::Central,
                "MonitorFlow".into(),
                NodeState::Ok,
            ),
        );
        for (id, ..) in &relays {
            links.add(link(
                &NodeKey::Central,
                &NodeKey::Relay(*id),
                LinkSource::Relay,
            ));
        }
    }
    if sources.relays || sources.dependencies {
        let key = |n: &Node| match n {
            Node::Server(id) | Node::Check(id, ..) => NodeKey::Server(*id),
            Node::Relay(id) => NodeKey::Relay(*id),
        };
        for edge in crate::dependencies::store::edges(&mut conn).await? {
            let source = if edge.id.is_some() {
                LinkSource::Dependency
            } else {
                LinkSource::Relay
            };
            let wanted = match source {
                LinkSource::Dependency => sources.dependencies,
                _ => sources.relays,
            };
            let (parent, child) = (key(&edge.parent), key(&edge.child));
            if wanted
                && parent != child
                && nodes.contains_key(&parent)
                && nodes.contains_key(&child)
            {
                links.add(link(&parent, &child, source));
            }
        }
    }

    let devices: Vec<Uuid> = servers
        .iter()
        .filter(|s| s.kind == "device")
        .map(|s| s.id)
        .collect();
    let mut neighbors_read_at = None;
    if sources.neighbors && !devices.is_empty() {
        neighbors_read_at = sqlx::query_scalar(
            "SELECT max(read_at) FROM network_neighbor_reads WHERE server_id = ANY($1)",
        )
        .bind(&devices)
        .fetch_one(&mut *conn)
        .await?;
        type Seen = (Uuid, String, String, String, Option<String>);
        let seen: Vec<Seen> = sqlx::query_as(
            "SELECT server_id, local_port, remote_name, remote_port, host(remote_address)
             FROM network_neighbors WHERE server_id = ANY($1)
             ORDER BY server_id, local_port, remote_name",
        )
        .bind(&devices)
        .fetch_all(&mut *conn)
        .await?;
        let mut by_name: HashMap<String, NodeKey> = HashMap::new();
        let mut by_address: HashMap<String, NodeKey> = HashMap::new();
        for server in &servers {
            let key = NodeKey::Server(server.id);
            by_name
                .entry(maps::short_name(&server.hostname))
                .or_insert_with(|| key.clone());
            if let Some(address) = &server.address {
                by_address
                    .entry(address.clone())
                    .or_insert_with(|| key.clone());
            }
        }
        let mut extra = 0;
        for (device, local_port, remote_name, remote_port, address) in seen {
            let from = NodeKey::Server(device);
            let short = maps::short_name(&remote_name);
            let to = match address
                .as_ref()
                .and_then(|a| by_address.get(a))
                .or_else(|| by_name.get(&short))
            {
                Some(key) => key.clone(),
                None => {
                    let key = NodeKey::Neighbor(short.clone());
                    if !nodes.contains_key(&key) {
                        if extra >= MAX_EXTRA || short.is_empty() {
                            truncated |= extra >= MAX_EXTRA;
                            continue;
                        }
                        extra += 1;
                        let mut live = node(
                            &key,
                            NodeKind::Neighbor,
                            remote_name.clone(),
                            NodeState::Unknown,
                        );
                        live.address = address.clone();
                        nodes.insert(key.clone(), live);
                    }
                    key
                }
            };
            if to == from {
                continue;
            }
            let mut found = link(&from, &to, LinkSource::Neighbor);
            found.from_port = Some(local_port.clone());
            found.to_port = Some(remote_port).filter(|p| !p.is_empty());
            found.interface = Some(InterfaceRef {
                server_id: device,
                instance: local_port,
            });
            links.add(found);
        }
    }

    if sources.discovered && !site_ids.is_empty() {
        let runs: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
            "SELECT DISTINCT ON (r.scan_id) r.id, r.scan_id, s.site_id
             FROM network_scan_runs r JOIN network_scans s ON s.id = r.scan_id
             WHERE s.site_id = ANY($1) AND r.state = 'done'
             ORDER BY r.scan_id, r.finished_at DESC",
        )
        .bind(&site_ids)
        .fetch_all(&mut *conn)
        .await?;
        let mut extra = 0;
        for (run, scan, site) in runs {
            let known = super::network_scans::known(&mut conn, site).await?;
            let dismissed = super::network_scans::dismissed(&mut conn, scan).await?;
            for stored in super::network_scans::hosts(&mut conn, run).await? {
                let host = stored.host;
                let key = NodeKey::Host(host.address);
                if dismissed.contains(&host.address)
                    || nodes.contains_key(&key)
                    || crate::netscan::proposals::monitored_by(&host, &known).is_some()
                {
                    continue;
                }
                if extra >= MAX_EXTRA {
                    truncated = true;
                    break;
                }
                extra += 1;
                let mut live = node(
                    &key,
                    NodeKind::Host,
                    crate::netscan::proposals::device_name(&host),
                    NodeState::Unknown,
                );
                live.site_id = Some(site);
                live.address = Some(host.address.to_string());
                nodes.insert(key, live);
            }
        }
    }
    drop(conn);

    // Links drawn by hand, between nodes the viewer sees.
    for drawn in &document.links {
        let (Ok(from), Ok(to)) = (drawn.from.parse::<NodeKey>(), drawn.to.parse::<NodeKey>())
        else {
            continue;
        };
        if !nodes.contains_key(&from) || !nodes.contains_key(&to) {
            continue;
        }
        let mut live = link(&from, &to, LinkSource::Manual);
        live.id = drawn.id.clone();
        live.label = drawn.label.clone();
        live.interface = drawn.interface.clone();
        // Never merged: a person drew it on purpose.
        links.list.push(live);
    }

    // Traffic of the interfaces links show, in one query.
    let peers: HashMap<Uuid, &str> = servers.iter().map(|s| (s.id, s.peer_id.as_str())).collect();
    let traffic = traffic(state, &peers, &links.list).await;
    for live in &mut links.list {
        live.traffic = live
            .interface
            .as_ref()
            .and_then(|i| traffic.get(&(i.server_id, i.instance.clone())).copied());
        let end = |key: &str| {
            key.parse::<NodeKey>()
                .ok()
                .and_then(|k| nodes.get(&k))
                .map_or(NodeState::Unknown, |n| n.state)
        };
        live.state = maps::link_state(end(&live.from), end(&live.to), live.traffic.as_ref());
    }

    // Where people placed nodes, and which they left off.
    for placement in &document.nodes {
        if let Ok(key) = placement.key.parse::<NodeKey>() {
            if let Some(live) = nodes.get_mut(&key) {
                live.x = Some(placement.x);
                live.y = Some(placement.y);
                live.hidden = placement.hidden;
            }
        }
    }

    Ok(MapLive {
        id: row.id,
        name: row.name.clone(),
        group_by_site: document.group_by_site,
        sites: sites
            .into_iter()
            .map(|(id, name)| MapSite { id, name })
            .collect(),
        nodes: nodes.into_values().collect(),
        links: links.list,
        zones: document.zones,
        labels: document.labels,
        background: row.background_updated_at.map(|updated_at| LiveBackground {
            updated_at,
            placement: document.background,
        }),
        neighbors_read_at,
        truncated,
        now: Utc::now().timestamp(),
    })
}

/// The metric suffixes that say an interface's traffic and state.
const TRAFFIC_SUFFIXES: [&str; 7] = [
    "in_bytes_per_second",
    "out_bytes_per_second",
    "receive_bytes_per_second",
    "transmit_bytes_per_second",
    "in_utilization_ratio",
    "out_utilization_ratio",
    "oper_up",
];

/// Reads one answer of VictoriaMetrics into traffic by `(peer, instance)`.
pub fn read_traffic(data: &Value) -> HashMap<(String, String), Traffic> {
    let mut found: HashMap<(String, String), Traffic> = HashMap::new();
    for series in data["result"].as_array().into_iter().flatten() {
        let metric = &series["metric"];
        let (Some(name), Some(peer), Some(instance)) = (
            metric["__name__"].as_str(),
            metric["peer"].as_str(),
            metric["instance"].as_str(),
        ) else {
            continue;
        };
        let Some(value) = series["value"][1]
            .as_str()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite())
        else {
            continue;
        };
        let Some(suffix) = TRAFFIC_SUFFIXES.iter().find(|s| name.ends_with(*s)) else {
            continue;
        };
        let entry = found
            .entry((peer.to_owned(), instance.to_owned()))
            .or_default();
        match *suffix {
            "in_bytes_per_second" | "receive_bytes_per_second" => {
                entry.in_bytes_per_second = Some(value)
            }
            "out_bytes_per_second" | "transmit_bytes_per_second" => {
                entry.out_bytes_per_second = Some(value)
            }
            "oper_up" => entry.up = Some(value >= 1.0),
            _ => {
                let use_ = value.clamp(0.0, 1.0);
                entry.utilization = Some(entry.utilization.map_or(use_, |u| u.max(use_)));
            }
        }
    }
    found
}

/// The traffic of the interfaces `links` show, by `(server, instance)`;
/// none when the metrics backend is absent or fails, as the map still
/// draws without it.
async fn traffic(
    state: &ApiState,
    peers: &HashMap<Uuid, &str>,
    links: &[LiveLink],
) -> HashMap<(Uuid, String), Traffic> {
    let interfaces: BTreeSet<(Uuid, String)> = links
        .iter()
        .filter_map(|l| l.interface.as_ref())
        .filter(|i| peers.contains_key(&i.server_id))
        .map(|i| (i.server_id, i.instance.clone()))
        .collect();
    let Some(vm) = state.metrics.clone() else {
        return HashMap::new();
    };
    if interfaces.is_empty() {
        return HashMap::new();
    }
    let peer_names: BTreeSet<&str> = interfaces.iter().map(|(id, _)| peers[id]).collect();
    let instances: BTreeSet<&str> = interfaces.iter().map(|(_, i)| i.as_str()).collect();
    let alternatives = |values: &BTreeSet<&str>| {
        values
            .iter()
            .map(|v| regex_escape(v))
            .collect::<Vec<_>>()
            .join("|")
    };
    let query = format!(
        "{{__name__=~\"monitorflow_.+_({})\",peer=~\"{}\",instance=~\"{}\"}}",
        TRAFFIC_SUFFIXES.join("|"),
        alternatives(&peer_names),
        alternatives(&instances),
    );
    if query.len() > super::dashboard_data::MAX_QUERY_BYTES {
        return HashMap::new();
    }
    let at = Utc::now().timestamp() / 30 * 30;
    let answer = super::dashboard_data::cached(
        &vm,
        &state.dashboard_cache,
        "/api/v1/query",
        &[("query", query), ("time", at.to_string())],
    )
    .await;
    let data = match answer {
        Ok(data) => data,
        Err(error) => {
            tracing::warn!(%error, "map traffic query failed");
            return HashMap::new();
        }
    };
    let by_peer: HashMap<&str, Uuid> = peers.iter().map(|(id, peer)| (*peer, *id)).collect();
    read_traffic(&data)
        .into_iter()
        .filter_map(|((peer, instance), traffic)| {
            let id = *by_peer.get(peer.as_str())?;
            interfaces
                .contains(&(id, instance.clone()))
                .then_some(((id, instance), traffic))
        })
        .collect()
}

/// Reads and stores the neighbours of one device; the error is a
/// sentence.
async fn read_device(state: &ApiState, user: &CurrentUser, id: Uuid) -> Result<usize, String> {
    let mut walks: [neighbors::Rows; 6] = Default::default();
    for (slot, oid) in walks.iter_mut().zip(neighbors::WALKS) {
        let params = BTreeMap::from([
            ("walk".to_owned(), oid.to_owned()),
            (
                "max".to_owned(),
                monitorflow_snmp::read::MAX_WALK.to_string(),
            ),
        ]);
        let (_, rows, _, _) = super::snmp_checks::read_on(state, user, id, None, params)
            .await
            .map_err(|e| match e {
                ApiError::Invalid(m) | ApiError::Conflict(m) => m,
                other => other.to_string(),
            })?;
        *slot = rows
            .into_iter()
            .filter(|(_, kind, _)| !kind.starts_with("noSuch") && kind != "endOfMibView")
            .map(|(oid, _, value)| (oid, value))
            .collect();
    }
    let found = neighbors::parse(&walks);
    let mut tx = state.pool.begin().await.map_err(|e| e.to_string())?;
    let store = async {
        sqlx::query("DELETE FROM network_neighbors WHERE server_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        for n in &found {
            sqlx::query(
                "INSERT INTO network_neighbors
                     (server_id, protocol, local_port, remote_name, remote_port, remote_address)
                 VALUES ($1, $2, $3, $4, $5, $6::inet)",
            )
            .bind(id)
            .bind(n.protocol)
            .bind(&n.local_port)
            .bind(&n.remote_name)
            .bind(&n.remote_port)
            .bind(n.remote_address.map(|a: IpAddr| a.to_string()))
            .execute(&mut *tx)
            .await?;
        }
        Ok::<_, sqlx::Error>(())
    };
    store.await.map_err(|e| e.to_string())?;
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(found.len())
}

async fn note_read(
    db: impl PgExecutor<'_>,
    id: Uuid,
    error: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO network_neighbor_reads (server_id, read_at, error) VALUES ($1, now(), $2)
         ON CONFLICT (server_id) DO UPDATE SET read_at = now(), error = $2",
    )
    .bind(id)
    .bind(error.map(|e| e.chars().take(512).collect::<String>()))
    .execute(db)
    .await?;
    Ok(())
}

/// `POST /api/maps/{id}/neighbors`: reads the LLDP and CDP neighbours of
/// the SNMP devices of the map's place that the caller may change, from
/// each device's probe, and keeps them for every map. Operators, as it
/// sends the devices' credentials to their probes; not audited, as it
/// changes no setting.
pub async fn read_neighbors(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<NeighborRead>, ApiError> {
    let id = parse_id(&id)?;
    let row = visible_row(&state.pool, &user, id).await?;
    if !may_edit(&user, row.place()) {
        return Err(ApiError::Forbidden);
    }
    let (site, customer) = columns(row.place());
    let (all, modifiable) = user.modifiable_sites().bind();
    let devices: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT s.id, s.hostname FROM servers s
         JOIN sites si ON si.id = s.site_id
         JOIN snmp_credentials c ON c.server_id = s.id
         WHERE s.kind = 'device' AND s.revoked_at IS NULL AND s.address IS NOT NULL
           AND ($1::uuid IS NULL OR s.site_id = $1) AND ($2::uuid IS NULL OR si.customer_id = $2)
           AND ($3 OR s.site_id = ANY($4))
         ORDER BY s.hostname, s.id LIMIT $5",
    )
    .bind(site)
    .bind(customer)
    .bind(all)
    .bind(&modifiable)
    .bind(MAX_DEVICES as i64)
    .fetch_all(&state.pool)
    .await?;
    let semaphore = Arc::new(tokio::sync::Semaphore::new(PARALLEL));
    let mut tasks = tokio::task::JoinSet::new();
    for (device, name) in devices.clone() {
        let (state, user, semaphore) = (state.clone(), user.clone(), semaphore.clone());
        tasks.spawn(async move {
            let _permit = semaphore.acquire_owned().await;
            (device, name, read_device(&state, &user, device).await)
        });
    }
    let mut result = NeighborRead {
        devices: devices.len(),
        neighbors: 0,
        failed: Vec::new(),
    };
    while let Some(joined) = tasks.join_next().await {
        let (device, name, outcome) = joined.map_err(|e| ApiError::Internal(e.to_string()))?;
        match outcome {
            Ok(count) => {
                result.neighbors += count;
                note_read(&state.pool, device, None).await?;
            }
            Err(error) => {
                note_read(&state.pool, device, Some(&error)).await?;
                result.failed.push(NeighborFailure {
                    server_id: device,
                    name,
                    error,
                });
            }
        }
    }
    result.failed.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Json(result))
}

/// Reads `(dashboard, map)` a screen asks the background of: whether the
/// dashboard's spec has a panel of that map.
pub(super) fn spec_shows_map(spec: &super::dashboards::Spec, map: Uuid) -> bool {
    spec.panels.iter().any(|p| p.map_id == Some(map))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backgrounds_are_pictures_by_their_first_bytes() {
        assert_eq!(image_type(b"\x89PNG\r\n\x1a\n...."), Some("image/png"));
        assert_eq!(image_type(&[0xff, 0xd8, 0xff, 0xe0, 0]), Some("image/jpeg"));
        assert_eq!(image_type(b"RIFF\x10\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(image_type(b"<svg onload=alert(1)>"), None);
        assert_eq!(image_type(b"GIF89a"), None);
        assert_eq!(image_type(b""), None);
    }

    #[test]
    fn traffic_reads_agents_and_snmp_alike() {
        let data = json!({"result": [
            {"metric": {"__name__": "monitorflow_snmp_if_in_bytes_per_second", "peer": "sw", "instance": "Gi0/1"}, "value": [1, "1000"]},
            {"metric": {"__name__": "monitorflow_snmp_if_out_bytes_per_second", "peer": "sw", "instance": "Gi0/1"}, "value": [1, "500"]},
            {"metric": {"__name__": "monitorflow_snmp_if_in_utilization_ratio", "peer": "sw", "instance": "Gi0/1"}, "value": [1, "0.2"]},
            {"metric": {"__name__": "monitorflow_snmp_if_out_utilization_ratio", "peer": "sw", "instance": "Gi0/1"}, "value": [1, "0.4"]},
            {"metric": {"__name__": "monitorflow_snmp_if_oper_up", "peer": "sw", "instance": "Gi0/1"}, "value": [1, "1"]},
            {"metric": {"__name__": "monitorflow_net_receive_bytes_per_second", "peer": "web", "instance": "eth0"}, "value": [1, "42"]},
            {"metric": {"__name__": "monitorflow_snmp_if_oper_up", "peer": "sw", "instance": "Gi0/2"}, "value": [1, "0"]},
            {"metric": {"__name__": "monitorflow_snmp_if_in_bytes_per_second", "peer": "sw"}, "value": [1, "7"]},
            {"metric": {"__name__": "monitorflow_snmp_if_in_bytes_per_second", "peer": "sw", "instance": "x"}, "value": [1, "NaN"]}
        ]});
        let traffic = read_traffic(&data);
        assert_eq!(traffic.len(), 3);
        let uplink = traffic[&("sw".to_owned(), "Gi0/1".to_owned())];
        assert_eq!(uplink.in_bytes_per_second, Some(1000.0));
        assert_eq!(uplink.out_bytes_per_second, Some(500.0));
        assert_eq!(uplink.utilization, Some(0.4));
        assert_eq!(uplink.up, Some(true));
        assert_eq!(
            traffic[&("web".to_owned(), "eth0".to_owned())].in_bytes_per_second,
            Some(42.0)
        );
        assert_eq!(
            traffic[&("sw".to_owned(), "Gi0/2".to_owned())].up,
            Some(false)
        );
    }

    #[test]
    fn links_between_the_same_ends_are_one() {
        let (a, b) = (
            NodeKey::Server(Uuid::from_u128(1)),
            NodeKey::Relay(Uuid::from_u128(2)),
        );
        let mut links = Links::default();
        links.add(link(&b, &a, LinkSource::Relay));
        let mut seen = link(&a, &b, LinkSource::Neighbor);
        seen.from_port = Some("eth0".into());
        seen.to_port = Some("Gi0/3".into());
        links.add(seen);
        assert_eq!(links.list.len(), 1);
        let only = &links.list[0];
        assert_eq!(only.source, LinkSource::Relay);
        // The ports follow their ends, whichever way the link was drawn.
        assert_eq!(only.from, b.to_string());
        assert_eq!(only.from_port.as_deref(), Some("Gi0/3"));
        assert_eq!(only.to_port.as_deref(), Some("eth0"));
    }
}
