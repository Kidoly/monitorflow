//! What a server's page shows of virtualization (ADR-0156): for a
//! vCenter, ESXi or Hyper-V source, its hosts, clusters, virtual machines
//! and datastores; for any server, the virtual machine it is.

use axum::extract::{Path, State};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use super::access::authorize;
use super::auth::CurrentUser;
use super::sensors::parse_id;
use super::{ApiError, ApiState};

/// Objects not seen for this long show "Not seen since".
pub const STALE_MINUTES: i64 = 15;

/// Whether a source was read at its last collection.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct SourceView {
    /// Whether the last collection worked.
    pub up: bool,
    /// Why not: `connect`, `tls`, `certificate`, `login`, `api` for a
    /// vCenter; `not_installed`, `denied`, `failed` for Hyper-V.
    pub reason: Option<String>,
    /// The message, in English, such as what to do about it.
    pub error: Option<String>,
    /// What answered: `product`, `version`, `api_type`, `host`.
    #[schema(value_type = Object)]
    pub details: Value,
    pub seen_at: DateTime<Utc>,
}

/// The server or device a virtual machine is.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct LinkedServer {
    pub id: Uuid,
    pub hostname: String,
    /// `agent` or `device`.
    pub kind: String,
    /// Whether it is waiting for its agent.
    pub awaiting_agent: bool,
}

/// One host, cluster, virtual machine or datastore.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ObjectView {
    pub id: Uuid,
    /// `vm`, `host`, `cluster` or `datastore`.
    pub kind: String,
    /// Its name, as its series' `instance`.
    pub name: String,
    /// Its ID at the source: a managed object ID, or Hyper-V's VM ID.
    pub key: String,
    /// What it says of itself: for a virtual machine `uuid`, `host`,
    /// `power_state`, `guest_hostname`, `guest_os`, `addresses`, `macs`,
    /// `replication_mode`, `replication_state`; for a host `cluster`,
    /// `connection_state`, `maintenance`, `product`, `vendor`, `model`;
    /// for a datastore `type`.
    #[schema(value_type = Object)]
    pub details: Value,
    /// Its numbers at the last collection, by measure without the check's
    /// and kind's prefix: `powered_on`, `cpu_usage_ratio`,
    /// `memory_bytes`, `snapshots`, `snapshot_age_seconds`,
    /// `free_ratio`, `alarms_critical`, and so on.
    #[schema(value_type = Object)]
    pub metrics: Value,
    pub collected_at: DateTime<Utc>,
    pub seen_at: DateTime<Utc>,
    /// Not seen for 15 minutes: the source stopped listing it.
    pub stale: bool,
    /// For a virtual machine, the server or device it is; `null` when
    /// none is known, or when the viewer cannot read it.
    pub server: Option<LinkedServer>,
    /// `uuid`, `mac`, `address`, `name`, or `person` when someone tied
    /// it by hand; `null` without a server.
    pub matched_by: Option<String>,
    /// Whether a person decided its server, even none.
    pub link_fixed: bool,
}

/// A virtual machine a server is, with where it runs.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct GuestOf {
    /// The virtual machine.
    pub object: ObjectView,
    /// The vCenter, ESXi or Hyper-V host listing it.
    pub source_id: Uuid,
    pub source_name: String,
}

/// Body of `GET /api/servers/{id}/virtualization`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct Virtualization {
    /// When the server is a source: its last collection; else `null`.
    pub source: Option<SourceView>,
    /// When the server is a source: its objects, hosts first, then
    /// clusters, virtual machines and datastores, by name.
    pub objects: Vec<ObjectView>,
    /// The virtual machines this server is, from sources the viewer can
    /// read: usually none or one.
    pub guest_of: Vec<GuestOf>,
}

type ObjectRow = (
    Uuid,
    String,
    String,
    String,
    Value,
    Value,
    DateTime<Utc>,
    DateTime<Utc>,
    Option<Uuid>,
    Option<String>,
    bool,
    Option<String>,
    Option<String>,
    Option<bool>,
    Option<Uuid>,
);

const OBJECT_COLUMNS: &str =
    "o.id, o.kind, o.instance, o.key, o.details, o.metrics, o.collected_at,
     o.seen_at, o.server_id, o.matched_by, o.link_fixed, l.hostname, l.kind, l.awaiting_agent,
     l.site_id";

fn object(row: ObjectRow, now: DateTime<Utc>, readable: &super::access::Scope) -> ObjectView {
    let (
        id,
        kind,
        name,
        key,
        details,
        metrics,
        collected_at,
        seen_at,
        server,
        matched_by,
        link_fixed,
        hostname,
        server_kind,
        awaiting,
        server_site,
    ) = row;
    let visible = server_site.is_some_and(|s| readable.contains(s));
    ObjectView {
        id,
        kind,
        name,
        key,
        details,
        metrics,
        collected_at,
        stale: (now - seen_at).num_minutes() >= STALE_MINUTES,
        seen_at,
        server: server.filter(|_| visible).map(|id| LinkedServer {
            id,
            hostname: hostname.unwrap_or_default(),
            kind: server_kind.unwrap_or_default(),
            awaiting_agent: awaiting.unwrap_or(false),
        }),
        matched_by: matched_by.filter(|_| visible),
        link_fixed,
    }
}

/// `GET /api/servers/{id}/virtualization`. Any role on the server's
/// site.
pub async fn get(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<Virtualization>, ApiError> {
    let id = parse_id(&id)?;
    let mut conn = state.pool.acquire().await?;
    let site: Option<Uuid> =
        sqlx::query_scalar("SELECT site_id FROM servers WHERE id = $1 AND revoked_at IS NULL")
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
    authorize(&user, site, false)?;
    let readable = user.readable_sites();
    let now = Utc::now();
    type SourceRow = (bool, Option<String>, Option<String>, Value, DateTime<Utc>);
    let source: Option<SourceRow> = sqlx::query_as(
        "SELECT up, reason, error, details, seen_at FROM virtual_sources WHERE server_id = $1",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    let rows: Vec<ObjectRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {OBJECT_COLUMNS} FROM virtual_objects o LEFT JOIN servers l ON l.id = o.server_id
         WHERE o.source_id = $1
         ORDER BY array_position(ARRAY['host', 'cluster', 'vm', 'datastore'], o.kind), lower(o.instance)"
    )))
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let (all, sites) = readable.bind();
    let guests: Vec<(String, Uuid, Uuid)> = sqlx::query_as(
        "SELECT s.hostname, s.id, o.id FROM virtual_objects o JOIN servers s ON s.id = o.source_id
         WHERE o.server_id = $1 AND o.kind = 'vm' AND s.revoked_at IS NULL
           AND ($2 OR s.site_id = ANY($3))
         ORDER BY o.seen_at DESC",
    )
    .bind(id)
    .bind(all)
    .bind(&sites)
    .fetch_all(&mut *conn)
    .await?;
    let mut guest_of = Vec::new();
    for (source_name, source_id, object_id) in guests {
        let row: ObjectRow = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {OBJECT_COLUMNS} FROM virtual_objects o LEFT JOIN servers l ON l.id = o.server_id
             WHERE o.id = $1"
        )))
        .bind(object_id)
        .fetch_one(&mut *conn)
        .await?;
        guest_of.push(GuestOf {
            object: object(row, now, &readable),
            source_id,
            source_name,
        });
    }
    Ok(Json(Virtualization {
        source: source.map(|(up, reason, error, details, seen_at)| SourceView {
            up,
            reason,
            error,
            details,
            seen_at,
        }),
        objects: rows
            .into_iter()
            .map(|r| object(r, now, &readable))
            .collect(),
        guest_of,
    }))
}
