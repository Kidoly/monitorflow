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
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::sensors::{parse_id, RunBy};
use super::{ApiError, ApiState};
use crate::vmware;

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

/// Body of `POST /api/servers/{id}/hyperv`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct HypervWatch {
    /// Whether the check was added now; `false` when it was there.
    pub added: bool,
    /// The check's name, so also its metrics' prefix: `hyperv`.
    pub check: String,
}

/// `POST /api/servers/{id}/hyperv`: gives a Windows server the `hyperv`
/// check every 5 minutes, with its alerts. Operators of its site.
pub async fn watch_hyperv(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<HypervWatch>, ApiError> {
    let id = parse_id(&id)?;
    let mut tx = state.pool.begin().await?;
    let row: Option<(Uuid, String, String)> = sqlx::query_as(
        "SELECT site_id, kind, os_family FROM servers WHERE id = $1 AND revoked_at IS NULL FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    authorize(&user, row.as_ref().map(|r| r.0), true)?;
    let (_, kind, os) = row.ok_or(ApiError::NotFound)?;
    if kind != "agent" || os != "windows" {
        return Err(ApiError::Invalid(
            "Hyper-V is read by the agent of a Windows server".into(),
        ));
    }
    let added = vmware::add_check(
        &mut tx,
        id,
        "hyperv",
        vmware::HYPERV_CHECK,
        &RunBy::default(),
        &vmware::HYPERV_THRESHOLDS,
    )
    .await?;
    let actor = user.id.to_string();
    let target = id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: "server.hyperv_watch",
            target: Some(("server", &target)),
            details: serde_json::json!({ "added": added }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(Json(HypervWatch {
        added,
        check: vmware::HYPERV_CHECK.into(),
    }))
}

/// Body of `PUT /api/virtual-objects/{id}/server`.
#[derive(Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LinkRequest {
    /// The server or device the virtual machine is, of the source's
    /// customer; `null` for none. Kept until changed: matching leaves it.
    #[serde(default)]
    pub server_id: Option<Uuid>,
    /// `true` hands the virtual machine back to automatic matching;
    /// `server_id` is then ignored.
    #[serde(default)]
    pub automatic: bool,
}

/// The virtual machine `id` locked, with its source's site and customer,
/// after checking the user's right on that site.
async fn vm_of(
    tx: &mut sqlx::PgConnection,
    user: &CurrentUser,
    id: Uuid,
    admin: bool,
) -> Result<
    (
        Uuid,
        Uuid,
        Uuid,
        String,
        Value,
        Option<Uuid>,
        Option<String>,
    ),
    ApiError,
> {
    type Row = (
        Uuid,
        Uuid,
        Uuid,
        String,
        Value,
        Option<Uuid>,
        Option<String>,
        String,
    );
    let row: Option<Row> = sqlx::query_as(
        "SELECT o.source_id, s.site_id, si.customer_id, o.instance, o.details, o.server_id,
                o.matched_by, o.kind
         FROM virtual_objects o JOIN servers s ON s.id = o.source_id JOIN sites si ON si.id = s.site_id
         WHERE o.id = $1 AND s.revoked_at IS NULL
         FOR UPDATE OF o",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let site = row.as_ref().map(|r| r.1);
    if admin {
        super::access::authorize_admin(user, site)?;
    } else {
        authorize(user, site, true)?;
    }
    let (source, site, customer, instance, details, server, by, kind) =
        row.ok_or(ApiError::NotFound)?;
    if kind != "vm" {
        return Err(ApiError::Invalid(
            "only a virtual machine is a server".into(),
        ));
    }
    Ok((source, site, customer, instance, details, server, by))
}

fn record_event<'a>(actor: &'a str, action: &'a str, target: &'a str, details: Value) -> Event<'a> {
    Event {
        actor_kind: "user",
        actor_id: actor,
        action,
        target: Some(("virtual_object", target)),
        details,
    }
}

/// `PUT /api/virtual-objects/{id}/server`: ties a virtual machine to a
/// server by hand, to none, or back to matching. Operators of the
/// source's site, for a server they can read.
pub async fn link(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<LinkRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<ObjectView>, ApiError> {
    let id = parse_id(&id)?;
    let request = super::sensors::body(request)?;
    let mut tx = state.pool.begin().await?;
    let (source, _, customer, _, _, before, before_by) = vm_of(&mut tx, &user, id, false).await?;
    let wanted = if request.automatic {
        None
    } else {
        request.server_id
    };
    if let Some(server) = wanted {
        let found: Option<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT s.site_id, si.customer_id FROM servers s JOIN sites si ON si.id = s.site_id
             WHERE s.id = $1 AND s.revoked_at IS NULL",
        )
        .bind(server)
        .fetch_optional(&mut *tx)
        .await?;
        let readable = user.readable_sites();
        match found {
            Some((site, of)) if readable.contains(site) && of == customer && server != source => {}
            _ => {
                return Err(ApiError::Invalid(
                    "`server_id` must be a server or device of the same customer as the virtual machine's source".into(),
                ))
            }
        }
    }
    sqlx::query(
        "UPDATE virtual_objects SET server_id = $2, matched_by = CASE WHEN $2 IS NULL THEN NULL ELSE 'person' END,
                link_fixed = $3
         WHERE id = $1",
    )
    .bind(id)
    .bind(wanted)
    .bind(!request.automatic)
    .execute(&mut *tx)
    .await?;
    let actor = user.id.to_string();
    let target = id.to_string();
    audit::record(
        &mut *tx,
        record_event(
            &actor,
            "virt.link",
            &target,
            serde_json::json!({
                "before": {"server_id": before, "matched_by": before_by},
                "after": {"server_id": wanted, "automatic": request.automatic},
            }),
        ),
    )
    .await?;
    tx.commit().await?;
    if request.automatic {
        crate::virt::match_source(&state.pool, source).await?;
    }
    one(&state, &user, id).await.map(Json)
}

/// One object as the API shows it.
async fn one(state: &ApiState, user: &CurrentUser, id: Uuid) -> Result<ObjectView, ApiError> {
    let row: ObjectRow = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {OBJECT_COLUMNS} FROM virtual_objects o LEFT JOIN servers l ON l.id = o.server_id
         WHERE o.id = $1"
    )))
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(ApiError::NotFound)?;
    Ok(object(row, Utc::now(), &user.readable_sites()))
}

/// The name a server waiting for its agent gets from a virtual machine:
/// its guest host name, else its name, as a host name would read.
pub fn expected_name(instance: &str, details: &Value) -> String {
    let guest = details
        .get("guest_hostname")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|g| !g.is_empty());
    let name = guest.unwrap_or(instance);
    // `web (vm-42)` when two share a name: the part before.
    let name = name.split(" (").next().unwrap_or(name);
    name.split('.').next().unwrap_or(name).trim().to_owned()
}

/// `POST /api/virtual-objects/{id}/install-agent`: adds a server waiting
/// for its agent, named after the virtual machine, in its source's site,
/// and ties them. 201 with the server; its page shows the install
/// command. Administrators of the source's customer, who make install
/// keys.
pub async fn install_agent(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<(axum::http::StatusCode, Json<super::servers::ServerView>), ApiError> {
    let id = parse_id(&id)?;
    let mut tx = state.pool.begin().await?;
    let (_, site, _, instance, details, server, _) = vm_of(&mut tx, &user, id, true).await?;
    if server.is_some() {
        return Err(ApiError::Conflict(
            "this virtual machine is already a server".into(),
        ));
    }
    let name = expected_name(&instance, &details);
    if name.is_empty() {
        return Err(ApiError::Invalid(
            "this virtual machine has no name to give a server".into(),
        ));
    }
    let windows = details
        .get("guest_os")
        .and_then(Value::as_str)
        .is_some_and(|os| os.to_ascii_lowercase().contains("windows"));
    crate::enroll::identity::lock(&mut tx).await?;
    let base = crate::enroll::install_key::peer_id_from_hostname(&name);
    let peer_id = crate::enroll::install_key::new_id(&mut tx, &base)
        .await
        .map_err(|e| ApiError::Conflict(e.to_string()))?;
    let server: Uuid = sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family, awaiting_agent, package_updates)
         VALUES ($1, $2, $3, $4, true, false) RETURNING id",
    )
    .bind(&peer_id)
    .bind(site)
    .bind(&name)
    .bind(if windows { "windows" } else { "linux" })
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE virtual_objects SET server_id = $2, matched_by = 'person', link_fixed = true WHERE id = $1",
    )
    .bind(id)
    .bind(server)
    .execute(&mut *tx)
    .await?;
    let actor = user.id.to_string();
    let target = id.to_string();
    audit::record(
        &mut *tx,
        record_event(
            &actor,
            "virt.install_agent",
            &target,
            serde_json::json!({"server_id": server, "peer_id": peer_id, "site_id": site}),
        ),
    )
    .await?;
    tx.commit().await?;
    let view = super::servers::view(&state, server)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok((axum::http::StatusCode::CREATED, Json(view)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expected_names_read_as_host_names() {
        assert_eq!(expected_name("web-01", &serde_json::json!({})), "web-01");
        assert_eq!(
            expected_name(
                "vm",
                &serde_json::json!({"guest_hostname": "db-1.corp.example"})
            ),
            "db-1"
        );
        assert_eq!(
            expected_name("web (vm-42)", &serde_json::json!({"guest_hostname": " "})),
            "web"
        );
    }
}
