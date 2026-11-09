//! `/api/servers/{id}/proxmox`: the Proxmox VE VM or container of a
//! server, which the remediation loop snapshots before change-tier
//! commands (ADR-0044, ADR-0093), and whether snapshots are available for
//! it. Discovery maps most servers by itself; a mapping by hand overrides
//! it, and unmapping tells it the guest is not this server's.

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
use super::notifications::is_global_admin;
use super::sensors::{body, parse_id, site_of_server, UNIQUE_VIOLATION};
use super::{ApiError, ApiSettings, ApiState};
use crate::remediation::snapshot::discovery::Owner;
use crate::remediation::snapshot::local::{self, Storage};
use crate::remediation::snapshot::{choice, GuestKind, Mapped, Support, Unsupported, VmRef};

/// Guests listed at most for the picker.
const MAX_GUESTS: i64 = 2000;

/// To an administrator of one customer who names, on a cluster serving
/// every customer, a guest that discovery did not match to the server.
/// The same answer whether the guest exists, is free or is mapped.
const NOT_MATCHED: &str = "this Proxmox cluster serves several customers: choose one of the     guests matched to this server, or ask a global administrator to map it";

/// Answer of the GET and PUT routes.
#[derive(Debug, Serialize)]
pub struct ProxmoxView {
    pub server_id: Uuid,
    pub mapping: Option<Mapping>,
    pub snapshots: SnapshotSupport,
    /// The configured clusters that serve the server, to map it to.
    pub clusters: Vec<String>,
    /// Guests that match the server equally, when discovery could not
    /// choose: the page asks which one.
    pub candidates: Vec<Guest>,
}

/// A guest of a cluster, as the last discovery pass saw it.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Guest {
    pub cluster: String,
    pub node: String,
    #[sqlx(try_from = "i32")]
    pub vmid: u32,
    #[sqlx(try_from = "String")]
    pub kind: KindColumn,
    /// The VM's name or the container's host name.
    pub name: String,
    /// Among candidates, what matched; elsewhere null.
    #[sqlx(default)]
    pub matched_by: Option<String>,
    /// Mapped to another server already.
    #[sqlx(default)]
    pub taken: bool,
}

/// `GET /api/servers/{id}/proxmox/guests`.
#[derive(Debug, Serialize)]
pub struct GuestList {
    pub guests: Vec<Guest>,
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
    /// Null when mapped by hand, else what discovery matched: `uuid`,
    /// `mac` or `hostname`.
    pub matched_by: Option<String>,
    /// The guest's name, once discovery saw it.
    pub name: Option<String>,
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
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct SnapshotSupport {
    pub available: bool,
    /// `disabled`, `not_mapped`, `unknown_cluster` or `cannot_snapshot`
    /// when unavailable.
    #[schema(value_type = Option<String>)]
    pub reason: Option<Unsupported>,
    /// The mapped guest's kind, so the reason can say VM or container.
    pub kind: Option<GuestKind>,
    /// With `cannot_snapshot`: what Proxmox cannot snapshot, like "rootfs
    /// is on storage local (dir), which cannot take snapshots".
    pub detail: Option<String>,
    /// What a snapshot does not hold, though Proxmox takes one.
    pub left_out: Option<String>,
    /// Which snapshot a change gets when available: `proxmox`, or a local
    /// `lvm` or `zfs` snapshot (ADR-0094).
    pub method: Option<&'static str>,
}

impl SnapshotSupport {
    /// From a server's mapping, with what the last check found, and the
    /// clusters that serve the server.
    pub fn of(
        settings: &ApiSettings,
        clusters: &[String],
        mapping: Option<Mapped>,
        left_out: Option<String>,
    ) -> Self {
        let kind = mapping.as_ref().map(|m| m.vm.kind);
        let detail = mapping.as_ref().and_then(|m| m.cannot_snapshot.clone());
        let support = Support::of(settings.proxmox_enabled, clusters, mapping);
        let unsupported = support.reason();
        Self {
            available: support.is_available(),
            reason: unsupported,
            kind,
            detail: detail.filter(|_| matches!(unsupported, Some(Unsupported::CannotSnapshot(_)))),
            left_out: left_out.filter(|_| support.is_available()),
            method: support.is_available().then_some("proxmox"),
        }
    }

    /// Where Proxmox has no say (off, or the server is not one of its
    /// guests), a local snapshot from the server's storage report, in the
    /// order the remediation hook follows ([`choice::choose`], ADR-0094).
    pub fn with_local(mut self, os_family: &str, storage: Option<&Storage>) -> Self {
        let Some(reason) = self.reason.filter(|r| !choice::proxmox_decides(*r)) else {
            return self;
        };
        match local::plan_of(os_family, storage) {
            Ok(plan) => {
                self.available = true;
                self.method = Some(plan.method);
                self.detail = Some(plan.summary);
            }
            Err(why) => self.detail = Some(why.to_string()),
        }
        self.reason = Some(reason);
        self
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
    let target = authorize(&state, &user, &id).await?;
    Ok(Json(view(&state.pool, &state.settings, &target).await?))
}

/// `GET /api/servers/{id}/proxmox/guests`: the guests the server may be
/// mapped to, for the picker. A cluster that serves every customer is
/// listed to global administrators only, as its guests belong to all.
pub async fn guests(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<GuestList>, ApiError> {
    let target = authorize(&state, &user, &id).await?;
    let global = is_global_admin(&user);
    let clusters: Vec<String> = target
        .clusters(&state.settings)
        .into_iter()
        .filter(|c| global || state.settings.proxmox_owner(c) != Owner::Everyone)
        .collect();
    let guests: Vec<Guest> = sqlx::query_as(
        "SELECT g.cluster, g.node, g.vmid, g.kind, g.name,
                (v.server_id IS NOT NULL AND v.server_id <> $2) AS taken
         FROM proxmox_guests g
         LEFT JOIN server_proxmox_vms v ON v.cluster = g.cluster AND v.vmid = g.vmid
         WHERE g.cluster = ANY($1)
         ORDER BY g.cluster, g.vmid
         LIMIT $3",
    )
    .bind(&clusters)
    .bind(target.server)
    .bind(MAX_GUESTS)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(GuestList { guests }))
}

/// `PUT /api/servers/{id}/proxmox`: sets or replaces the mapping, by hand.
/// On a cluster that serves every customer, only a global administrator
/// names any guest; others one of the server's candidates, else 400.
pub async fn set(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<MappingRequest>, JsonRejection>,
) -> Result<Json<ProxmoxView>, ApiError> {
    let target = authorize(&state, &user, &id).await?;
    let server = target.server;
    let request = body(request)?;
    let vm = VmRef {
        cluster: request.cluster,
        node: request.node,
        vmid: request.vmid,
        kind: request.kind,
    };
    vm.check().map_err(|m| ApiError::Invalid(m.into()))?;
    // A cluster of another customer is as unknown as a missing one.
    if !target.clusters(&state.settings).contains(&vm.cluster) {
        return Err(ApiError::Invalid("unknown Proxmox cluster".into()));
    }
    let mut tx = state.pool.begin().await?;
    // A cluster that serves every customer holds the guests of all of
    // them, and a mapping is what gets snapshotted before each AI change.
    // Only a global administrator names a guest there; the others choose
    // among those discovery matched to the server (ADR-0093). Checked
    // before the insert, whose conflict would say which IDs are mapped.
    if by_candidate_only(
        state.settings.proxmox_owner(&vm.cluster),
        is_global_admin(&user),
    ) {
        let matched: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM server_proxmox_candidates
                            WHERE server_id = $1 AND cluster = $2 AND vmid = $3)",
        )
        .bind(server)
        .bind(&vm.cluster)
        .bind(vm.vmid as i32)
        .fetch_one(&mut *tx)
        .await?;
        if !matched {
            return Err(ApiError::Invalid(NOT_MATCHED.into()));
        }
    }
    let previous: Option<(String, String, i32, String, Option<String>)> = sqlx::query_as(
        "SELECT cluster, node, vmid, kind, matched_by FROM server_proxmox_vms
         WHERE server_id = $1 FOR UPDATE",
    )
    .bind(server)
    .fetch_optional(&mut *tx)
    .await?;
    // A new guest is checked again by the next pass, and discovery leaves
    // a mapping by hand alone (ADR-0093).
    sqlx::query(
        "INSERT INTO server_proxmox_vms (server_id, cluster, node, vmid, kind, updated_by)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (server_id) DO UPDATE
         SET cluster = $2, node = $3, vmid = $4, kind = $5, updated_by = $6, updated_at = now(),
             checked_at = NULL, cannot_snapshot = NULL, left_out = NULL, matched_by = NULL",
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
    sqlx::query(
        "DELETE FROM server_proxmox_rejections
         WHERE server_id = $1 AND cluster = $2 AND vmid = $3",
    )
    .bind(server)
    .bind(&vm.cluster)
    .bind(vm.vmid as i32)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM server_proxmox_candidates WHERE server_id = $1")
        .bind(server)
        .execute(&mut *tx)
        .await?;
    let previous = previous.map(|(cluster, node, vmid, kind, matched_by)| {
        json!({ "cluster": cluster, "node": node, "vmid": vmid, "kind": kind,
                "matched_by": matched_by })
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
    Ok(Json(view(&state.pool, &state.settings, &target).await?))
}

/// `DELETE /api/servers/{id}/proxmox`: 204, or 404 without a mapping.
/// Snapshots already taken stay on the guest. Discovery never maps the
/// server to that guest again, unless it is mapped by hand.
pub async fn clear(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let server = authorize(&state, &user, &id).await?.server;
    let mut tx = state.pool.begin().await?;
    let (cluster, node, vmid, kind, matched_by): (String, String, i32, String, Option<String>) =
        sqlx::query_as(
            "DELETE FROM server_proxmox_vms WHERE server_id = $1
             RETURNING cluster, node, vmid, kind, matched_by",
        )
        .bind(server)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ApiError::NotFound)?;
    sqlx::query(
        "INSERT INTO server_proxmox_rejections (server_id, cluster, vmid, rejected_by)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (server_id, cluster, vmid) DO UPDATE
         SET rejected_by = $4, rejected_at = now()",
    )
    .bind(server)
    .bind(&cluster)
    .bind(vmid)
    .bind(user.id)
    .execute(&mut *tx)
    .await?;
    record(
        &mut tx,
        &user,
        "server.proxmox.clear",
        server,
        json!({ "cluster": cluster, "node": node, "vmid": vmid, "kind": kind,
                "matched_by": matched_by }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Whether a mapping by hand on a cluster of `owner` is limited to the
/// guests discovery matched to the server: on a cluster without an owner,
/// for everyone but global administrators.
fn by_candidate_only(owner: Owner, global_admin: bool) -> bool {
    owner == Owner::Everyone && !global_admin
}

/// A server whose customer the user administers.
struct Target {
    server: Uuid,
    customer: Uuid,
    site: Uuid,
}

impl Target {
    /// The configured clusters that serve the server.
    fn clusters(&self, settings: &ApiSettings) -> Vec<String> {
        settings.proxmox_clusters_for(self.customer, self.site)
    }
}

/// The server once the user administers its customer.
async fn authorize(state: &ApiState, user: &CurrentUser, id: &str) -> Result<Target, ApiError> {
    let server = parse_id(id)?;
    let site = site_of_server(&state.pool, server).await?;
    let site = authorize_admin(user, site)?;
    let customer = sqlx::query_scalar("SELECT customer_id FROM sites WHERE id = $1")
        .bind(site)
        .fetch_one(&state.pool)
        .await?;
    Ok(Target {
        server,
        customer,
        site,
    })
}

async fn view(
    pool: &PgPool,
    settings: &ApiSettings,
    target: &Target,
) -> Result<ProxmoxView, sqlx::Error> {
    let server = target.server;
    let clusters = target.clusters(settings);
    let mapping: Option<Mapping> = sqlx::query_as(
        "SELECT v.cluster, v.node, v.vmid, v.kind, v.updated_by, v.updated_at, v.checked_at,
                v.cannot_snapshot, v.left_out, v.matched_by, g.name
         FROM server_proxmox_vms v
         LEFT JOIN proxmox_guests g ON g.cluster = v.cluster AND g.vmid = v.vmid
         WHERE v.server_id = $1",
    )
    .bind(server)
    .fetch_optional(pool)
    .await?;
    let candidates: Vec<Guest> = sqlx::query_as(
        "SELECT g.cluster, g.node, g.vmid, g.kind, g.name, c.matched_by
         FROM server_proxmox_candidates c
         JOIN proxmox_guests g ON g.cluster = c.cluster AND g.vmid = c.vmid
         WHERE c.server_id = $1 AND c.cluster = ANY($2)
         ORDER BY g.cluster, g.vmid",
    )
    .bind(server)
    .bind(&clusters)
    .fetch_all(pool)
    .await?;
    let snapshots = SnapshotSupport::of(
        settings,
        &clusters,
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
            clusters
        } else {
            Vec::new()
        },
        candidates: candidates
            .into_iter()
            .filter(|_| settings.proxmox_enabled)
            .collect(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_global_administrators_name_any_guest_of_a_shared_cluster() {
        let (customer, site) = (Uuid::from_u128(1), Uuid::from_u128(2));
        assert!(by_candidate_only(Owner::Everyone, false));
        assert!(!by_candidate_only(Owner::Everyone, true));
        // A cluster with an owner serves only that customer or site, whose
        // administrators map as before.
        assert!(!by_candidate_only(Owner::Customer(customer), false));
        assert!(!by_candidate_only(Owner::Site(site), false));
    }
}
