//! `GET /api/major-upgrades` (ADR-0182): for the servers a user sees, the
//! next release of their distribution, every check before upgrading to it
//! (the agent's, and the central server's own: the host's consent, the
//! agent's version and the mirrors), the rollback they would get, and the
//! progress of their last upgrade. Upgrades themselves are campaigns.
//!
//! `POST /api/servers/{id}/major-upgrade/preupgrade` runs `leapp
//! preupgrade` alone on a server of the RHEL family, before any campaign:
//! its report comes back in the server's progress, and nothing changes.

use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use monitorflow_common::command::{LEAPP_PREUPGRADE, MAJOR_UPGRADE};
use monitorflow_common::major_upgrade::{self, Method, FAILED, OK, WARNING};
use monitorflow_common::proto::v1::MajorUpgrade;
use prost::Message;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::access::authorize;
use super::auth::CurrentUser;
use super::commands::{queue, CommandDetail, Target};
use super::proxmox::SnapshotSupport;
use super::sensors::parse_id;
use super::{package_sources, rollback, ApiError, ApiState};
use crate::commands::major_upgrade::{Check, Progress, Stored};

/// Time to live of a `leapp_preupgrade`: it is asked from the page, by
/// someone who waits for the report.
const PREUPGRADE_TTL: Duration = Duration::from_secs(3600);

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
    /// The user may run `leapp preupgrade` alone on it now (`POST
    /// /servers/{id}/major-upgrade/preupgrade`): the RHEL family, an
    /// administrator of its customer, and an agent that accepts it.
    pub can_preupgrade: bool,
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

/// The `mirror` check of Ubuntu and the RHEL family, whose tools take
/// the next release from the distribution: a server on the mirrors goes
/// back to its own sources for the upgrade, and to the mirrors of the
/// next release after it when they carry it (ADR-0182). `mirror` as for
/// [`server_checks`].
pub fn own_sources_first(
    upgrade: &major_upgrade::Upgrade,
    mirror: Option<Option<String>>,
) -> Check {
    let tool = match upgrade.method {
        Method::Leapp => "leapp",
        _ => "do-release-upgrade",
    };
    match mirror {
        None => Check::new(
            "mirror",
            OK,
            "The upgrade takes the next release from the distribution, not from the mirrors.",
        ),
        Some(None) => Check::new(
            "mirror",
            OK,
            format!(
                "The server uses the mirrors, which {tool} does not use: the campaign puts its own sources back before the upgrade, and the mirrors of {} after it.",
                upgrade.to_label()
            ),
        ),
        Some(Some(problem)) => Check::new(
            "mirror",
            WARNING,
            format!(
                "The server uses the mirrors, which {tool} does not use: the campaign puts its own sources back before the upgrade. {problem} After the upgrade it keeps its own sources until the mirrors carry {}.",
                upgrade.to_label()
            ),
        ),
    }
}

/// What the server's agent does with `kind`, as `Hello` said.
fn kind_state<'a>(row: &'a Row, kind: &str) -> Option<&'a str> {
    row.command_kinds
        .as_ref()
        .and_then(|k| k.0.get(kind))
        .map(String::as_str)
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
            let mirror = if on_mirror.contains(&row.id) {
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
            if upgrade.method != Method::Apt {
                checks = server_checks(acceptance.as_deref(), stored, &target, None);
                checks[2] = own_sources_first(upgrade, mirror);
            } else {
                checks = server_checks(acceptance.as_deref(), stored, &target, mirror);
            }
            if let Some(stored) = stored.filter(|s| s.target == target) {
                checks.extend(stored.checks.iter().cloned());
            }
        }
        let ready = known.is_some() && !checks.iter().any(|c| c.state == FAILED);
        let can_preupgrade = administered.contains(row.customer_id)
            && known.is_some_and(|u| u.method == Method::Leapp)
            && kind_state(&row, LEAPP_PREUPGRADE) == Some("accepted");
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
            can_preupgrade,
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

/// Why `leapp preupgrade` cannot run on a server of `os_version` whose
/// agent reports `acceptance` for it; `None` when it can.
pub fn preupgrade_refusal(os_version: &str, acceptance: Option<&str>) -> Option<String> {
    match major_upgrade::next(os_version) {
        Some(upgrade) if upgrade.method == Method::Leapp => {}
        Some(upgrade) => {
            return Some(format!(
                "leapp preupgrade is for the RHEL family; {} upgrades with its own tool",
                upgrade.from_label()
            ))
        }
        None => {
            return Some(format!(
                "MonitorFlow knows no next release for {}",
                if os_version.is_empty() {
                    "this server"
                } else {
                    os_version
                }
            ))
        }
    }
    match acceptance {
        Some("accepted") => None,
        Some("off") => Some(
            "The server does not allow major upgrades from the web interface: run `monitorflow-agent major-upgrades on` there as root, or install it again with MF_MAJOR_UPGRADES_FROM_WEB=1.".into(),
        ),
        Some(_) => Some("This server's agent cannot run leapp preupgrade.".into()),
        None => Some(
            "The agent of this server is too old for leapp preupgrade from here; it updates by itself.".into(),
        ),
    }
}

/// The server a `leapp_preupgrade` goes to.
#[derive(Debug, sqlx::FromRow)]
struct PreupgradeRow {
    site_id: Uuid,
    customer_id: Uuid,
    os_version: Option<String>,
    command_kinds: Option<sqlx::types::Json<BTreeMap<String, String>>>,
    peer_id: String,
    revoked: bool,
}

/// `POST /api/servers/{id}/major-upgrade/preupgrade`: 201 with the
/// `leapp_preupgrade` command for the next release of the server, which
/// runs `leapp preupgrade` alone; 409 saying why not. Its report comes
/// back in the server's `progress`, with `preupgrade_only`.
pub async fn preupgrade(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<CommandDetail>), ApiError> {
    let id = parse_id(&id)?;
    let row: Option<PreupgradeRow> = sqlx::query_as(
        "SELECT s.site_id, si.customer_id, s.os_version, s.command_kinds, s.peer_id,
                 s.revoked_at IS NOT NULL AS revoked
             FROM servers s JOIN sites si ON si.id = s.site_id
             WHERE s.id = $1 AND s.kind = 'agent'",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?;
    authorize(&user, row.as_ref().map(|r| r.site_id), false)?;
    let PreupgradeRow {
        customer_id: customer,
        os_version,
        command_kinds: kinds,
        peer_id,
        revoked,
        ..
    } = row.ok_or(ApiError::NotFound)?;
    // Like a campaign: administrators of the customer only (ADR-0182).
    if !user.administered_customers().contains(customer) {
        return Err(ApiError::Forbidden);
    }
    let os_version = os_version.unwrap_or_default();
    let acceptance = kinds
        .as_ref()
        .and_then(|k| k.0.get(LEAPP_PREUPGRADE).cloned());
    if let Some(refusal) = preupgrade_refusal(&os_version, acceptance.as_deref()) {
        return Err(ApiError::Conflict(refusal));
    }
    let Some(upgrade) = major_upgrade::next(&os_version) else {
        return Err(ApiError::Conflict("no next release".into()));
    };
    let payload = MajorUpgrade {
        from: upgrade.from_release(),
        to: upgrade.to_release(),
    }
    .encode_to_vec();
    let target = Target {
        id,
        peer_id,
        os_family: "rhel".into(),
        package_updates: true,
        revoked,
    };
    queue(
        &state,
        &user,
        &target,
        LEAPP_PREUPGRADE,
        payload,
        PREUPGRADE_TTL,
    )
    .await
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
    fn ubuntu_and_rhel_on_the_mirrors_go_back_to_their_own_sources_first() {
        let ubuntu = major_upgrade::next("ubuntu 22.04").unwrap();
        assert_eq!(own_sources_first(ubuntu, None).state, OK);
        let ready = own_sources_first(ubuntu, Some(None));
        assert_eq!(ready.state, OK);
        assert!(
            ready
                .detail
                .contains("the mirrors of Ubuntu 24.04 (noble) after it"),
            "{}",
            ready.detail
        );
        let rocky = major_upgrade::next("rocky 8.10").unwrap();
        let missing = own_sources_first(
            rocky,
            Some(Some("No mirror carries Rocky Linux 9 yet.".into())),
        );
        assert_eq!(
            missing.state, WARNING,
            "never blocks: the server keeps its own sources"
        );
        assert!(
            missing.detail.contains("which leapp does not use"),
            "{}",
            missing.detail
        );
        assert!(missing
            .detail
            .contains("keeps its own sources until the mirrors carry Rocky Linux 9"));
    }

    #[test]
    fn leapp_preupgrade_needs_the_rhel_family_and_an_agent_that_accepts_it() {
        assert_eq!(preupgrade_refusal("rocky 8.10", Some("accepted")), None);
        assert_eq!(preupgrade_refusal("rhel 9.4", Some("accepted")), None);
        assert!(preupgrade_refusal("debian 12", Some("accepted"))
            .unwrap()
            .starts_with("leapp preupgrade is for the RHEL family"));
        assert!(preupgrade_refusal("rocky 9.4", Some("accepted"))
            .unwrap()
            .contains("no next release for rocky 9.4"));
        assert!(preupgrade_refusal("rocky 8.10", Some("off"))
            .unwrap()
            .contains("major-upgrades on"));
        assert!(preupgrade_refusal("rocky 8.10", None)
            .unwrap()
            .contains("too old"));
    }

    #[test]
    fn the_target_mirror_is_the_catalogs() {
        assert_eq!(mirror_id("debian 13"), "debian-13");
    }
}
