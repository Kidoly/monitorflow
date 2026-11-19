//! Dependencies (ADR-0152): what is behind what, set from a server's,
//! device's or relay's page and through the public API.
//!
//! Operators of both ends' sites add and remove them, viewers read them.
//! Each change is audited. A dependency never crosses customers, and one
//! that would put something behind itself is refused with the path.

use std::collections::{HashMap, HashSet};

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use monitorflow_common::tls::PeerRole;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::access::authorize;
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::error::ApiError;
use super::sensors::{body, parse_id};
use super::servers::{ServerState, ServerView};
use super::ApiState;
use crate::dependencies::store::{self, Streams};
use crate::dependencies::{graph, CheckRef, Edge, Hold, Holds, Node};
use crate::sensor_config::builtin_defaults;

/// Most children one request puts behind a parent.
pub const MAX_CHILDREN: usize = 500;

/// A check of a server or device, by its name and what it watches.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = DependencyCheck)]
#[serde(deny_unknown_fields)]
pub struct Check {
    /// The check's name, like `ping` or `service_nginx`.
    pub name: String,
    /// What it watches, `""` (the default) for every instance.
    #[serde(default)]
    pub instance: String,
}

impl From<CheckRef> for Check {
    fn from(c: CheckRef) -> Self {
        Self {
            name: c.name,
            instance: c.instance,
        }
    }
}

/// One end of a dependency in a request: a server or device, one of its
/// checks, or (for a parent only) a relay.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[schema(as = DependencyEndRequest)]
#[serde(deny_unknown_fields)]
pub struct EndRequest {
    /// A server or device.
    pub server_id: Option<Uuid>,
    /// A relay; a parent only.
    pub relay_id: Option<Uuid>,
    /// One check of `server_id` instead of all of it.
    pub check: Option<Check>,
}

/// Body of `POST /api/dependencies`: `children` are behind `parent`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[schema(as = DependencyRequest)]
#[serde(deny_unknown_fields)]
pub struct DependencyRequest {
    pub parent: EndRequest,
    /// At most 500, all added or none.
    pub children: Vec<EndRequest>,
}

/// What an end is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[schema(as = DependencyEndKind)]
#[serde(rename_all = "lowercase")]
pub enum EndKind {
    Server,
    Device,
    Relay,
}

/// One end of a dependency, named.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
#[schema(as = DependencyEnd)]
pub struct EndView {
    pub kind: EndKind,
    /// The server or device; `null` for a relay.
    pub server_id: Option<Uuid>,
    /// The relay; `null` otherwise.
    pub relay_id: Option<Uuid>,
    /// Its host name, or the relay's name.
    pub name: String,
    pub site_id: Uuid,
    /// One of its checks; `null` for all of it.
    pub check: Option<Check>,
}

/// A dependency: `child` is behind `parent`.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[schema(as = Dependency)]
pub struct DependencyView {
    /// `null` for an automatic one, which cannot be removed.
    pub id: Option<Uuid>,
    /// A server behind its relay, or a check behind the relay that runs it.
    pub automatic: bool,
    pub child: EndView,
    pub parent: EndView,
    /// `null` for an automatic one.
    pub created_at: Option<DateTime<Utc>>,
    /// Who added it; `null` for an automatic one, an import's without a
    /// person, or once the account is removed.
    pub created_by: Option<String>,
    /// Whether the caller may remove it.
    pub can_change: bool,
}

/// Body of `GET /api/dependencies` and `POST /api/dependencies`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = DependencyList)]
pub struct DependencyList {
    pub dependencies: Vec<DependencyView>,
}

/// Query string of `GET /api/dependencies`.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ListParams {
    /// Only those whose child or parent is this server or device, or one
    /// of its checks.
    pub server_id: Option<Uuid>,
    /// Only those behind this relay.
    pub relay_id: Option<Uuid>,
}

/// Why something cannot be checked now.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[schema(as = Unreachable)]
pub struct UnreachableNote {
    /// The nearest parent that is down.
    pub because: EndView,
    /// Its incident, which speaks for what is behind it.
    pub incident_id: Uuid,
    pub since: DateTime<Utc>,
}

/// A check of a page's server that is unreachable, while the server is not.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[schema(as = UnreachableCheck)]
pub struct UnreachableCheck {
    pub check: Check,
    #[serde(flatten)]
    pub note: UnreachableNote,
}

/// Why a parent is suggested.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[schema(as = DependencySuggestionReason)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionReason {
    /// The Proxmox host its virtual machine or container runs on.
    ProxmoxHost,
    /// A switch, router or firewall of the same site.
    NetworkDevice,
}

/// A parent that looks obvious; never added without being asked.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[schema(as = DependencySuggestion)]
pub struct Suggestion {
    pub parent: EndView,
    pub reason: SuggestionReason,
}

/// Body of `GET /api/servers/{id}/dependencies` and
/// `GET /api/relays/{id}/dependencies`: what its page shows.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = Dependencies)]
pub struct PageView {
    /// What it, or one of its checks, depends on, automatic ones included.
    pub parents: Vec<DependencyView>,
    /// What depends directly on it or one of its checks.
    pub children: Vec<DependencyView>,
    /// Everything behind it, directly or through others.
    pub behind: Vec<EndView>,
    /// Parents that look obvious and are not set.
    pub suggestions: Vec<Suggestion>,
    /// While a parent is down; `null` otherwise.
    pub unreachable: Option<UnreachableNote>,
    /// Its checks that are unreachable while it is not.
    pub unreachable_checks: Vec<UnreachableCheck>,
    /// Whether the caller may add and remove its dependencies.
    pub can_change: bool,
}

/// What the API knows of a server, device or relay.
#[derive(Debug, Clone)]
struct Place {
    kind: EndKind,
    name: String,
    site_id: Uuid,
    customer_id: Uuid,
}

/// The places of the servers and relays `nodes` name.
async fn places(
    db: &mut PgConnection,
    nodes: impl IntoIterator<Item = &Node>,
) -> Result<HashMap<Node, Place>, sqlx::Error> {
    let mut servers = Vec::new();
    let mut relays = Vec::new();
    for node in nodes {
        match node {
            Node::Server(id) | Node::Check(id, ..) => servers.push(*id),
            Node::Relay(id) => relays.push(*id),
        }
    }
    let rows: Vec<(bool, Uuid, String, bool, Uuid, Uuid)> = sqlx::query_as(
        "SELECT false, s.id, s.hostname, s.kind = 'device', s.site_id, si.customer_id
         FROM servers s JOIN sites si ON si.id = s.site_id WHERE s.id = ANY($1)
         UNION ALL
         SELECT true, r.id, r.name, false, r.site_id, si.customer_id
         FROM relays r JOIN sites si ON si.id = r.site_id WHERE r.id = ANY($2)",
    )
    .bind(&servers)
    .bind(&relays)
    .fetch_all(&mut *db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(relay, id, name, device, site_id, customer_id)| {
            let (node, kind) = match (relay, device) {
                (true, _) => (Node::Relay(id), EndKind::Relay),
                (false, true) => (Node::Server(id), EndKind::Device),
                (false, false) => (Node::Server(id), EndKind::Server),
            };
            (
                node,
                Place {
                    kind,
                    name,
                    site_id,
                    customer_id,
                },
            )
        })
        .collect())
}

fn whole(node: &Node) -> Node {
    match node {
        Node::Check(id, ..) => Node::Server(*id),
        other => other.clone(),
    }
}

fn end_view(node: &Node, places: &HashMap<Node, Place>) -> Option<EndView> {
    let place = places.get(&whole(node))?;
    Some(EndView {
        kind: place.kind,
        server_id: node.server(),
        relay_id: match node {
            Node::Relay(id) => Some(*id),
            _ => None,
        },
        name: place.name.clone(),
        site_id: place.site_id,
        check: node.check_ref().map(Check::from),
    })
}

/// How a node reads in a sentence: "web-1", "ping on sw-core".
fn words(node: &Node, places: &HashMap<Node, Place>) -> String {
    let name = places
        .get(&whole(node))
        .map_or_else(|| "something removed".to_owned(), |p| p.name.clone());
    match node {
        Node::Check(_, check, instance) if instance.is_empty() => format!("{check} on {name}"),
        Node::Check(_, check, instance) => format!("{check} ({instance}) on {name}"),
        _ => name,
    }
}

/// Who added each dependency and when, by ID.
async fn metadata(
    db: &mut PgConnection,
    ids: &[Uuid],
) -> Result<HashMap<Uuid, (DateTime<Utc>, Option<String>)>, sqlx::Error> {
    let rows: Vec<(Uuid, DateTime<Utc>, Option<String>)> = sqlx::query_as(
        "SELECT d.id, d.created_at, u.username FROM dependencies d
         LEFT JOIN users u ON u.id = d.created_by WHERE d.id = ANY($1)",
    )
    .bind(ids)
    .fetch_all(&mut *db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, at, by)| (id, (at, by)))
        .collect())
}

/// The views of `edges` whose both ends the user may read.
async fn views(
    db: &mut PgConnection,
    user: &CurrentUser,
    edges: &[&Edge],
) -> Result<Vec<DependencyView>, sqlx::Error> {
    let nodes: Vec<&Node> = edges.iter().flat_map(|e| [&e.child, &e.parent]).collect();
    let places = places(&mut *db, nodes).await?;
    let ids: Vec<Uuid> = edges.iter().filter_map(|e| e.id).collect();
    let metadata = metadata(&mut *db, &ids).await?;
    let readable = user.readable_sites();
    let modifiable = user.modifiable_sites();
    let mut views: Vec<DependencyView> = edges
        .iter()
        .filter_map(|edge| {
            let child = end_view(&edge.child, &places)?;
            let parent = end_view(&edge.parent, &places)?;
            if !readable.contains(child.site_id) || !readable.contains(parent.site_id) {
                return None;
            }
            let (created_at, created_by) = edge
                .id
                .and_then(|id| metadata.get(&id).cloned())
                .map_or((None, None), |(at, by)| (Some(at), by));
            Some(DependencyView {
                id: edge.id,
                automatic: edge.id.is_none(),
                can_change: edge.id.is_some()
                    && modifiable.contains(child.site_id)
                    && modifiable.contains(parent.site_id),
                child,
                parent,
                created_at,
                created_by,
            })
        })
        .collect();
    views.sort_by(|a, b| {
        (&a.child.name, &a.parent.name, a.automatic).cmp(&(
            &b.child.name,
            &b.parent.name,
            b.automatic,
        ))
    });
    Ok(views)
}

/// The holds now, from every instance's streams.
pub(super) async fn holds(state: &ApiState, db: &mut PgConnection) -> Result<Holds, sqlx::Error> {
    let live: HashSet<(PeerRole, String)> = state
        .registry
        .live_all()
        .into_iter()
        .map(|p| (p.role, p.id))
        .collect();
    store::holds(&mut *db, Streams::Live(&live)).await
}

/// The note of `hold` when it is unreachable.
async fn note(
    db: &mut PgConnection,
    holds: &Holds,
    hold: Option<&Hold>,
    places: &mut HashMap<Node, Place>,
) -> Result<Option<UnreachableNote>, sqlx::Error> {
    let Some((cause, incident)) = hold.and_then(Hold::unreachable) else {
        return Ok(None);
    };
    if !places.contains_key(&whole(cause)) {
        places.extend(self::places(&mut *db, [cause]).await?);
    }
    let Some(because) = end_view(cause, places) else {
        return Ok(None);
    };
    let since = holds.down(cause).map_or_else(Utc::now, |d| d.since);
    Ok(Some(UnreachableNote {
        because,
        incident_id: incident,
        since,
    }))
}

/// Marks the servers of `views` that are behind a parent that is down:
/// `state: unreachable` with the parent, unless silenced or waiting for
/// their agent (ADR-0152).
pub(super) async fn mark_unreachable(
    state: &ApiState,
    views: &mut [ServerView],
) -> Result<(), sqlx::Error> {
    if views.is_empty() {
        return Ok(());
    }
    let mut conn = state.pool.acquire().await?;
    let holds = holds(state, &mut conn).await?;
    let mut places = HashMap::new();
    for view in views.iter_mut() {
        if matches!(view.state, ServerState::Silenced | ServerState::Waiting) {
            continue;
        }
        let hold = holds.of(&Node::Server(view.id));
        if let Some(note) = note(&mut conn, &holds, hold, &mut places).await? {
            view.state = ServerState::Unreachable;
            view.unreachable = Some(note);
        }
    }
    Ok(())
}

/// What is behind the parent the open incident `incident` says is down,
/// as far as the user may read.
pub(super) async fn hides(
    state: &ApiState,
    user: &CurrentUser,
    incident: Uuid,
) -> Result<Vec<EndView>, sqlx::Error> {
    let mut conn = state.pool.acquire().await?;
    let holds = holds(state, &mut conn).await?;
    let behind = holds.behind_incident(incident);
    let places = places(&mut conn, &behind).await?;
    let readable = user.readable_sites();
    Ok(behind
        .iter()
        .filter_map(|n| end_view(n, &places))
        .filter(|v| readable.contains(v.site_id))
        .collect())
}

/// Why each of `servers` that is unreachable now is, for dashboards.
pub(super) async fn unreachable_notes(
    state: &ApiState,
    servers: &[Uuid],
) -> Result<HashMap<Uuid, UnreachableNote>, sqlx::Error> {
    let mut found = HashMap::new();
    if servers.is_empty() {
        return Ok(found);
    }
    let mut conn = state.pool.acquire().await?;
    let holds = holds(state, &mut conn).await?;
    let mut places = HashMap::new();
    for id in servers {
        if let Some(note) =
            note(&mut conn, &holds, holds.of(&Node::Server(*id)), &mut places).await?
        {
            found.insert(*id, note);
        }
    }
    Ok(found)
}

/// Whether `server` is unreachable in `holds`, for the summary.
pub(super) fn unreachable(holds: &Holds, server: Uuid) -> bool {
    holds
        .of(&Node::Server(server))
        .is_some_and(|h| h.unreachable().is_some())
}

/// `GET /api/dependencies`: the dependencies set between servers,
/// devices and relays the user may read. Any role.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
    params: Result<Query<ListParams>, QueryRejection>,
) -> Result<Json<DependencyList>, ApiError> {
    let Query(params) = params.map_err(|r| ApiError::Invalid(r.body_text()))?;
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let mut conn = state.pool.acquire().await?;
    if let Some(server) = params.server_id {
        let site = super::sensors::site_of_server(&state.pool, server).await?;
        authorize(&user, site, false)?;
    }
    if let Some(relay) = params.relay_id {
        authorize(&user, site_of_relay(&state.pool, relay).await?, false)?;
    }
    let edges = store::edges(&mut conn).await?;
    let chosen: Vec<&Edge> = edges
        .iter()
        .filter(|e| e.id.is_some())
        .filter(|e| {
            params
                .server_id
                .is_none_or(|s| e.child.server() == Some(s) || e.parent.server() == Some(s))
        })
        .filter(|e| params.relay_id.is_none_or(|r| e.parent == Node::Relay(r)))
        .collect();
    let dependencies = views(&mut conn, &user, &chosen).await?;
    Ok(Json(DependencyList { dependencies }))
}

async fn site_of_relay(pool: &PgPool, relay: Uuid) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar("SELECT site_id FROM relays WHERE id = $1 AND revoked_at IS NULL")
        .bind(relay)
        .fetch_optional(pool)
        .await
}

/// The node of a request's end, checked for shape.
fn node_of(end: &EndRequest, parent: bool) -> Result<Node, ApiError> {
    match (end.server_id, end.relay_id, &end.check) {
        (Some(server), None, None) => Ok(Node::Server(server)),
        (Some(server), None, Some(check)) => {
            let valid = !check.name.is_empty()
                && check.name.len() <= 64
                && check.name.starts_with(|c: char| c.is_ascii_lowercase())
                && check
                    .name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                && check.instance.len() <= 256
                && !check.instance.chars().any(char::is_control);
            if !valid {
                return Err(ApiError::Invalid(format!(
                    "`{}` is not a check name: lowercase letters, digits and `_`.",
                    check.name
                )));
            }
            Ok(Node::Check(
                server,
                check.name.clone(),
                check.instance.clone(),
            ))
        }
        (None, Some(relay), None) if parent => Ok(Node::Relay(relay)),
        (None, Some(_), None) => Err(ApiError::Invalid(
            "A relay can be a parent only: what is behind it is set from its servers.".into(),
        )),
        (None, Some(_), Some(_)) => {
            Err(ApiError::Invalid("A relay has no checks to choose.".into()))
        }
        _ => Err(ApiError::Invalid(
            "Give each end a `server_id`, or a `relay_id` for a parent.".into(),
        )),
    }
}

/// The checks `server` has: its own, its site's, and the built-in ones of
/// an agent.
async fn checks_of(
    db: &mut PgConnection,
    server: Uuid,
    device: bool,
) -> Result<HashSet<String>, sqlx::Error> {
    let mut names: HashSet<String> = sqlx::query_scalar(
        "SELECT c.name FROM sensor_configs c JOIN servers s ON s.id = $1
         WHERE c.server_id = s.id OR (c.site_id = s.site_id AND NOT $2)",
    )
    .bind(server)
    .bind(device)
    .fetch_all(&mut *db)
    .await?
    .into_iter()
    .collect();
    if !device {
        names.extend(builtin_defaults(0).into_iter().map(|spec| spec.name));
    }
    Ok(names)
}

async fn record(
    db: &mut PgConnection,
    user: &CurrentUser,
    action: &str,
    id: Uuid,
    child: &Node,
    parent: &Node,
) -> Result<(), sqlx::Error> {
    let end = |node: &Node| match node {
        Node::Relay(id) => json!({ "relay_id": id }),
        Node::Server(id) => json!({ "server_id": id }),
        Node::Check(id, name, instance) => {
            json!({ "server_id": id, "check": {"name": name, "instance": instance} })
        }
    };
    let actor = user.id.to_string();
    audit::record(
        &mut *db,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action,
            target: Some(("dependency", &id.to_string())),
            details: json!({ "child": end(child), "parent": end(parent) }),
        },
    )
    .await
}

/// `POST /api/dependencies`: puts `children` behind `parent`, all or none.
/// Operators of every end's site.
pub async fn create(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<DependencyRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<DependencyList>), ApiError> {
    let request = body(request)?;
    if request.children.is_empty() {
        return Err(ApiError::Invalid(
            "Choose at least one server, device or check behind it.".into(),
        ));
    }
    if request.children.len() > MAX_CHILDREN {
        return Err(ApiError::Invalid(format!(
            "Choose at most {MAX_CHILDREN} at once."
        )));
    }
    let parent = node_of(&request.parent, true)?;
    let mut children = Vec::with_capacity(request.children.len());
    for child in &request.children {
        let child = node_of(child, false)?;
        if !children.contains(&child) {
            children.push(child);
        }
    }
    let mut tx = state.pool.begin().await?;
    let all: Vec<&Node> = children.iter().chain([&parent]).collect();
    let places = places(&mut tx, all.iter().copied()).await?;
    // Missing or unreadable ends are 404, as if they did not exist.
    let place = |node: &Node| places.get(&whole(node)).ok_or(ApiError::NotFound);
    let parent_place = place(&parent)?;
    authorize(&user, Some(parent_place.site_id), true)?;
    let removed: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM servers WHERE id = ANY($1) AND revoked_at IS NOT NULL
         UNION ALL SELECT id FROM relays WHERE id = ANY($1) AND revoked_at IS NOT NULL",
    )
    .bind(
        all.iter()
            .filter_map(|n| {
                n.server().or(match n {
                    Node::Relay(id) => Some(*id),
                    _ => None,
                })
            })
            .collect::<Vec<Uuid>>(),
    )
    .fetch_all(&mut *tx)
    .await?;
    if !removed.is_empty() {
        return Err(ApiError::NotFound);
    }
    for child in &children {
        let child_place = place(child)?;
        authorize(&user, Some(child_place.site_id), true)?;
        if child_place.customer_id != parent_place.customer_id {
            return Err(ApiError::Invalid(
                "Choose a parent of the same customer: a dependency never crosses customers."
                    .into(),
            ));
        }
        if whole(child) == whole(&parent)
            && !(matches!(child, Node::Check(..)) && matches!(parent, Node::Check(..)))
        {
            return Err(ApiError::Invalid(format!(
                "{} cannot depend on itself.",
                words(child, &places)
            )));
        }
        if child == &parent {
            return Err(ApiError::Invalid(format!(
                "{} cannot depend on itself.",
                words(child, &places)
            )));
        }
    }
    // Checks must exist.
    let mut known: HashMap<Uuid, HashSet<String>> = HashMap::new();
    for node in &all {
        if let Node::Check(server, name, _) = node {
            if !known.contains_key(server) {
                let device = place(node)?.kind == EndKind::Device;
                known.insert(*server, checks_of(&mut tx, *server, device).await?);
            }
            if !known[server].contains(name) {
                return Err(ApiError::Invalid(format!(
                    "{} has no check `{name}`.",
                    place(node)?.name
                )));
            }
        }
    }
    let edges = store::edges(&mut tx).await?;
    for child in &children {
        if edges
            .iter()
            .any(|e| &e.child == child && e.parent == parent)
        {
            return Err(ApiError::Conflict(format!(
                "{} already depends on {}.",
                words(child, &places),
                words(&parent, &places)
            )));
        }
    }
    let adding: Vec<(Node, Node)> = children
        .iter()
        .map(|c| (c.clone(), parent.clone()))
        .collect();
    if let Some((child, path)) = graph::cycle(&edges, &adding) {
        let mut named = places.clone();
        named.extend(self::places(&mut tx, path.iter()).await?);
        let path_words: Vec<String> = path.iter().map(|n| words(n, &named)).collect();
        return Err(ApiError::Conflict(format!(
            "{} cannot depend on {}: {} already depends on {} ({}).",
            words(&child, &named),
            words(&parent, &named),
            words(&parent, &named),
            words(&child, &named),
            path_words.join(", then ")
        )));
    }
    let mut created = Vec::with_capacity(children.len());
    for child in &children {
        let (parent_server, parent_relay, parent_check) = match &parent {
            Node::Server(id) => (Some(*id), None, None),
            Node::Check(id, name, instance) => (Some(*id), None, Some((name, instance))),
            Node::Relay(id) => (None, Some(*id), None),
        };
        let (check_name, check_instance) = match child {
            Node::Check(_, name, instance) => (Some(name.as_str()), instance.as_str()),
            _ => (None, ""),
        };
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO dependencies (server_id, check_name, check_instance, parent_server_id,
                                       parent_relay_id, parent_check_name, parent_check_instance,
                                       created_by)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
        )
        .bind(child.server())
        .bind(check_name)
        .bind(check_instance)
        .bind(parent_server)
        .bind(parent_relay)
        .bind(parent_check.map(|(n, _)| n.as_str()))
        .bind(parent_check.map_or("", |(_, i)| i.as_str()))
        .bind(user.id)
        .fetch_one(&mut *tx)
        .await?;
        record(&mut tx, &user, "dependency.create", id, child, &parent).await?;
        created.push(Edge {
            id: Some(id),
            child: child.clone(),
            parent: parent.clone(),
        });
    }
    let refs: Vec<&Edge> = created.iter().collect();
    let dependencies = views(&mut tx, &user, &refs).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(DependencyList { dependencies })))
}

/// `DELETE /api/dependencies/{id}`. Operators of both ends' sites.
pub async fn remove(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    let mut tx = state.pool.begin().await?;
    let edges = store::edges(&mut tx).await?;
    let Some(edge) = edges.into_iter().find(|e| e.id == Some(id)) else {
        return Err(ApiError::NotFound);
    };
    let places = places(&mut tx, [&edge.child, &edge.parent]).await?;
    let site = |node: &Node| places.get(&whole(node)).map(|p| p.site_id);
    authorize(&user, site(&edge.child), false)?;
    authorize(&user, site(&edge.parent), false)?;
    authorize(&user, site(&edge.child), true)?;
    authorize(&user, site(&edge.parent), true)?;
    sqlx::query("DELETE FROM dependencies WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    record(
        &mut tx,
        &user,
        "dependency.remove",
        id,
        &edge.child,
        &edge.parent,
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The page view of `node`, a server or a relay.
async fn page(
    state: &ApiState,
    user: &CurrentUser,
    node: Node,
    site: Uuid,
) -> Result<PageView, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let edges = store::edges(&mut conn).await?;
    let id = match &node {
        Node::Server(id) | Node::Relay(id) | Node::Check(id, ..) => *id,
    };
    let is_me = |n: &Node| match (&node, n) {
        (Node::Relay(_), _) => n == &node,
        (_, n) => n.server() == Some(id),
    };
    let parents: Vec<&Edge> = edges.iter().filter(|e| is_me(&e.child)).collect();
    let children: Vec<&Edge> = edges.iter().filter(|e| is_me(&e.parent)).collect();
    let parents = views(&mut conn, user, &parents).await?;
    let children = views(&mut conn, user, &children).await?;
    let holds = holds(state, &mut conn).await?;
    let behind_nodes = holds.behind(&node);
    let mut places = places(&mut conn, behind_nodes.iter().chain([&node])).await?;
    let readable = user.readable_sites();
    let behind: Vec<EndView> = behind_nodes
        .iter()
        .filter_map(|n| end_view(n, &places))
        .filter(|v| readable.contains(v.site_id))
        .collect();
    let unreachable = note(&mut conn, &holds, holds.of(&node), &mut places).await?;
    let mut unreachable_checks = Vec::new();
    if let Node::Server(server) = node {
        for (check, hold) in holds.checks_of(server) {
            if let Some(note) = note(&mut conn, &holds, Some(&hold), &mut places).await? {
                unreachable_checks.push(UnreachableCheck {
                    check: check.into(),
                    note,
                });
            }
        }
        unreachable_checks.sort_by(|a, b| {
            (&a.check.name, &a.check.instance).cmp(&(&b.check.name, &b.check.instance))
        });
    }
    let suggestions = match node {
        Node::Server(server) => suggestions(&mut conn, user, server, &edges).await?,
        _ => Vec::new(),
    };
    Ok(PageView {
        parents,
        children,
        behind,
        suggestions,
        unreachable,
        unreachable_checks,
        can_change: user.modifiable_sites().contains(site),
    })
}

/// Parents that look obvious for `server` and are not set: the Proxmox
/// host it runs on, when monitored, and the switches, routers and
/// firewalls of its site. None that would close a loop.
async fn suggestions(
    db: &mut PgConnection,
    user: &CurrentUser,
    server: Uuid,
    edges: &[Edge],
) -> Result<Vec<Suggestion>, sqlx::Error> {
    let found: Vec<(Uuid, bool)> = sqlx::query_as(
        "SELECT h.id, true FROM server_proxmox_vms v
         JOIN servers me ON me.id = v.server_id
         JOIN sites ms ON ms.id = me.site_id
         JOIN servers h ON lower(split_part(h.hostname, '.', 1)) = lower(v.node)
                       AND h.id <> me.id AND h.revoked_at IS NULL
         JOIN sites hs ON hs.id = h.site_id AND hs.customer_id = ms.customer_id
         WHERE v.server_id = $1
         UNION ALL
         SELECT d.id, false FROM servers me
         JOIN servers d ON d.site_id = me.site_id AND d.id <> me.id AND d.kind = 'device'
                       AND d.device_template IN ('switch', 'router', 'firewall')
                       AND d.revoked_at IS NULL
         WHERE me.id = $1
         ORDER BY 2 DESC",
    )
    .bind(server)
    .fetch_all(&mut *db)
    .await?;
    let me = Node::Server(server);
    let set: HashSet<&Node> = edges
        .iter()
        .filter(|e| e.child == me)
        .map(|e| &e.parent)
        .collect();
    let mut nodes = Vec::new();
    for (id, proxmox) in found {
        let parent = Node::Server(id);
        if set.contains(&parent) || nodes.iter().any(|(n, _)| n == &parent) {
            continue;
        }
        if graph::cycle(edges, &[(me.clone(), parent.clone())]).is_some() {
            continue;
        }
        let reason = if proxmox {
            SuggestionReason::ProxmoxHost
        } else {
            SuggestionReason::NetworkDevice
        };
        nodes.push((parent, reason));
    }
    let places = places(&mut *db, nodes.iter().map(|(n, _)| n)).await?;
    let readable = user.readable_sites();
    Ok(nodes
        .into_iter()
        .filter_map(|(node, reason)| {
            let parent = end_view(&node, &places)?;
            readable
                .contains(parent.site_id)
                .then_some(Suggestion { parent, reason })
        })
        .collect())
}

/// `GET /api/servers/{id}/dependencies`. Any role on its site.
pub async fn server_page(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<PageView>, ApiError> {
    let id = parse_id(&id)?;
    let site = super::sensors::site_of_server(&state.pool, id).await?;
    let site = authorize(&user, site, false)?;
    Ok(Json(page(&state, &user, Node::Server(id), site).await?))
}

/// `GET /api/relays/{id}/dependencies`. Any role on its site.
pub async fn relay_page(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<PageView>, ApiError> {
    let id = parse_id(&id)?;
    let site = authorize(&user, site_of_relay(&state.pool, id).await?, false)?;
    Ok(Json(page(&state, &user, Node::Relay(id), site).await?))
}
