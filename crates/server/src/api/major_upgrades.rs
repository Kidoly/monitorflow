//! `GET /api/major-upgrades` (ADR-0182): for the servers a user sees, the
//! next release of their distribution, every check before upgrading to it
//! (the agent's, and the central server's own: the host's consent, the
//! agent's version and the mirrors), the rollback they would get, and the
//! progress of their last upgrade. Upgrades themselves are campaigns.

use std::collections::{BTreeMap, HashSet};

use axum::extract::{Query, State};
use axum::Json;
use chrono::{DateTime, Utc};
use monitorflow_common::command::MAJOR_UPGRADE;
use monitorflow_common::major_upgrade::{self, Method, FAILED, OK};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::auth::CurrentUser;
use super::proxmox::SnapshotSupport;
use super::{package_sources, rollback, ApiError, ApiState};
use crate::commands::major_upgrade::{Check, Progress, Stored};

/// What `GET /api/major-upgrades` filters on; every server the user sees
/// without any.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct Filter {
    pub customer_id: Option<Uuid>,
    pub site_id: Option<Uuid>,
    pub server_id: Option<Uuid>,
}

/// One server and its next release.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ServerUpgrade {
    pub server_id: Uuid,
    pub hostname: String,
    pub site_id: Uuid,
    pub customer_id: Uuid,
    /// What the server runs, like `debian 12`.
    pub release: String,
    /// "Debian 12 (bookworm)", when the release is one MonitorFlow can
    /// upgrade from.
    pub release_label: Option<String>,
    /// The next release, like `debian 13`; `null` when none is known.
    pub target: Option<String>,
    pub target_label: Option<String>,
    /// No check failed: a campaign may upgrade it.
    pub ready: bool,
    /// The central server's checks first, then the agent's.
    pub checks: Vec<Check>,
    /// The snapshot the upgrade gets first, or why none (ADR-0094).
    pub rollback: SnapshotSupport,
    /// The last major upgrade of the server.
    pub progress: Option<Progress>,
    /// When the agent last reported its checks.
    pub reported_at: Option<DateTime<Utc>>,
    /// The user may start it: an administrator of its customer.
    pub can_upgrade: bool,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct MajorUpgradeList {
    pub servers: Vec<ServerUpgrade>,
}

#[derive(Debug, sqlx::FromRow)]
struct Row {
    id: Uuid,
    hostname: String,
    site_id: Uuid,
    customer_id: Uuid,
    os_family: String,
    os_version: Option<String>,
    command_kinds: Option<sqlx::types::Json<BTreeMap<String, String>>>,
    report: Option<sqlx::types::Json<Stored>>,
    reported_at: Option<DateTime<Utc>>,
}

/// The central server's checks of a server whose agent reports
/// `acceptance` for `major_upgrade` and `stored`, for an upgrade to
/// `target`. `mirror` is `None` when the server uses its own sources,
/// else what the mirrors lack, if anything.
pub fn server_checks(
    acceptance: Option<&str>,
    stored: Option<&Stored>,
    target: &str,
    mirror: Option<Option<String>>,
) -> Vec<Check> {
    let allowed = match acceptance {
        Some("accepted") => Check::new("allowed", OK, "The server allows major upgrades from the web interface."),
        Some("off") => Check::new(
            "allowed",
            FAILED,
            "The server does not allow major upgrades from the web interface: run `monitorflow-agent major-upgrades on` there as root, or install it again with MF_MAJOR_UPGRADES_FROM_WEB=1.",
        ),
        Some(_) => Check::new("allowed", FAILED, "This server's agent cannot run major upgrades."),
        None => Check::new(
            "allowed",
            FAILED,
            "The agent of this server is too old for major upgrades; it updates by itself.",
        ),
    };
    let agent = match stored {
        Some(s) if s.target == target => {
            Check::new("agent_version", OK, "The agent knows the next release.")
        }
        Some(_) | None => Check::new(
            "agent_version",
            FAILED,
            "The agent of this server does not report the next release yet; update the agent.",
        ),
    };
    let mirror = match mirror {
        None => Check::new(
            "mirror",
            OK,
            "The server takes its packages from its own sources.",
        ),
        Some(None) => Check::new("mirror", OK, "The mirrors carry the next release."),
        Some(Some(problem)) => Check::new("mirror", FAILED, problem),
    };
    vec![allowed, agent, mirror]
}

/// The catalog's mirror of `release`, like `debian-13`.
fn mirror_id(release: &str) -> String {
    release.replace(' ', "-")
}

/// The servers of `ids` (or all the user reads, filtered), with their
/// readiness. Revoked servers and servers outside the Linux families are
/// left out.
pub(crate) async fn readiness(
    state: &ApiState,
    user: &CurrentUser,
    filter: &Filter,
    ids: Option<&[Uuid]>,
) -> Result<Vec<ServerUpgrade>, ApiError> {
    let (all, sites) = user.readable_sites().bind();
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT s.id, s.hostname, s.site_id, si.customer_id, s.os_family, s.os_version,
             s.command_kinds, m.report, m.reported_at
         FROM servers s JOIN sites si ON si.id = s.site_id
         LEFT JOIN server_major_upgrades m ON m.server_id = s.id
         WHERE s.revoked_at IS NULL AND s.kind = 'agent' AND s.os_family IN ('debian', 'rhel')
           AND ($1 OR s.site_id = ANY($2))
           AND ($3::uuid IS NULL OR si.customer_id = $3)
           AND ($4::uuid IS NULL OR s.site_id = $4)
           AND ($5::uuid IS NULL OR s.id = $5)
           AND ($6::uuid[] IS NULL OR s.id = ANY($6))
         ORDER BY s.hostname, s.id",
    )
    .bind(all)
    .bind(&sites)
    .bind(filter.customer_id)
    .bind(filter.site_id)
    .bind(filter.server_id)
    .bind(ids)
    .fetch_all(&state.pool)
    .await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let on_mirror: HashSet<Uuid> = package_sources::using_mirror(&state.pool, &ids).await?;
    let mut problems: BTreeMap<String, Option<String>> = BTreeMap::new();
    let administered = user.administered_customers();
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let release = row.os_version.clone().unwrap_or_default();
        let known = major_upgrade::next(&release);
        let stored = row.report.as_ref().map(|r| &r.0);
        if known.is_none() && stored.and_then(|s| s.progress.as_ref()).is_none() {
            continue;
        }
        let (rollback, _) =
            rollback::support(state, row.id, row.site_id, row.customer_id, &row.os_family).await?;
        let mut checks = Vec::new();
        if let Some(upgrade) = known {
            let target = upgrade.to_release();
            // Ubuntu's and leapp's tools take the new release from the
            // distribution; the agent's `sources` check refuses a server on
            // the mirrors.
            let mirror = if upgrade.method != Method::Apt {
                None
            } else if on_mirror.contains(&row.id) {
                let id = mirror_id(&target);
                let problem = match problems.get(&id) {
                    Some(problem) => problem.clone(),
                    None => {
                        let problem = package_sources::mirror_problem(&state.pool, &id).await?;
                        problems.insert(id, problem.clone());
                        problem
                    }
                };
                Some(problem)
            } else {
                None
            };
            let acceptance = row
                .command_kinds
                .as_ref()
                .and_then(|k| k.0.get(MAJOR_UPGRADE).cloned());
            checks = server_checks(acceptance.as_deref(), stored, &target, mirror);
            if upgrade.method != Method::Apt {
                checks[2] = Check::new(
                    "mirror",
                    OK,
                    "The upgrade takes the next release from the distribution, not from the mirrors.",
                );
            }
            if let Some(stored) = stored.filter(|s| s.target == target) {
                checks.extend(stored.checks.iter().cloned());
            }
        }
        let ready = known.is_some() && !checks.iter().any(|c| c.state == FAILED);
        out.push(ServerUpgrade {
            server_id: row.id,
            hostname: row.hostname,
            site_id: row.site_id,
            customer_id: row.customer_id,
            release,
            release_label: known.map(|u| u.from_label()),
            target: known.map(|u| u.to_release()),
            target_label: known.map(|u| u.to_label()),
            ready,
            checks,
            rollback,
            progress: stored.and_then(|s| s.progress.clone()),
            reported_at: row.reported_at,
            can_upgrade: administered.contains(row.customer_id),
        });
    }
    Ok(out)
}

pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
    Query(filter): Query<Filter>,
) -> Result<Json<MajorUpgradeList>, ApiError> {
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let servers = readiness(&state, &user, &filter, None).await?;
    Ok(Json(MajorUpgradeList { servers }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(target: &str) -> Stored {
        Stored {
            release: "debian 12".into(),
            target: target.into(),
            ..Stored::default()
        }
    }

    #[test]
    fn the_server_checks_say_what_is_missing() {
        let report = stored("debian 13");
        let ok = server_checks(Some("accepted"), Some(&report), "debian 13", None);
        assert!(ok.iter().all(|c| c.state == OK), "{ok:?}");
        let names: Vec<&str> = ok.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["allowed", "agent_version", "mirror"]);

        let off = server_checks(
            Some("off"),
            None,
            "debian 13",
            Some(Some("No mirror".into())),
        );
        let failed: Vec<&str> = off
            .iter()
            .filter(|c| c.state == FAILED)
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(failed, ["allowed", "agent_version", "mirror"]);
        assert!(off[0]
            .detail
            .contains("monitorflow-agent major-upgrades on"));
        assert_eq!(off[2].detail, "No mirror");
        let old = server_checks(None, Some(&stored("")), "debian 13", Some(None));
        assert!(old[0].detail.contains("too old"));
        assert_eq!(old[2].state, OK);
    }

    #[test]
    fn the_target_mirror_is_the_catalogs() {
        assert_eq!(mirror_id("debian 13"), "debian-13");
    }
}
