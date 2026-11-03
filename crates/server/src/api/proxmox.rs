//! `/api/servers/{id}/proxmox`: the Proxmox VE VM or container of a
//! server, which the remediation loop snapshots before change-tier
//! commands (ADR-0044, ADR-0093), and whether snapshots are available for
//! it.

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
use crate::remediation::snapshot::{GuestKind, Mapped, Support, Unsupported, VmRef};

/// Answer of the GET and PUT routes.
#[derive(Debug, Serialize)]
pub struct ProxmoxView {
    pub server_id: Uuid,
    pub mapping: Option<Mapping>,
    pub snapshots: SnapshotSupport,
    /// The configured clusters, to map the server to.
    pub clusters: Vec<String>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Mapping {
    pub cluster: String,
    pub node: String,
    #[sqlx(try_from = "i32")]
    pub vmid: u32,
    /// `qemu` for a VM, `lxc` for a container.
    #[sqlx(try_from = "String")]
    pub kind: KindColumn,
    pub updated_by: Option<Uuid>,
    pub updated_at: DateTime<Utc>,
    /// When Proxmox last answered whether it can snapshot the guest;
    /// null until it did.
    pub checked_at: Option<DateTime<Utc>>,
    /// Why Proxmox refuses snapshots of the guest, if it does.
    pub cannot_snapshot: Option<String>,
    /// What a snapshot of the guest does not hold, like a bind mount.
    pub left_out: Option<String>,
}

/// `server_proxmox_vms.kind`, which its check keeps to two values.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(transparent)]
pub struct KindColumn(pub GuestKind);

impl TryFrom<String> for KindColumn {
    type Error = String;

    fn try_from(kind: String) -> Result<Self, String> {
        GuestKind::parse(&kind)
            .map(Self)
            .ok_or_else(|| format!("unknown guest kind {kind}"))
    }
}

/// Whether the server can be snapshotted; without it the interface shows
/// that rollback is limited.
#[derive(Debug, Serialize)]
pub struct SnapshotSupport {
    pub available: bool,
    /// `disabled`, `not_mapped`, `unknown_cluster` or `cannot_snapshot`
    /// when unavailable.
    pub reason: Option<Unsupported>,
    /// The mapped guest's kind, so the reason can say VM or container.
    pub kind: Option<GuestKind>,
    /// With `cannot_snapshot`: what Proxmox cannot snapshot, like "rootfs
    /// is on storage local (dir), which cannot take snapshots".
    pub detail: Option<String>,
    /// What a snapshot does not hold, though Proxmox takes one.
    pub left_out: Option<String>,
}

impl SnapshotSupport {
    /// From a server's mapping, with what the last check found.
    pub fn of(settings: &ApiSettings, mapping: Option<Mapped>, left_out: Option<String>) -> Self {
        let kind = mapping.as_ref().map(|m| m.vm.kind);
        let detail = mapping.as_ref().and_then(|m| m.cannot_snapshot.clone());
        let support = Support::of(
            settings.proxmox_enabled,
            &settings.proxmox_clusters,
            mapping,
        );
        let unsupported = support.reason();
        Self {
            available: support.is_available(),
            reason: unsupported,
            kind,
            detail: detail.filter(|_| matches!(unsupported, Some(Unsupported::CannotSnapshot(_)))),
            left_out: left_out.filter(|_| support.is_available()),
        }
    }
}

/// Body of `PUT`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MappingRequest {
    pub cluster: String,
    pub node: String,
    pub vmid: u32,
    /// `qemu` (the default) or `lxc`.
    #[serde(default)]
    pub kind: GuestKind,
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
        kind: request.kind,
    };
    vm.check().map_err(|m| ApiError::Invalid(m.into()))?;
    if !state.settings.proxmox_clusters.contains(&vm.cluster) {
        return Err(ApiError::Invalid("unknown Proxmox cluster".into()));
    }
    let mut tx = state.pool.begin().await?;
    let previous: Option<(String, String, i32, String)> = sqlx::query_as(
        "SELECT cluster, node, vmid, kind FROM server_proxmox_vms
         WHERE server_id = $1 FOR UPDATE",
    )
    .bind(server)
    .fetch_optional(&mut *tx)
    .await?;
    // A new guest is checked again by the next pass (ADR-0093).
    sqlx::query(
        "INSERT INTO server_proxmox_vms (server_id, cluster, node, vmid, kind, updated_by)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (server_id) DO UPDATE
         SET cluster = $2, node = $3, vmid = $4, kind = $5, updated_by = $6, updated_at = now(),
             checked_at = NULL, cannot_snapshot = NULL, left_out = NULL",
    )
    .bind(server)
    .bind(&vm.cluster)
    .bind(&vm.node)
    .bind(vm.vmid as i32)
    .bind(vm.kind.as_str())
    .bind(user.id)
    .execute(&mut *tx)
    .await
    .map_err(|e| match e.as_database_error().and_then(|d| d.code()) {
        Some(code) if code == UNIQUE_VIOLATION => {
            ApiError::Conflict("the VM is already mapped to another server".into())
        }
        _ => ApiError::Database(e),
    })?;
    let previous = previous.map(|(cluster, node, vmid, kind)| {
        json!({ "cluster": cluster, "node": node, "vmid": vmid, "kind": kind })
    });
    record(
        &mut tx,
        &user,
        "server.proxmox.set",
        server,
        json!({ "cluster": vm.cluster, "node": vm.node, "vmid": vm.vmid, "kind": vm.kind,
                "previous": previous }),
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
    let (cluster, node, vmid, kind): (String, String, i32, String) = sqlx::query_as(
        "DELETE FROM server_proxmox_vms WHERE server_id = $1
         RETURNING cluster, node, vmid, kind",
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
        json!({ "cluster": cluster, "node": node, "vmid": vmid, "kind": kind }),
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
        "SELECT cluster, node, vmid, kind, updated_by, updated_at, checked_at,
                cannot_snapshot, left_out
         FROM server_proxmox_vms WHERE server_id = $1",
    )
    .bind(server)
    .fetch_optional(pool)
    .await?;
    let snapshots = SnapshotSupport::of(
        settings,
        mapping.as_ref().map(|m| Mapped {
            vm: VmRef {
                cluster: m.cluster.clone(),
                node: m.node.clone(),
                vmid: m.vmid,
                kind: m.kind.0,
            },
            cannot_snapshot: m.cannot_snapshot.clone(),
        }),
        mapping.as_ref().and_then(|m| m.left_out.clone()),
    );
    Ok(ProxmoxView {
        server_id: server,
        mapping,
        snapshots,
        clusters: if settings.proxmox_enabled {
            settings.proxmox_clusters.clone()
        } else {
            Vec::new()
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
