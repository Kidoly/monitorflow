//! `/api/servers/{id}/proxmox`: the Proxmox VE VM of a server, which the
//! remediation loop snapshots before change-tier commands (ADR-0044), and
//! whether snapshots are available for it.

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use super::access::authorize_admin;
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::sensors::{body, parse_id, site_of_server, UNIQUE_VIOLATION};
use super::{ApiError, ApiSettings, ApiState};
use crate::remediation::snapshot::{Support, Unsupported, VmRef};

/// Answer of the GET and PUT routes.
#[derive(Debug, Serialize)]
pub struct ProxmoxView {
    pub server_id: Uuid,
    pub mapping: Option<Mapping>,
    pub snapshots: SnapshotSupport,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Mapping {
    pub cluster: String,
    pub node: String,
    #[sqlx(try_from = "i32")]
    pub vmid: u32,
    pub updated_by: Option<Uuid>,
    pub updated_at: DateTime<Utc>,
}

/// Whether the server can be snapshotted; without it the interface shows
/// that rollback is limited.
#[derive(Debug, Serialize)]
pub struct SnapshotSupport {
    pub available: bool,
    /// `disabled`, `not_mapped` or `unknown_cluster` when unavailable.
    pub reason: Option<Unsupported>,
}

/// Body of `PUT`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MappingRequest {
    pub cluster: String,
    pub node: String,
    pub vmid: u32,
}

/// `GET /api/servers/{id}/proxmox`: administrators of the customer.
pub async fn get(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<ProxmoxView>, ApiError> {
    let server = authorize(&state, &user, &id).await?;
    Ok(Json(view(&state.pool, &state.settings, server).await?))
}

/// `PUT /api/servers/{id}/proxmox`: sets or replaces the mapping.
pub async fn set(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<MappingRequest>, JsonRejection>,
) -> Result<Json<ProxmoxView>, ApiError> {
    let server = authorize(&state, &user, &id).await?;
    let request = body(request)?;
    let vm = VmRef {
        cluster: request.cluster,
        node: request.node,
        vmid: request.vmid,
    };
    vm.check().map_err(|m| ApiError::Invalid(m.into()))?;
    if !state.settings.proxmox_clusters.contains(&vm.cluster) {
        return Err(ApiError::Invalid("unknown Proxmox cluster".into()));
    }
    let mut tx = state.pool.begin().await?;
    let previous: Option<(String, String, i32)> = sqlx::query_as(
        "SELECT cluster, node, vmid FROM server_proxmox_vms WHERE server_id = $1 FOR UPDATE",
    )
    .bind(server)
    .fetch_optional(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO server_proxmox_vms (server_id, cluster, node, vmid, updated_by)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (server_id) DO UPDATE
         SET cluster = $2, node = $3, vmid = $4, updated_by = $5, updated_at = now()",
    )
    .bind(server)
    .bind(&vm.cluster)
    .bind(&vm.node)
    .bind(vm.vmid as i32)
    .bind(user.id)
    .execute(&mut *tx)
    .await
    .map_err(|e| match e.as_database_error().and_then(|d| d.code()) {
        Some(code) if code == UNIQUE_VIOLATION => {
            ApiError::Conflict("the VM is already mapped to another server".into())
        }
        _ => ApiError::Database(e),
    })?;
    let previous = previous
        .map(|(cluster, node, vmid)| json!({ "cluster": cluster, "node": node, "vmid": vmid }));
    record(
        &mut tx,
        &user,
        "server.proxmox.set",
        server,
        json!({ "cluster": vm.cluster, "node": vm.node, "vmid": vm.vmid, "previous": previous }),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(view(&state.pool, &state.settings, server).await?))
}

/// `DELETE /api/servers/{id}/proxmox`: 204, or 404 without a mapping.
/// Snapshots already taken stay on the VM.
pub async fn clear(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let server = authorize(&state, &user, &id).await?;
    let mut tx = state.pool.begin().await?;
    let (cluster, node, vmid): (String, String, i32) = sqlx::query_as(
        "DELETE FROM server_proxmox_vms WHERE server_id = $1 RETURNING cluster, node, vmid",
    )
    .bind(server)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        &user,
        "server.proxmox.clear",
        server,
        json!({ "cluster": cluster, "node": node, "vmid": vmid }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The server's ID once the user administers its customer.
async fn authorize(state: &ApiState, user: &CurrentUser, id: &str) -> Result<Uuid, ApiError> {
    let server = parse_id(id)?;
    let site = site_of_server(&state.pool, server).await?;
    authorize_admin(user, site)?;
    Ok(server)
}

async fn view(
    pool: &PgPool,
    settings: &ApiSettings,
    server: Uuid,
) -> Result<ProxmoxView, sqlx::Error> {
    let mapping: Option<Mapping> = sqlx::query_as(
        "SELECT cluster, node, vmid, updated_by, updated_at
         FROM server_proxmox_vms WHERE server_id = $1",
    )
    .bind(server)
    .fetch_optional(pool)
    .await?;
    let support = Support::of(
        settings.proxmox_enabled,
        &settings.proxmox_clusters,
        mapping.as_ref().map(|m| VmRef {
            cluster: m.cluster.clone(),
            node: m.node.clone(),
            vmid: m.vmid,
        }),
    );
    Ok(ProxmoxView {
        server_id: server,
        mapping,
        snapshots: SnapshotSupport {
            available: support.is_available(),
            reason: support.reason(),
        },
    })
}

async fn record(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user: &CurrentUser,
    action: &str,
    server: Uuid,
    details: serde_json::Value,
) -> Result<(), sqlx::Error> {
    let actor = user.id.to_string();
    audit::record(
        &mut **tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action,
            target: Some(("server", &server.to_string())),
            details,
        },
    )
    .await
}
