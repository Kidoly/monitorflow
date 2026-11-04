//! `GET /api/servers/{id}/rollback` (ADR-0094): what a change on the
//! server gets before it runs, in the order of [`choice::choose`]: a
//! Proxmox snapshot of its VM or container, else a local LVM or ZFS
//! snapshot, else nothing and why. With the local snapshots on the host
//! and when retention deletes them. Read-only, for anyone who sees the
//! server; the Proxmox mapping itself stays on `/proxmox`, for
//! administrators.
//!
//! [`choice::choose`]: crate::remediation::snapshot::choice::choose

use axum::extract::{Path, State};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use super::access::authorize;
use super::auth::CurrentUser;
use super::proxmox::SnapshotSupport;
use super::sensors::parse_id;
use super::{ApiError, ApiState};
use crate::remediation::snapshot::local::{self, SnapshotSet, StoredVolume};
use crate::remediation::snapshot::store;

#[derive(Debug, Serialize)]
pub struct RollbackView {
    pub server_id: Uuid,
    /// The same object as the approval queue's: `available`, `method`
    /// (`proxmox`, `lvm` or `zfs`), Proxmox's `reason` and `kind`, and
    /// `detail`, a sentence on what a local snapshot covers or why there
    /// is none.
    pub snapshots: SnapshotSupport,
    /// System mount points a local snapshot leaves out.
    pub not_covered: Vec<String>,
    /// The system volumes from the last report.
    pub volumes: Vec<StoredVolume>,
    /// MonitorFlow's local snapshots on the host, oldest first.
    pub local_snapshots: Vec<SnapshotSet>,
    /// When the agent last reported its storage.
    pub reported_at: Option<DateTime<Utc>>,
}

pub async fn get(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<RollbackView>, ApiError> {
    let id = parse_id(&id)?;
    let row: Option<(Uuid, Uuid, String)> = sqlx::query_as(
        "SELECT s.site_id, si.customer_id, s.os_family
         FROM servers s JOIN sites si ON si.id = s.site_id WHERE s.id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?;
    authorize(&user, row.as_ref().map(|r| r.0), false)?;
    let (site, customer, os_family) = row.ok_or(ApiError::NotFound)?;
    let clusters = state.settings.proxmox_clusters_for(customer, site);
    let mapping = store::mapping(&state.pool, id).await?;
    let left_out: Option<String> =
        sqlx::query_scalar("SELECT left_out FROM server_proxmox_vms WHERE server_id = $1")
            .bind(id)
            .fetch_optional(&state.pool)
            .await?
            .flatten();
    let storage = local::load(&state.pool, id).await?;
    let (report, reported_at) = storage.unzip();
    let snapshots = SnapshotSupport::of(&state.settings, &clusters, mapping, left_out)
        .with_local(&os_family, report.as_ref());
    let not_covered = match snapshots.method {
        Some("lvm" | "zfs") => local::plan_of(&os_family, report.as_ref())
            .map(|plan| plan.not_covered)
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    Ok(Json(RollbackView {
        server_id: id,
        snapshots,
        not_covered,
        volumes: report
            .as_ref()
            .map(|r| r.volumes.clone())
            .unwrap_or_default(),
        local_snapshots: report
            .as_ref()
            .map(|r| local::sets(&r.snapshots))
            .unwrap_or_default(),
        reported_at,
    }))
}
