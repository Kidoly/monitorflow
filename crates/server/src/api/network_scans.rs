//! Network scans (ADR-0154): a site's addresses scanned from the central
//! server or a relay, what they found and what they propose. Viewers of
//! the site read them; operators of the site create, change, run and
//! delete them, add what they propose and dismiss addresses. Every
//! change is audited; runs the schedule starts are not.

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgConnection;
use uuid::Uuid;

use super::access::authorize;
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::devices;
use super::sensors::{body, parse_id, run_by, Owner, RunBy};
use super::{ApiError, ApiState};
use crate::netscan::proposals::{self, Known, Proposal};
use crate::netscan::{self, results::Host, targets};
use crate::snmp;

/// Longest name of a scan.
pub const MAX_NAME: usize = 128;
/// Most scans of one site.
pub const MAX_PER_SITE: i64 = 50;

/// Body of `POST /api/network-scans`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[schema(as = NetworkScanRequest)]
#[serde(deny_unknown_fields)]
pub struct ScanRequest {
    /// The site whose addresses it scans.
    pub site_id: Uuid,
    /// What people call it, like `Office network`.
    pub name: String,
    /// IP addresses, blocks like `192.168.1.0/24` and ranges like
    /// `192.168.1.10-50` or `192.168.1.10-192.168.1.50`: at most 64
    /// entries and 1,024 addresses in all. Never names. Loopback,
    /// multicast, link-local and unspecified addresses are refused;
    /// network and broadcast addresses of IPv4 blocks are left out.
    pub targets: Vec<String>,
    /// TCP ports to try, at most 32; absent: 22, 23, 80, 135, 139, 443,
    /// 445, 3389, 5985, 8080 and 8443.
    #[serde(default)]
    pub ports: Option<Vec<u16>>,
    /// Where it runs from: `central`, or the ID of a relay of the site.
    /// Absent: the relay of the site when there is one, else the central
    /// server.
    #[serde(default)]
    pub probe: Option<String>,
    /// `hour`, `day` or `week` to scan again on its own; absent or
    /// `null`: only when asked.
    #[serde(default)]
    pub every: Option<String>,
    /// Whether a run that finds something new or gone sends a
    /// notification to the site's channels. Off by default.
    #[serde(default)]
    pub notify: bool,
    /// SNMP credentials to try besides those the site's devices have.
    /// Stored sealed, never returned.
    #[serde(default)]
    pub snmp: Option<snmp::CredentialsRequest>,
    /// Whether to scan now. `true` by default.
    #[serde(default = "yes")]
    pub run: bool,
}

fn yes() -> bool {
    true
}

/// Body of `PUT /api/network-scans/{id}`: the whole scan but its site.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[schema(as = NetworkScanUpdate)]
#[serde(deny_unknown_fields)]
pub struct ScanUpdate {
    pub name: String,
    pub targets: Vec<String>,
    #[serde(default)]
    pub ports: Option<Vec<u16>>,
    #[serde(default)]
    pub probe: Option<String>,
    /// `null` or absent: only when asked.
    #[serde(default)]
    pub every: Option<String>,
    #[serde(default)]
    pub notify: bool,
    /// New credentials; absent keeps the stored ones, and a secret left
    /// out keeps the stored one while the version stays.
    #[serde(default)]
    pub snmp: Option<snmp::CredentialsRequest>,
    /// Removes the scan's own credentials.
    #[serde(default)]
    pub remove_snmp: bool,
}

/// A scan's own SNMP credentials, without secrets.
#[derive(Debug, Serialize, PartialEq, Eq, utoipa::ToSchema)]
#[schema(as = NetworkScanSnmp)]
pub struct SnmpView {
    pub version: String,
    pub port: u16,
    pub user: Option<String>,
    pub auth_protocol: Option<String>,
    pub priv_protocol: Option<String>,
}

/// One run of a scan.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = NetworkScanRun)]
pub struct RunView {
    pub id: Uuid,
    /// `queued`, `running`, `done` or `failed`.
    #[schema(example = "done")]
    pub state: String,
    /// `person` or `schedule`.
    pub trigger: String,
    pub queued_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    /// Addresses to try, and tried so far.
    pub addresses: i32,
    pub scanned: i32,
    /// Addresses that answered something.
    pub answered: i32,
    /// Whether a finished run came before it, so `new` and `gone` mean
    /// something.
    pub compared: bool,
    /// Addresses that answered and not in the previous finished run.
    pub new: i32,
    /// Addresses that answered in the previous finished run and not in
    /// this one.
    pub gone: Vec<String>,
    /// Answers of the probe that were not believed: an address or port it
    /// was not asked about, a name that does not fit.
    pub dropped: i32,
    /// Why the probe could not ping, when it could not; ports and SNMP
    /// were still tried.
    pub ping_error: Option<String>,
    /// Why it failed: `interrupted` (the central server instance running
    /// it stopped), `relay_disconnected`, `relay_outdated` (the relay
    /// does not run scans yet: update it), `relay_removed`, or the
    /// probe's own words.
    pub error: Option<String>,
}

/// A scan.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = NetworkScan)]
pub struct ScanView {
    pub id: Uuid,
    pub site_id: Uuid,
    pub site_name: String,
    pub name: String,
    pub targets: Vec<String>,
    /// How many addresses `targets` stand for.
    pub addresses: usize,
    pub ports: Vec<u16>,
    /// `central` or `relay`.
    pub probe: String,
    pub probe_relay_id: Option<Uuid>,
    /// `central` or the relay's ID: where it runs from, in words.
    pub probe_key: String,
    /// `hour`, `day`, `week`, or `null` for only when asked.
    pub every: Option<String>,
    pub notify: bool,
    /// Its own SNMP credentials, without secrets; `null` when it tries
    /// only those of the site's devices.
    pub snmp: Option<SnmpView>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The latest run, whatever its state.
    pub last_run: Option<RunView>,
}

/// Body of `GET /api/network-scans`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = NetworkScanList)]
pub struct ScanList {
    pub scans: Vec<ScanView>,
}

/// Query of `GET /api/network-scans`.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListQuery {
    /// Only the scans of this site.
    pub site_id: Option<Uuid>,
}

/// One address that answered, and what the scan proposes for it.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = NetworkScanHost)]
pub struct HostView {
    pub address: String,
    /// Whether it answered ping, and its round trip.
    pub ping: bool,
    pub rtt_seconds: Option<f64>,
    /// Open TCP ports, ascending.
    pub ports: Vec<u16>,
    /// Its name from reverse DNS.
    pub name: Option<String>,
    /// Its MAC address, when it is on the probe's own link.
    pub mac: Option<String>,
    /// Whether it answered SNMP, and what it said: shown as text.
    pub snmp: bool,
    pub snmp_description: Option<String>,
    pub snmp_name: Option<String>,
    pub snmp_object_id: Option<String>,
    /// Groups that answer among `interfaces`, `host`, `ups`,
    /// `temperature`, `printer`.
    pub snmp_groups: Vec<String>,
    /// Whether it did not answer in the previous finished run.
    pub new: bool,
    pub proposal: Proposal,
}

/// Body of `GET /api/network-scans/{id}/results`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = NetworkScanResults)]
pub struct Results {
    /// The latest finished run, whose hosts these are; `null` before one.
    pub run: Option<RunView>,
    /// A run queued or running now, if any.
    pub current: Option<RunView>,
    pub hosts: Vec<HostView>,
}

/// Body of `GET /api/network-scans/{id}/runs`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = NetworkScanRuns)]
pub struct RunList {
    /// The latest first, at most 10.
    pub runs: Vec<RunView>,
}

/// One address to add as a device.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[schema(as = NetworkScanAddDevice)]
#[serde(deny_unknown_fields)]
pub struct AddDevice {
    pub address: String,
    /// Its name; absent: the proposal's.
    #[serde(default)]
    pub name: Option<String>,
    /// For an address that answered SNMP, the template instead of the
    /// proposed one.
    #[serde(default)]
    pub template: Option<String>,
}

/// Body of `POST /api/network-scans/{id}/add`.
#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
#[schema(as = NetworkScanAddRequest)]
#[serde(deny_unknown_fields)]
pub struct AddRequest {
    /// Every address proposed as a device or an SNMP device, with what
    /// the proposal says.
    #[serde(default)]
    pub all: bool,
    /// These addresses, any that answered and is not monitored yet; one
    /// proposed for the agent is added as a device.
    #[serde(default)]
    pub devices: Vec<AddDevice>,
}

/// A device added from a scan.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = NetworkScanAdded)]
pub struct Added {
    pub address: String,
    pub server_id: Uuid,
    pub name: String,
    pub template: Option<String>,
}

/// An address that was not added, and why.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = NetworkScanSkipped)]
pub struct Skipped {
    pub address: String,
    /// `not_found` (it did not answer in the latest finished run),
    /// `monitored` (the site watches it already), `no_credentials` (the
    /// SNMP credentials that answered are gone; add it by hand).
    pub reason: String,
}

/// Body of the answer of `POST /api/network-scans/{id}/add`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = NetworkScanAddResult)]
pub struct AddResult {
    pub added: Vec<Added>,
    pub skipped: Vec<Skipped>,
}

/// Body of dismiss and restore.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[schema(as = NetworkScanAddress)]
#[serde(deny_unknown_fields)]
pub struct AddressRequest {
    pub address: String,
}

fn check_name(name: &str) -> Result<String, ApiError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME || name.chars().any(char::is_control) {
        return Err(ApiError::Invalid(format!(
            "`name` must be 1 to {MAX_NAME} characters without control characters"
        )));
    }
    Ok(name.to_owned())
}

fn check_every(every: Option<&str>) -> Result<Option<String>, ApiError> {
    match every {
        None => Ok(None),
        Some(e @ ("hour" | "day" | "week")) => Ok(Some(e.to_owned())),
        Some(_) => Err(ApiError::Invalid(
            "`every` must be `hour`, `day`, `week` or null".into(),
        )),
    }
}

/// The probe a scan asks for, resolved in its site.
async fn probe_of(
    db: &mut PgConnection,
    site: Uuid,
    wanted: Option<&str>,
) -> Result<RunBy, ApiError> {
    match wanted {
        None => Ok(super::sensors::default_probe(&mut *db, site).await?),
        Some(probe) if probe == monitorflow_probe::CENTRAL => Ok(RunBy {
            probe: Some("central"),
            key: probe.into(),
            ..RunBy::default()
        }),
        Some(relay) => {
            let id: Option<Uuid> = relay.parse().ok();
            let peer: Option<String> = sqlx::query_scalar(
                "SELECT peer_id FROM relays WHERE id = $1 AND site_id = $2 AND revoked_at IS NULL",
            )
            .bind(id)
            .bind(site)
            .fetch_optional(&mut *db)
            .await?;
            Ok(RunBy {
                probe: Some("relay"),
                relay: id,
                key: peer.ok_or_else(|| {
                    ApiError::Invalid(
                        "`probe` must be `central` or the ID of a relay of the site".into(),
                    )
                })?,
                address: None,
            })
        }
    }
}

async fn record(
    db: &mut PgConnection,
    user: &CurrentUser,
    action: &str,
    scan: Uuid,
    details: serde_json::Value,
) -> Result<(), sqlx::Error> {
    let actor = user.id.to_string();
    let target = scan.to_string();
    audit::record(
        db,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action,
            target: Some(("network_scan", &target)),
            details,
        },
    )
    .await
}

type RunRow = (
    Uuid,
    String,
    String,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    i32,
    i32,
    i32,
    bool,
    i32,
    Vec<String>,
    i32,
    Option<String>,
    Option<String>,
);

fn run_view(row: RunRow) -> RunView {
    let (id, state, trigger, queued_at, started_at, finished_at, addresses, scanned, answered) = (
        row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7, row.8,
    );
    RunView {
        id,
        state,
        trigger,
        queued_at,
        started_at,
        finished_at,
        addresses,
        scanned,
        answered,
        compared: row.9,
        new: row.10,
        gone: row.11,
        dropped: row.12,
        ping_error: row.13,
        error: row.14,
    }
}

async fn runs(
    db: &mut PgConnection,
    scan: Uuid,
    filter: &str,
) -> Result<Vec<RunView>, sqlx::Error> {
    let sql = match filter {
        "latest" => {
            "SELECT id, state, trigger, queued_at, started_at, finished_at, addresses, scanned,
                answered, compared, new_count, array(SELECT host(g) FROM unnest(gone) g), dropped,
                ping_error, error
             FROM network_scan_runs WHERE scan_id = $1 ORDER BY queued_at DESC LIMIT 1"
        }
        "done" => {
            "SELECT id, state, trigger, queued_at, started_at, finished_at, addresses, scanned,
                answered, compared, new_count, array(SELECT host(g) FROM unnest(gone) g), dropped,
                ping_error, error
             FROM network_scan_runs WHERE scan_id = $1 AND state = 'done'
             ORDER BY finished_at DESC LIMIT 1"
        }
        "current" => {
            "SELECT id, state, trigger, queued_at, started_at, finished_at, addresses, scanned,
                answered, compared, new_count, array(SELECT host(g) FROM unnest(gone) g), dropped,
                ping_error, error
             FROM network_scan_runs WHERE scan_id = $1 AND state IN ('queued', 'running') LIMIT 1"
        }
        _ => {
            "SELECT id, state, trigger, queued_at, started_at, finished_at, addresses, scanned,
                answered, compared, new_count, array(SELECT host(g) FROM unnest(gone) g), dropped,
                ping_error, error
             FROM network_scan_runs WHERE scan_id = $1 ORDER BY queued_at DESC LIMIT 10"
        }
    };
    let rows: Vec<RunRow> = sqlx::query_as(sql).bind(scan).fetch_all(&mut *db).await?;
    Ok(rows.into_iter().map(run_view).collect())
}

type ScanRow = (
    Uuid,
    Uuid,
    String,
    String,
    Vec<String>,
    Vec<i32>,
    String,
    Option<Uuid>,
    Option<String>,
    Option<String>,
    bool,
    Option<String>,
    DateTime<Utc>,
    DateTime<Utc>,
);

const SCAN_SELECT: &str = "SELECT s.id, s.site_id, si.name, s.name, s.targets, s.ports, s.probe,
        s.probe_relay_id, rl.peer_id, s.every, s.notify, s.snmp_secret, s.created_at, s.updated_at
     FROM network_scans s JOIN sites si ON si.id = s.site_id
     LEFT JOIN relays rl ON rl.id = s.probe_relay_id";

async fn scan_view(db: &mut PgConnection, row: ScanRow) -> Result<ScanView, sqlx::Error> {
    let (id, site_id, site_name, name, targets, ports, probe, probe_relay_id, peer) = (
        row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7, row.8,
    );
    let snmp = row
        .11
        .as_deref()
        .and_then(|s| netscan::open(s).ok())
        .map(|c| SnmpView {
            version: c.version,
            port: c.port,
            user: c.user,
            auth_protocol: c.auth_protocol,
            priv_protocol: c.priv_protocol,
        });
    let last_run = runs(db, id, "latest").await?.into_iter().next();
    Ok(ScanView {
        addresses: targets::expand(&targets).map_or(0, |(_, a)| a.len()),
        id,
        site_id,
        site_name,
        name,
        targets,
        ports: ports
            .into_iter()
            .filter_map(|p| u16::try_from(p).ok())
            .collect(),
        probe_key: peer.unwrap_or_else(|| monitorflow_probe::CENTRAL.into()),
        probe,
        probe_relay_id,
        every: row.9,
        notify: row.10,
        snmp,
        created_at: row.12,
        updated_at: row.13,
        last_run,
    })
}

/// The scan `id` and its site, after checking the user's right on it.
async fn scan_site(
    db: &mut PgConnection,
    user: &CurrentUser,
    id: Uuid,
    modify: bool,
) -> Result<Uuid, ApiError> {
    let site: Option<Uuid> = sqlx::query_scalar("SELECT site_id FROM network_scans WHERE id = $1")
        .bind(id)
        .fetch_optional(&mut *db)
        .await?;
    authorize(user, site, modify)
}

async fn view_of(db: &mut PgConnection, id: Uuid) -> Result<ScanView, ApiError> {
    let row: ScanRow = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SCAN_SELECT} WHERE s.id = $1"
    )))
    .bind(id)
    .fetch_optional(&mut *db)
    .await?
    .ok_or(ApiError::NotFound)?;
    Ok(scan_view(db, row).await?)
}

/// Queues a run asked for by `user`; 409 when one waits or runs.
async fn queue(db: &mut PgConnection, user: &CurrentUser, scan: Uuid) -> Result<Uuid, ApiError> {
    let run: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO network_scan_runs (scan_id, trigger, requested_by) VALUES ($1, 'person', $2)
         ON CONFLICT (scan_id) WHERE state IN ('queued', 'running') DO NOTHING RETURNING id",
    )
    .bind(scan)
    .bind(user.id)
    .fetch_optional(&mut *db)
    .await?;
    run.ok_or_else(|| ApiError::Conflict("this scan is already queued or running".into()))
}

fn save_snmp(credentials: &snmp::Credentials) -> Result<String, ApiError> {
    netscan::seal(credentials).map_err(ApiError::Internal)
}

/// `GET /api/network-scans`: the scans of the sites the user reads.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
    Query(query): Query<ListQuery>,
) -> Result<Json<ScanList>, ApiError> {
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let (all, sites) = user.readable_sites().bind();
    let mut conn = state.pool.acquire().await?;
    let rows: Vec<ScanRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SCAN_SELECT} WHERE ($1 OR s.site_id = ANY($2)) AND ($3::uuid IS NULL OR s.site_id = $3)
         ORDER BY si.name, s.name, s.id"
    )))
    .bind(all)
    .bind(&sites)
    .bind(query.site_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut scans = Vec::with_capacity(rows.len());
    for row in rows {
        scans.push(scan_view(&mut conn, row).await?);
    }
    Ok(Json(ScanList { scans }))
}

/// `POST /api/network-scans`: 201 with the scan, queued at once unless
/// `run` is false.
pub async fn create(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<ScanRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<ScanView>), ApiError> {
    let request = body(request)?;
    let exists: Option<Uuid> = sqlx::query_scalar("SELECT id FROM sites WHERE id = $1")
        .bind(request.site_id)
        .fetch_optional(&state.pool)
        .await?;
    let site = authorize(&user, exists, true)?;
    let name = check_name(&request.name)?;
    let (entries, addresses) = targets::expand(&request.targets).map_err(ApiError::Invalid)?;
    let ports = targets::ports(request.ports.as_deref()).map_err(ApiError::Invalid)?;
    let every = check_every(request.every.as_deref())?;
    let credentials = request
        .snmp
        .as_ref()
        .map(|r| snmp::check(r, None))
        .transpose()
        .map_err(ApiError::Invalid)?;
    let sealed = credentials.as_ref().map(save_snmp).transpose()?;
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT 1 FROM sites WHERE id = $1 FOR UPDATE")
        .bind(site)
        .execute(&mut *tx)
        .await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM network_scans WHERE site_id = $1")
        .bind(site)
        .fetch_one(&mut *tx)
        .await?;
    if count >= MAX_PER_SITE {
        return Err(ApiError::Conflict(format!(
            "a site has at most {MAX_PER_SITE} scans; remove one first"
        )));
    }
    let by = probe_of(&mut tx, site, request.probe.as_deref()).await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO network_scans (site_id, name, targets, ports, probe, probe_relay_id, every,
                                    notify, snmp_secret, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
    )
    .bind(site)
    .bind(&name)
    .bind(&entries)
    .bind(ports.iter().map(|p| i32::from(*p)).collect::<Vec<_>>())
    .bind(by.probe)
    .bind(by.relay)
    .bind(&every)
    .bind(request.notify)
    .bind(&sealed)
    .bind(user.id)
    .fetch_one(&mut *tx)
    .await?;
    record(
        &mut tx,
        &user,
        "network_scan.create",
        id,
        json!({
            "site_id": site, "name": name, "targets": entries, "addresses": addresses.len(),
            "ports": ports, "probe": by.key, "every": every, "notify": request.notify,
            "snmp": credentials.as_ref().map(snmp::audit),
        }),
    )
    .await?;
    if request.run {
        let run = queue(&mut tx, &user, id).await?;
        record(
            &mut tx,
            &user,
            "network_scan.run",
            id,
            json!({ "run_id": run }),
        )
        .await?;
    }
    let view = view_of(&mut tx, id).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(view)))
}

/// `GET /api/network-scans/{id}`.
pub async fn get(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<ScanView>, ApiError> {
    let id = parse_id(&id)?;
    let mut conn = state.pool.acquire().await?;
    scan_site(&mut conn, &user, id, false).await?;
    Ok(Json(view_of(&mut conn, id).await?))
}

/// `PUT /api/network-scans/{id}`: replaces it but its site and runs.
pub async fn update(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<ScanUpdate>, JsonRejection>,
) -> Result<Json<ScanView>, ApiError> {
    let id = parse_id(&id)?;
    let request = body(request)?;
    let mut tx = state.pool.begin().await?;
    let site = scan_site(&mut tx, &user, id, true).await?;
    let stored: Option<String> =
        sqlx::query_scalar("SELECT snmp_secret FROM network_scans WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    let name = check_name(&request.name)?;
    let (entries, addresses) = targets::expand(&request.targets).map_err(ApiError::Invalid)?;
    let ports = targets::ports(request.ports.as_deref()).map_err(ApiError::Invalid)?;
    let every = check_every(request.every.as_deref())?;
    let before = stored.as_deref().and_then(|s| netscan::open(s).ok());
    let credentials = match (&request.snmp, request.remove_snmp) {
        (_, true) => None,
        (Some(r), false) => Some(snmp::check(r, before.as_ref()).map_err(ApiError::Invalid)?),
        (None, false) => before,
    };
    let sealed = match (&request.snmp, request.remove_snmp) {
        (None, false) => stored,
        _ => credentials.as_ref().map(save_snmp).transpose()?,
    };
    let by = probe_of(&mut tx, site, request.probe.as_deref()).await?;
    sqlx::query(
        "UPDATE network_scans SET name = $2, targets = $3, ports = $4, probe = $5,
             probe_relay_id = $6, every = $7, notify = $8, snmp_secret = $9, updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(&name)
    .bind(&entries)
    .bind(ports.iter().map(|p| i32::from(*p)).collect::<Vec<_>>())
    .bind(by.probe)
    .bind(by.relay)
    .bind(&every)
    .bind(request.notify)
    .bind(&sealed)
    .execute(&mut *tx)
    .await?;
    record(
        &mut tx,
        &user,
        "network_scan.update",
        id,
        json!({
            "name": name, "targets": entries, "addresses": addresses.len(), "ports": ports,
            "probe": by.key, "every": every, "notify": request.notify,
            "snmp": credentials.as_ref().map(snmp::audit),
        }),
    )
    .await?;
    let view = view_of(&mut tx, id).await?;
    tx.commit().await?;
    Ok(Json(view))
}

/// `DELETE /api/network-scans/{id}`: 204, with its runs; a run in
/// progress stops at its next chunk. Devices it added stay.
pub async fn delete(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    let mut tx = state.pool.begin().await?;
    scan_site(&mut tx, &user, id, true).await?;
    let name: String = sqlx::query_scalar("DELETE FROM network_scans WHERE id = $1 RETURNING name")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    record(
        &mut tx,
        &user,
        "network_scan.delete",
        id,
        json!({ "name": name }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/network-scans/{id}/run`: 202 with the queued run.
pub async fn run(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<RunView>), ApiError> {
    let id = parse_id(&id)?;
    let mut tx = state.pool.begin().await?;
    scan_site(&mut tx, &user, id, true).await?;
    let run = queue(&mut tx, &user, id).await?;
    record(
        &mut tx,
        &user,
        "network_scan.run",
        id,
        json!({ "run_id": run }),
    )
    .await?;
    let view = runs(&mut tx, id, "current")
        .await?
        .into_iter()
        .next()
        .ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok((StatusCode::ACCEPTED, Json(view)))
}

/// `GET /api/network-scans/{id}/runs`: the latest 10.
pub async fn list_runs(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<RunList>, ApiError> {
    let id = parse_id(&id)?;
    let mut conn = state.pool.acquire().await?;
    scan_site(&mut conn, &user, id, false).await?;
    Ok(Json(RunList {
        runs: runs(&mut conn, id, "all").await?,
    }))
}

type HostRow = (
    String,
    bool,
    Option<f64>,
    Vec<i32>,
    Option<String>,
    Option<String>,
    bool,
    Option<String>,
    Option<String>,
    Option<String>,
    Vec<String>,
    Option<String>,
    bool,
    bool,
    Option<Uuid>,
);

/// A stored host, and whether it is new and where its SNMP credentials
/// came from.
struct Stored {
    host: Host,
    snmp: bool,
    new: bool,
    from_scan: bool,
    device: Option<Uuid>,
}

async fn hosts(db: &mut PgConnection, run: Uuid) -> Result<Vec<Stored>, sqlx::Error> {
    let rows: Vec<HostRow> = sqlx::query_as(
        "SELECT host(address), ping, rtt_seconds, ports, name, mac, snmp, snmp_descr, snmp_name,
                snmp_object_id, snmp_groups, template, is_new, snmp_from_scan, snmp_device_id
         FROM network_scan_hosts WHERE run_id = $1 ORDER BY address",
    )
    .bind(run)
    .fetch_all(&mut *db)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let mut host = Host::new(r.0.parse().ok()?);
            host.ping = r.1;
            host.rtt_seconds = r.2;
            host.ports =
                r.3.into_iter()
                    .filter_map(|p| u16::try_from(p).ok())
                    .collect();
            host.name = r.4;
            host.mac = r.5;
            host.snmp_set = r.6.then_some(1);
            host.snmp_descr = r.7;
            host.snmp_name = r.8;
            host.snmp_object_id = r.9;
            host.snmp_groups = r.10;
            host.template = r.11;
            Some(Stored {
                host,
                snmp: r.6,
                new: r.12,
                from_scan: r.13,
                device: r.14,
            })
        })
        .collect())
}

async fn known(db: &mut PgConnection, site: Uuid) -> Result<Vec<Known>, sqlx::Error> {
    type Row = (Uuid, String, String, Option<String>, Vec<String>);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, hostname, kind, address, mac_addresses FROM servers
         WHERE site_id = $1 AND revoked_at IS NULL",
    )
    .bind(site)
    .fetch_all(&mut *db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, name, kind, address, macs)| Known {
            id,
            name,
            kind,
            address,
            macs,
        })
        .collect())
}

async fn dismissed(db: &mut PgConnection, scan: Uuid) -> Result<BTreeSet<IpAddr>, sqlx::Error> {
    let rows: Vec<String> =
        sqlx::query_scalar("SELECT host(address) FROM network_scan_dismissals WHERE scan_id = $1")
            .bind(scan)
            .fetch_all(&mut *db)
            .await?;
    Ok(rows.into_iter().filter_map(|a| a.parse().ok()).collect())
}

/// `GET /api/network-scans/{id}/results`: the latest finished run's
/// addresses, each with its proposal.
pub async fn results(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<Results>, ApiError> {
    let id = parse_id(&id)?;
    let mut conn = state.pool.acquire().await?;
    let site = scan_site(&mut conn, &user, id, false).await?;
    let run = runs(&mut conn, id, "done").await?.into_iter().next();
    let current = runs(&mut conn, id, "current").await?.into_iter().next();
    let mut views = Vec::new();
    if let Some(run) = &run {
        let known = known(&mut conn, site).await?;
        let dismissed = dismissed(&mut conn, id).await?;
        for stored in hosts(&mut conn, run.id).await? {
            let host = stored.host;
            let proposal = proposals::propose(&host, &known, dismissed.contains(&host.address));
            views.push(HostView {
                address: host.address.to_string(),
                ping: host.ping,
                rtt_seconds: host.rtt_seconds,
                ports: host.ports,
                name: host.name,
                mac: host.mac,
                snmp: stored.snmp,
                snmp_description: host.snmp_descr,
                snmp_name: host.snmp_name,
                snmp_object_id: host.snmp_object_id,
                snmp_groups: host.snmp_groups,
                new: stored.new,
                proposal,
            });
        }
    }
    Ok(Json(Results {
        run,
        current,
        hosts: views,
    }))
}

fn parse_address(address: &str) -> Result<IpAddr, ApiError> {
    address
        .trim()
        .parse()
        .map_err(|_| ApiError::Invalid("`address` must be an IP address".into()))
}

/// Adds the port checks of a device, run by `by`, with their alert:
/// critical when the port stays closed for 2 minutes.
async fn add_ports(
    db: &mut PgConnection,
    device: Uuid,
    ports: &[u16],
    by: &RunBy,
) -> Result<(), sqlx::Error> {
    for port in ports {
        sqlx::query(
            "INSERT INTO sensor_configs (server_id, kind, name, instance, interval_ms, params, probe, probe_relay_id)
             VALUES ($1, 'port', $2, $3, 60000, $4, $5, $6)
             ON CONFLICT ON CONSTRAINT sensor_configs_key DO NOTHING",
        )
        .bind(device)
        .bind(format!("port_{port}"))
        .bind(&by.key)
        .bind(json!({ "port": port.to_string() }))
        .bind(by.probe)
        .bind(by.relay)
        .execute(&mut *db)
        .await?;
        sqlx::query(
            "INSERT INTO thresholds (server_id, metric, comparison, warning, critical, duration_s)
             VALUES ($1, $2, '<', NULL, 1, 120) ON CONFLICT DO NOTHING",
        )
        .bind(device)
        .bind(format!("monitorflow_port_{port}_up"))
        .execute(&mut *db)
        .await?;
    }
    Ok(())
}

/// The SNMP credentials that answered for `stored`.
async fn answered_with(
    db: &mut PgConnection,
    stored: &Stored,
    scan_secret: Option<&str>,
) -> Result<Option<snmp::Credentials>, sqlx::Error> {
    if stored.from_scan {
        return Ok(scan_secret.and_then(|s| netscan::open(s).ok()));
    }
    match stored.device {
        Some(device) => Ok(snmp::load(&mut *db, device).await?.and_then(Result::ok)),
        None => Ok(None),
    }
}

/// `POST /api/network-scans/{id}/add`: adds devices from the latest
/// finished run.
pub async fn add(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<AddRequest>, JsonRejection>,
) -> Result<Json<AddResult>, ApiError> {
    let id = parse_id(&id)?;
    let request = body(request)?;
    if !request.all && request.devices.is_empty() {
        return Err(ApiError::Invalid("give `devices` or `all`".into()));
    }
    let mut asked: BTreeMap<IpAddr, &AddDevice> = BTreeMap::new();
    for device in &request.devices {
        let address = parse_address(&device.address)?;
        if let Some(name) = &device.name {
            check_name(name)?;
        }
        if let Some(template) = &device.template {
            snmp::template(template).ok_or_else(|| {
                ApiError::Invalid(format!("`{}` is not a template", template.escape_debug()))
            })?;
        }
        asked.insert(address, device);
    }
    let mut tx = state.pool.begin().await?;
    let site = scan_site(&mut tx, &user, id, true).await?;
    let (probe, relay, secret): (String, Option<Uuid>, Option<String>) = sqlx::query_as(
        "SELECT probe, probe_relay_id, snmp_secret FROM network_scans WHERE id = $1 FOR UPDATE",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    let run: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM network_scan_runs WHERE scan_id = $1 AND state = 'done'
         ORDER BY finished_at DESC LIMIT 1",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let stored = match run {
        Some(run) => hosts(&mut tx, run).await?,
        None => Vec::new(),
    };
    let dismissed = dismissed(&mut tx, id).await?;
    let mut known = known(&mut tx, site).await?;
    let mut result = AddResult {
        added: Vec::new(),
        skipped: Vec::new(),
    };
    let skip = |result: &mut AddResult, address: IpAddr, reason: &str| {
        result.skipped.push(Skipped {
            address: address.to_string(),
            reason: reason.into(),
        });
    };
    for address in asked.keys() {
        if !stored.iter().any(|s| s.host.address == *address) {
            skip(&mut result, *address, "not_found");
        }
    }
    let wanted = match relay {
        Some(relay) => relay.to_string(),
        None => probe,
    };
    for entry in &stored {
        let host = &entry.host;
        let asked = asked.get(&host.address);
        let proposal = proposals::propose(host, &known, dismissed.contains(&host.address));
        let wanted_all =
            request.all && matches!(proposal.action.as_str(), "device" | "snmp_device");
        if asked.is_none() && !wanted_all {
            continue;
        }
        if proposal.action == "monitored" {
            skip(&mut result, host.address, "monitored");
            continue;
        }
        let template = asked
            .and_then(|a| a.template.clone())
            .or_else(|| {
                (proposal.action == "snmp_device")
                    .then(|| proposal.template.clone())
                    .flatten()
            })
            .filter(|_| entry.snmp);
        let credentials = match template {
            Some(_) => match answered_with(&mut tx, entry, secret.as_deref()).await? {
                Some(credentials) => Some(credentials),
                None => {
                    skip(&mut result, host.address, "no_credentials");
                    continue;
                }
            },
            None => None,
        };
        let name = asked
            .and_then(|a| a.name.clone())
            .map(|n| n.trim().to_owned())
            .unwrap_or_else(|| proposal.name.clone());
        let address = host.address.to_string();
        let (device, peer_id) = devices::insert(&mut tx, site, &name, &address).await?;
        let by = run_by(&mut tx, Owner::Server(device), Some(&wanted)).await?;
        if host.ping {
            devices::add_ping(&mut tx, device, &by).await?;
        }
        match (&credentials, template.as_deref().and_then(snmp::template)) {
            (Some(credentials), Some(template)) => {
                snmp::save(&mut tx, device, credentials)
                    .await
                    .map_err(|e| match e {
                        snmp::SaveError::Database(e) => ApiError::Database(e),
                        snmp::SaveError::Seal(e) => ApiError::Internal(e),
                    })?;
                devices::add_template(&mut tx, device, template, &by).await?;
            }
            _ => add_ports(&mut tx, device, &host.ports, &by).await?,
        }
        let actor = user.id.to_string();
        let target = device.to_string();
        audit::record(
            &mut *tx,
            Event {
                actor_kind: "user",
                actor_id: &actor,
                action: "device.create",
                target: Some(("server", &target)),
                details: json!({
                    "peer_id": peer_id, "site_id": site, "name": name, "address": address,
                    "probe": by.key, "template": template, "network_scan_id": id,
                    "snmp": credentials.as_ref().map(snmp::audit),
                }),
            },
        )
        .await?;
        known.push(Known {
            id: device,
            name: name.clone(),
            kind: "device".into(),
            address: Some(address.clone()),
            macs: Vec::new(),
        });
        result.added.push(Added {
            address,
            server_id: device,
            name,
            template,
        });
    }
    if !result.added.is_empty() {
        record(
            &mut tx,
            &user,
            "network_scan.add",
            id,
            json!({
                "all": request.all,
                "added": result.added.iter().map(|a| json!({"address": a.address, "server_id": a.server_id})).collect::<Vec<_>>(),
            }),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(Json(result))
}

async fn dismissal(
    state: &ApiState,
    user: &CurrentUser,
    id: &str,
    request: Result<Json<AddressRequest>, JsonRejection>,
    dismiss: bool,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(id)?;
    let address = parse_address(&body(request)?.address)?;
    let mut tx = state.pool.begin().await?;
    scan_site(&mut tx, user, id, true).await?;
    let changed = if dismiss {
        // Only an address the scan found, so the table holds what scans
        // saw, never text from a request.
        sqlx::query(
            "INSERT INTO network_scan_dismissals (scan_id, address, dismissed_by)
             SELECT $1, $2::inet, $3 WHERE EXISTS (
                 SELECT 1 FROM network_scan_hosts h JOIN network_scan_runs r ON r.id = h.run_id
                 WHERE r.scan_id = $1 AND h.address = $2::inet)
             ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .bind(address.to_string())
        .bind(user.id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
    } else {
        sqlx::query("DELETE FROM network_scan_dismissals WHERE scan_id = $1 AND address = $2::inet")
            .bind(id)
            .bind(address.to_string())
            .execute(&mut *tx)
            .await?
            .rows_affected()
    };
    if changed == 0 {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM network_scan_dismissals WHERE scan_id = $1 AND address = $2::inet)",
        )
        .bind(id)
        .bind(address.to_string())
        .fetch_one(&mut *tx)
        .await?;
        // Dismissing twice or restoring what is not dismissed is done.
        if dismiss != exists {
            return Err(ApiError::NotFound);
        }
        return Ok(StatusCode::NO_CONTENT);
    }
    let action = if dismiss {
        "network_scan.dismiss"
    } else {
        "network_scan.restore"
    };
    record(
        &mut tx,
        user,
        action,
        id,
        json!({ "address": address.to_string() }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/network-scans/{id}/dismiss`: 204; the scan stops proposing
/// the address. 404 for an address it never found.
pub async fn dismiss(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<AddressRequest>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    dismissal(&state, &user, &id, request, true).await
}

/// `POST /api/network-scans/{id}/restore`: 204; proposed again.
pub async fn restore(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<AddressRequest>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    dismissal(&state, &user, &id, request, false).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_schedules_are_checked() {
        assert_eq!(check_name(" Office ").unwrap(), "Office");
        assert!(check_name("").is_err());
        assert!(check_name("a\tb").is_err());
        assert_eq!(check_every(Some("day")).unwrap().as_deref(), Some("day"));
        assert_eq!(check_every(None).unwrap(), None);
        assert!(check_every(Some("month")).is_err());
    }
}
