//! Package sources from a server's page or for a whole site (ADR-0074):
//! where a server's sources stand, and pointing it at the mirrors of its
//! own distribution or back at its own sources, without naming a mirror.
//! Built on what campaigns use: the same spec, relay choice and commands
//! (ADR-0035, ADR-0038). Reading needs a role on the site, changing needs
//! an administrator of its customer, the rule of [`super::sources`].
//! Windows servers have no mirror: their status carries where Windows
//! Update takes their updates from instead (ADR-0086).

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use monitorflow_common::command::{self, CONFIGURE_SOURCES, REVERT_SOURCES};
use monitorflow_common::proto::v1::configure_sources::Mode;
use monitorflow_common::proto::v1::{ConfigureSources, RevertSources};
use prost::Message;
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use super::access::{authorize, authorize_admin};
use super::auth::CurrentUser;
use super::commands::{queue, CommandDetail, Target};
use super::sensors::parse_id;
use super::sources::signing_key;
use super::{ApiError, ApiState};
use crate::commands::sources::{self, ArchiveKey, Selected, CURRENT};
use crate::commands::update_source::UpdateSource;
use crate::commands::State as CommandState;
use crate::mirror::catalog::{self, Distribution};
use crate::mirror::relays;

/// Time to live of these commands: a server offline for a night still
/// takes them.
const TTL: Duration = Duration::from_secs(24 * 3600);

const NOT_ENABLED: &str = "Mirrors are not enabled on this server.";
const NO_ADDRESS: &str = "The address of the mirrors isn't set. Set it on the Settings page.";

/// Where a server's packages come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Using {
    /// A mirror of this install.
    Mirror,
    /// The sources the server came with.
    Own,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UsedMirror {
    pub name: String,
    /// A snapshot name, or `current` for the newest.
    pub snapshot: String,
}

/// Body of `GET /api/servers/{id}/sources`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourcesStatus {
    pub using: Using,
    /// With [`Using::Mirror`].
    pub mirrors: Vec<UsedMirror>,
    /// A change on its way to the server.
    pub changing_to: Option<Using>,
    /// Why the last change failed, when none succeeded or started since.
    pub failed: Option<String>,
    /// A campaign has the sources for now.
    pub campaign: bool,
    /// The catalog's name for what the server runs.
    pub distribution: Option<&'static str>,
    /// Why the server cannot use a mirror, in a sentence for the page.
    pub problem: Option<String>,
    /// The fix is adding [`Self::distribution`] on the Mirrors page.
    pub add_mirror: bool,
    /// Windows servers: where Windows Update takes updates from, from the
    /// latest package report. `null` on Linux and until an agent that
    /// reports it sends a report (ADR-0086).
    pub windows_update_source: Option<UpdateSource>,
}

impl SourcesStatus {
    /// Why the server cannot be pointed at a mirror now.
    fn refusal(&self) -> Option<String> {
        if self.using == Using::Mirror {
            Some("This server already uses a mirror.".into())
        } else {
            self.busy().or_else(|| self.problem.clone())
        }
    }

    /// Why the server's sources cannot change now.
    fn busy(&self) -> Option<String> {
        if self.changing_to.is_some() {
            Some("A change of package sources is on its way to this server.".into())
        } else if self.campaign {
            Some("An update campaign has this server's package sources for now.".into())
        } else {
            None
        }
    }
}

/// One server of `GET /api/sites/{id}/sources`.
#[derive(Debug, Serialize)]
pub struct SiteServer {
    pub id: Uuid,
    pub hostname: String,
    /// As in the server listing, so Windows rows read differently.
    pub os_family: String,
    #[serde(flatten)]
    pub status: SourcesStatus,
}

/// Body of `GET /api/sites/{id}/sources`, by host name.
#[derive(Debug, Serialize)]
pub struct SiteSources {
    pub servers: Vec<SiteServer>,
}

#[derive(Debug, Serialize)]
pub struct Skipped {
    pub id: Uuid,
    pub hostname: String,
    pub reason: String,
}

/// Answer of the site's changes: each server is queued or skipped alone.
#[derive(Debug, Serialize)]
pub struct SiteChange {
    pub queued: Vec<Uuid>,
    pub skipped: Vec<Skipped>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct Host {
    id: Uuid,
    site_id: Uuid,
    peer_id: String,
    hostname: String,
    os_family: String,
    os_version: Option<String>,
    package_updates: bool,
    windows_update_source: Option<sqlx::types::Json<UpdateSource>>,
}

impl Host {
    fn target(&self) -> Target {
        Target {
            id: self.id,
            peer_id: self.peer_id.clone(),
            os_family: self.os_family.clone(),
            package_updates: self.package_updates,
            revoked: false,
        }
    }
}

const HOST_COLUMNS: &str =
    "id, site_id, peer_id, hostname, os_family, os_version, package_updates, windows_update_source";

/// The last source commands of a server, one for each outcome.
#[derive(Debug, Default)]
struct History {
    open: Option<Row>,
    done: Option<Row>,
    failed: Option<Row>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct Row {
    server_id: Uuid,
    outcome: String,
    kind: String,
    payload: Vec<u8>,
    state: CommandState,
    error: Option<String>,
    at: DateTime<Utc>,
}

async fn histories(pool: &PgPool, servers: &[Uuid]) -> Result<HashMap<Uuid, History>, sqlx::Error> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT DISTINCT ON (server_id, outcome) *
         FROM (SELECT server_id, kind, payload, state, error,
                   COALESCE(finished_at, issued_at) AS at,
                   CASE WHEN state IN ('queued', 'sent', 'running') THEN 'open'
                        WHEN state = 'succeeded' THEN 'done'
                        ELSE 'failed' END AS outcome
               FROM commands WHERE server_id = ANY($1) AND kind IN ($2, $3)) c
         ORDER BY server_id, outcome, at DESC",
    )
    .bind(servers)
    .bind(CONFIGURE_SOURCES)
    .bind(REVERT_SOURCES)
    .fetch_all(pool)
    .await?;
    let mut histories: HashMap<Uuid, History> = HashMap::new();
    for row in rows {
        let history = histories.entry(row.server_id).or_default();
        match row.outcome.as_str() {
            "open" => history.open = Some(row),
            "done" => history.done = Some(row),
            _ => history.failed = Some(row),
        }
    }
    Ok(histories)
}

/// Servers whose sources an unfinished campaign has changed and not yet
/// restored.
async fn in_campaigns(pool: &PgPool, servers: &[Uuid]) -> Result<HashSet<Uuid>, sqlx::Error> {
    let rows: Vec<Uuid> = sqlx::query_scalar(
        "SELECT t.server_id FROM campaign_targets t
         JOIN campaigns c ON c.id = t.campaign_id
         WHERE t.server_id = ANY($1) AND t.sources_command_id IS NOT NULL
           AND t.restore_command_id IS NULL
           AND c.state NOT IN ('stopped', 'completed', 'failed')",
    )
    .bind(servers)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().collect())
}

fn repositories(spec: &ConfigureSources) -> Vec<UsedMirror> {
    let apt = spec.apt.iter().map(|r| (&r.mirror, &r.snapshot));
    let dnf = spec.dnf.iter().map(|r| (&r.mirror, &r.snapshot));
    apt.chain(dnf)
        .map(|(name, snapshot)| UsedMirror {
            name: name.clone(),
            snapshot: snapshot.clone(),
        })
        .collect()
}

/// `(using, mirrors, changing_to, failed)` from a server's commands.
fn stand(history: &History) -> (Using, Vec<UsedMirror>, Option<Using>, Option<String>) {
    let (using, mirrors) = match &history.done {
        Some(row) if row.kind == command::CONFIGURE_SOURCES => (
            Using::Mirror,
            ConfigureSources::decode(&*row.payload)
                .map(|spec| repositories(&spec))
                .unwrap_or_default(),
        ),
        _ => (Using::Own, Vec::new()),
    };
    let changing_to = history.open.as_ref().map(|row| {
        if row.kind == command::CONFIGURE_SOURCES {
            Using::Mirror
        } else {
            Using::Own
        }
    });
    let failed = history
        .failed
        .as_ref()
        .filter(|_| changing_to.is_none())
        .filter(|row| history.done.as_ref().is_none_or(|done| done.at < row.at))
        .map(|row| {
            row.error
                .clone()
                .unwrap_or_else(|| format!("The last change {}.", row.state.as_str()))
        });
    (using, mirrors, changing_to, failed)
}

/// What the mirrors of this install hold for one distribution.
enum Ready {
    Yes(Vec<Selected>),
    No { problem: String, add_mirror: bool },
}

async fn ready(pool: &PgPool, distribution: &Distribution) -> Result<Ready, sqlx::Error> {
    let mut selected = Vec::new();
    let mut waiting = false;
    for name in distribution.mirror_names() {
        if crate::mirror::store::by_name(pool, name).await?.is_none() {
            let problem = format!(
                "No mirror carries {} yet, so this server keeps its own package sources.",
                distribution.name
            );
            return Ok(Ready::No {
                problem,
                add_mirror: true,
            });
        }
        match sources::select(pool, name, CURRENT).await? {
            Ok(found) => selected.push(found),
            Err(_) => waiting = true,
        }
    }
    Ok(if waiting {
        Ready::No {
            problem: format!(
                "{} is still being copied. This server can use it after the first sync; until then it keeps its own package sources.",
                distribution.name
            ),
            add_mirror: false,
        }
    } else {
        Ready::Yes(selected)
    })
}

/// Sorts out servers and caches what each distribution holds.
struct Fit<'a> {
    state: &'a ApiState,
    public_url: Option<String>,
    ready: HashMap<&'static str, Ready>,
}

impl<'a> Fit<'a> {
    fn new(state: &'a ApiState) -> Self {
        Self {
            state,
            public_url: state.settings.live.mirror_public_url(),
            ready: HashMap::new(),
        }
    }

    /// `(distribution, problem, add_mirror)` of `host`.
    async fn of(
        &mut self,
        host: &Host,
    ) -> Result<(Option<&'static str>, Option<String>, bool), sqlx::Error> {
        let keeps = "It keeps its own package sources.";
        if !host.package_updates {
            let problem = format!("Package updates aren't available on this system. {keeps}");
            return Ok((None, Some(problem), false));
        }
        if host.os_family == "windows" {
            let problem = format!("Mirrors carry Linux packages only. {keeps}");
            return Ok((None, Some(problem), false));
        }
        let Some(version) = host.os_version.as_deref().filter(|v| !v.is_empty()) else {
            let problem =
                format!("This server hasn't said which distribution it runs, so no mirror matches it. {keeps}");
            return Ok((None, Some(problem), false));
        };
        let Some(distribution) = catalog::for_os_version(version) else {
            let problem = format!("MonitorFlow has no mirror for {version}. {keeps}");
            return Ok((None, Some(problem), false));
        };
        if self.state.mirrors.is_none() {
            return Ok((Some(distribution.name), Some(NOT_ENABLED.into()), false));
        }
        if !self.ready.contains_key(distribution.id) {
            let found = ready(&self.state.pool, distribution).await?;
            self.ready.insert(distribution.id, found);
        }
        Ok(match &self.ready[distribution.id] {
            Ready::Yes(_) => (Some(distribution.name), None, false),
            Ready::No {
                problem,
                add_mirror,
            } => (Some(distribution.name), Some(problem.clone()), *add_mirror),
        })
    }

    fn selected(&self, host: &Host) -> Option<&[Selected]> {
        let distribution = catalog::for_os_version(host.os_version.as_deref()?)?;
        match self.ready.get(distribution.id)? {
            Ready::Yes(selected) => Some(selected),
            Ready::No { .. } => None,
        }
    }

    /// The status of each of `hosts`.
    async fn statuses(&mut self, hosts: &[Host]) -> Result<Vec<SourcesStatus>, sqlx::Error> {
        let ids: Vec<Uuid> = hosts.iter().map(|h| h.id).collect();
        let histories = histories(&self.state.pool, &ids).await?;
        let campaigns = in_campaigns(&self.state.pool, &ids).await?;
        let mut statuses = Vec::with_capacity(hosts.len());
        for host in hosts {
            let (using, mirrors, changing_to, failed) = histories
                .get(&host.id)
                .map(stand)
                .unwrap_or_else(|| (Using::Own, Vec::new(), None, None));
            let (distribution, problem, add_mirror) = self.of(host).await?;
            statuses.push(SourcesStatus {
                using,
                mirrors,
                changing_to,
                failed,
                campaign: campaigns.contains(&host.id),
                distribution,
                problem,
                add_mirror,
                windows_update_source: host
                    .windows_update_source
                    .as_ref()
                    .filter(|_| host.os_family == "windows")
                    .map(|source| source.0.clone()),
            });
        }
        Ok(statuses)
    }

    /// The `configure_sources` payload for `host`, which [`Self::of`] found
    /// ready. Replaces its sources: a mirror next to the Internet would not
    /// be used, and only this works on a site without Internet.
    async fn payload(&self, host: &Host, key: &ArchiveKey) -> Result<Vec<u8>, ApiError> {
        let selected = self
            .selected(host)
            .ok_or_else(|| ApiError::Conflict(NOT_ENABLED.into()))?;
        let relay = relays::for_server(&mut *self.state.pool.acquire().await?, host.id).await?;
        let base = sources::base_url(self.public_url.as_deref(), relay.as_ref(), selected)
            .ok_or_else(|| ApiError::Conflict(NO_ADDRESS.into()))?;
        let spec = sources::build(&base, Mode::Replace, &host.os_family, selected, key)
            .map_err(ApiError::Invalid)?;
        Ok(spec.encode_to_vec())
    }
}

async fn find_host(state: &ApiState, id: Uuid) -> Result<Option<Host>, sqlx::Error> {
    // SQL-safe: interpolates only the constant HOST_COLUMNS.
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {HOST_COLUMNS} FROM servers WHERE id = $1 AND revoked_at IS NULL"
    )))
    .bind(id)
    .fetch_optional(&state.pool)
    .await
}

/// `GET /api/servers/{id}/sources`.
pub async fn server(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<SourcesStatus>, ApiError> {
    let host = find_host(&state, parse_id(&id)?).await?;
    authorize(&user, host.as_ref().map(|h| h.site_id), false)?;
    let host = host.ok_or(ApiError::NotFound)?;
    let mut statuses = Fit::new(&state).statuses(&[host]).await?;
    Ok(Json(statuses.remove(0)))
}

/// `POST /api/servers/{id}/sources/use-mirror`: 201 with the command that
/// points the server at the mirrors of its distribution, 409 saying why
/// not.
pub async fn use_mirror(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<CommandDetail>), ApiError> {
    let target = Target::find(&state, &user, &id).await?;
    let host = find_host(&state, target.id)
        .await?
        .ok_or_else(|| ApiError::Conflict("the server is revoked".into()))?;
    let mut fit = Fit::new(&state);
    let hosts = [host];
    let status = fit.statuses(&hosts).await?.remove(0);
    if let Some(refusal) = status.refusal() {
        return Err(ApiError::Conflict(refusal));
    }
    let mirrors = state
        .mirrors
        .as_ref()
        .ok_or_else(|| ApiError::Conflict(NOT_ENABLED.into()))?;
    let key = signing_key(mirrors).await?;
    let payload = fit.payload(&hosts[0], &key).await?;
    queue(
        &state,
        &user,
        &target,
        command::CONFIGURE_SOURCES,
        payload,
        TTL,
    )
    .await
}

async fn site_hosts(state: &ApiState, site: Uuid) -> Result<Vec<Host>, sqlx::Error> {
    // SQL-safe: interpolates only the constant HOST_COLUMNS.
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {HOST_COLUMNS} FROM servers
         WHERE site_id = $1 AND revoked_at IS NULL ORDER BY hostname, peer_id"
    )))
    .bind(site)
    .fetch_all(&state.pool)
    .await
}

async fn find_site(state: &ApiState, id: &str) -> Result<Option<Uuid>, ApiError> {
    Ok(sqlx::query_scalar("SELECT id FROM sites WHERE id = $1")
        .bind(parse_id(id)?)
        .fetch_optional(&state.pool)
        .await?)
}

/// `GET /api/sites/{id}/sources`.
pub async fn site(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<SiteSources>, ApiError> {
    let site = authorize(&user, find_site(&state, &id).await?, false)?;
    let hosts = site_hosts(&state, site).await?;
    let statuses = Fit::new(&state).statuses(&hosts).await?;
    let servers = hosts
        .into_iter()
        .zip(statuses)
        .map(|(host, status)| SiteServer {
            id: host.id,
            hostname: host.hostname,
            os_family: host.os_family,
            status,
        })
        .collect();
    Ok(Json(SiteSources { servers }))
}

/// Records the outcome of one server of a site.
fn sort_out(
    change: &mut SiteChange,
    host: &Host,
    result: Result<(), ApiError>,
) -> Result<(), ApiError> {
    match result {
        Ok(()) => change.queued.push(host.id),
        Err(ApiError::Conflict(reason) | ApiError::Invalid(reason)) => {
            change.skipped.push(Skipped {
                id: host.id,
                hostname: host.hostname.clone(),
                reason,
            })
        }
        Err(other) => return Err(other),
    }
    Ok(())
}

/// `POST /api/sites/{id}/sources/use-mirror`: points every server of the
/// site that can use a mirror at the mirrors of its distribution, and
/// says why the others were skipped.
pub async fn site_use_mirror(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<SiteChange>, ApiError> {
    let site = authorize_admin(&user, find_site(&state, &id).await?)?;
    let hosts = site_hosts(&state, site).await?;
    let mut fit = Fit::new(&state);
    let statuses = fit.statuses(&hosts).await?;
    let mut change = SiteChange {
        queued: Vec::new(),
        skipped: Vec::new(),
    };
    let mut key = None;
    for (host, status) in hosts.iter().zip(&statuses) {
        // Already there is what was asked for.
        if status.using == Using::Mirror && status.changing_to.is_none() {
            continue;
        }
        let result = async {
            if let Some(refusal) = status.refusal() {
                return Err(ApiError::Conflict(refusal));
            }
            if key.is_none() {
                let mirrors = state
                    .mirrors
                    .as_ref()
                    .ok_or_else(|| ApiError::Conflict(NOT_ENABLED.into()))?;
                key = Some(signing_key(mirrors).await?);
            }
            let key = key.as_ref().expect("set above");
            let payload = fit.payload(host, key).await?;
            queue(
                &state,
                &user,
                &host.target(),
                command::CONFIGURE_SOURCES,
                payload,
                TTL,
            )
            .await
            .map(drop)
        }
        .await;
        sort_out(&mut change, host, result)?;
    }
    Ok(Json(change))
}

/// `DELETE /api/sites/{id}/sources`: puts back the own sources of every
/// server of the site that uses a mirror.
pub async fn site_back(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<SiteChange>, ApiError> {
    let site = authorize_admin(&user, find_site(&state, &id).await?)?;
    let hosts = site_hosts(&state, site).await?;
    let statuses = Fit::new(&state).statuses(&hosts).await?;
    let mut change = SiteChange {
        queued: Vec::new(),
        skipped: Vec::new(),
    };
    for (host, status) in hosts.iter().zip(&statuses) {
        if status.using != Using::Mirror {
            continue;
        }
        let result = match status.busy() {
            Some(busy) => Err(ApiError::Conflict(busy)),
            None => queue(
                &state,
                &user,
                &host.target(),
                command::REVERT_SOURCES,
                RevertSources {}.encode_to_vec(),
                TTL,
            )
            .await
            .map(drop),
        };
        sort_out(&mut change, host, result)?;
    }
    Ok(Json(change))
}

#[cfg(test)]
mod tests {
    use monitorflow_common::proto::v1::configure_sources::AptRepository;

    use super::*;

    fn row(kind: &str, state: CommandState, at: i64) -> Row {
        let spec = ConfigureSources {
            apt: vec![AptRepository {
                mirror: "debian-12".into(),
                snapshot: CURRENT.into(),
                ..AptRepository::default()
            }],
            ..ConfigureSources::default()
        };
        Row {
            server_id: Uuid::nil(),
            outcome: String::new(),
            kind: kind.into(),
            payload: spec.encode_to_vec(),
            state,
            error: None,
            at: DateTime::from_timestamp(at, 0).unwrap(),
        }
    }

    #[test]
    fn a_server_never_changed_uses_its_own_sources() {
        let (using, mirrors, changing, failed) = stand(&History::default());
        assert_eq!(using, Using::Own);
        assert!(mirrors.is_empty() && changing.is_none() && failed.is_none());
    }

    #[test]
    fn the_last_success_says_where_a_server_stands() {
        let mut history = History {
            done: Some(row(CONFIGURE_SOURCES, CommandState::Succeeded, 10)),
            ..History::default()
        };
        let (using, mirrors, _, _) = stand(&history);
        assert_eq!(using, Using::Mirror);
        assert_eq!(mirrors[0].name, "debian-12");

        history.open = Some(row(REVERT_SOURCES, CommandState::Queued, 20));
        assert_eq!(stand(&history).2, Some(Using::Own));

        history.done = Some(row(REVERT_SOURCES, CommandState::Succeeded, 30));
        history.open = None;
        assert_eq!(stand(&history).0, Using::Own);
    }

    #[test]
    fn only_a_failure_after_the_last_success_is_told() {
        let mut failed = row(CONFIGURE_SOURCES, CommandState::Failed, 10);
        failed.error = Some("apt-get update failed".into());
        let mut history = History {
            failed: Some(failed),
            done: None,
            open: None,
        };
        assert_eq!(stand(&history).3.as_deref(), Some("apt-get update failed"));
        history.done = Some(row(REVERT_SOURCES, CommandState::Succeeded, 20));
        assert_eq!(stand(&history).3, None);
        history.done = Some(row(REVERT_SOURCES, CommandState::Succeeded, 5));
        history.open = Some(row(CONFIGURE_SOURCES, CommandState::Sent, 30));
        assert_eq!(stand(&history).3, None);

        history.open = None;
        history.failed = Some(row(CONFIGURE_SOURCES, CommandState::Expired, 10));
        assert_eq!(
            stand(&history).3.as_deref(),
            Some("The last change expired.")
        );
    }

    #[test]
    fn a_status_says_why_not() {
        let mut status = SourcesStatus {
            using: Using::Own,
            mirrors: Vec::new(),
            changing_to: None,
            failed: None,
            campaign: false,
            distribution: Some("Debian 12 (bookworm)"),
            problem: None,
            add_mirror: false,
            windows_update_source: None,
        };
        assert_eq!(status.refusal(), None);
        status.problem = Some("no".into());
        assert_eq!(status.refusal().as_deref(), Some("no"));
        status.campaign = true;
        assert!(status.refusal().unwrap().contains("campaign"));
        status.changing_to = Some(Using::Mirror);
        assert!(status.refusal().unwrap().contains("on its way"));
        status.using = Using::Mirror;
        assert!(status.refusal().unwrap().contains("already"));
    }
}
