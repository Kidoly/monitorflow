//! Devices (ADR-0148): addresses without an agent, watched by network
//! checks from the central server or a relay. A device is a `servers` row
//! of kind `device`, so it is read, silenced, removed and charted with the
//! server routes; these routes add one and change its name or address,
//! and list where a site's checks can run from.
//!
//! Operators of the site add and change devices, as they add checks.

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use monitorflow_common::tls::PeerRole;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgConnection;
use uuid::Uuid;

use super::access::authorize;
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::sensors::{body, default_probe, parse_id, RunBy};
use super::servers::{self, ServerView};
use super::{ApiError, ApiState};
use crate::enroll::{identity, install_key};
use crate::snmp;
use crate::vmware;

/// Longest name of a device.
pub const MAX_NAME: usize = 128;
/// Interval of the ping a new device gets.
pub const PING_INTERVAL_MS: i64 = 60_000;

/// Body of `POST /api/devices`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[schema(as = DeviceRequest)]
#[serde(deny_unknown_fields)]
pub struct DeviceRequest {
    /// The site it is in; its relays can check it.
    pub site_id: Uuid,
    /// What people call it, like `Core switch`: 1 to 128 characters
    /// without control characters. Its ID is made from it.
    pub name: String,
    /// Where its checks reach it: an IP address or a DNS name.
    pub address: String,
    /// Where its ping runs from: `central`, or the ID of a relay of the
    /// site. Absent: the relay of the site when there is one, else the
    /// central server.
    #[serde(default)]
    pub probe: Option<String>,
    /// Whether to add a ping every minute, with the thresholds that come
    /// with it: critical when it gets no answer for 2 minutes, a warning
    /// above 20% packet loss for 5 minutes. `true` by default.
    #[serde(default = "yes")]
    pub ping: bool,
    /// How to reach it over SNMP (ADR-0149); needed with `template`.
    /// Stored sealed, never returned.
    #[serde(default)]
    pub snmp: Option<snmp::CredentialsRequest>,
    /// The kind of device, whose SNMP checks and thresholds it gets, run
    /// from the same probe as its ping: `switch`, `router`, `firewall`,
    /// `printer`, `ups`, `nas` or `generic`. See
    /// `GET /api/devices/templates`, and `POST /api/devices/detect` for the
    /// one that fits.
    #[serde(default)]
    pub template: Option<String>,
    /// For a vCenter or a standalone ESXi host (ADR-0156): how its probe
    /// logs in, with a Read-only account. The device then gets its
    /// `vmware` check every 5 minutes from the same probe as its ping,
    /// with the alerts that come with it. Test it first with
    /// `POST /api/devices/vmware/test`. Stored sealed, never returned.
    #[serde(default)]
    pub vmware: Option<vmware::CredentialsRequest>,
}

fn yes() -> bool {
    true
}

/// Body of `PUT /api/devices/{id}`: what changes, the rest stays.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[schema(as = DeviceUpdate)]
#[serde(deny_unknown_fields)]
pub struct DeviceUpdate {
    pub name: Option<String>,
    pub address: Option<String>,
}

/// One place a site's checks can run from.
#[derive(Debug, Serialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct Probe {
    /// `central` or `relay`.
    #[schema(example = "relay")]
    pub probe: String,
    /// The relay; `null` for the central server.
    pub relay_id: Option<Uuid>,
    /// `central`, or the relay's name: the instance its checks default
    /// to.
    pub key: String,
    /// The relay's name for people; `null` for the central server.
    pub name: Option<String>,
    /// Whether it is connected now; the central server always is.
    pub online: bool,
    /// What a new check of a device of this site runs from when it names
    /// none.
    pub default: bool,
}

/// Body of `GET /api/sites/{id}/probes`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ProbeList {
    /// The central server first, then the relays by name.
    pub probes: Vec<Probe>,
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

/// An IP address or a DNS name, as the checks take it.
pub fn check_address(address: &str) -> Result<String, ApiError> {
    let address = address.trim();
    monitorflow_sensors::port::parse_host(address)
        .map_err(|_| ApiError::Invalid("`address` must be an IP address or a DNS name".into()))?;
    Ok(address
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned())
}

/// Adds a device named `name` at `address` to `site` in `tx`, with an ID
/// made from its name; returns its row ID and peer ID. Callers check
/// access, and audit.
pub async fn insert(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    site: Uuid,
    name: &str,
    address: &str,
) -> Result<(Uuid, String), ApiError> {
    identity::lock(tx).await?;
    let base = install_key::peer_id_from_hostname(name);
    let peer_id = install_key::new_id(tx, &base)
        .await
        .map_err(|e| ApiError::Conflict(e.to_string()))?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family, kind, address, package_updates)
         VALUES ($1, $2, $3, 'device', 'device', $4, false) RETURNING id",
    )
    .bind(&peer_id)
    .bind(site)
    .bind(name)
    .bind(address)
    .fetch_one(&mut **tx)
    .await?;
    Ok((id, peer_id))
}

/// Adds the ping of a device run by `by`, and its thresholds.
pub async fn add_ping(db: &mut PgConnection, device: Uuid, by: &RunBy) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO sensor_configs (server_id, kind, name, instance, interval_ms, params, probe, probe_relay_id)
         VALUES ($1, 'ping', 'ping', $2, $3, '{}', $4, $5)",
    )
    .bind(device)
    .bind(&by.key)
    .bind(PING_INTERVAL_MS)
    .bind(by.probe)
    .bind(by.relay)
    .execute(&mut *db)
    .await?;
    sqlx::query(
        "INSERT INTO thresholds (server_id, metric, comparison, warning, critical, duration_s)
         VALUES ($1, 'monitorflow_ping_up', '<', NULL, 1, 120),
                ($1, 'monitorflow_ping_loss_ratio', '>', 0.2, NULL, 300)
         ON CONFLICT DO NOTHING",
    )
    .bind(device)
    .execute(&mut *db)
    .await?;
    Ok(())
}

async fn record(
    db: &mut PgConnection,
    user: &CurrentUser,
    action: &str,
    id: Uuid,
    details: serde_json::Value,
) -> Result<(), sqlx::Error> {
    let actor_id = user.id.to_string();
    let target = id.to_string();
    audit::record(
        db,
        Event {
            actor_kind: "user",
            actor_id: &actor_id,
            action,
            target: Some(("server", &target)),
            details,
        },
    )
    .await
}

/// The view of a device just written.
async fn view(state: &ApiState, id: Uuid) -> Result<ServerView, ApiError> {
    servers::view(state, id).await?.ok_or(ApiError::NotFound)
}

/// `POST /api/devices`: 201 with the new device as a server.
pub async fn create(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<DeviceRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<ServerView>), ApiError> {
    let request = body(request)?;
    let exists: Option<Uuid> = sqlx::query_scalar("SELECT id FROM sites WHERE id = $1")
        .bind(request.site_id)
        .fetch_optional(&state.pool)
        .await?;
    let site = authorize(&user, exists, true)?;
    let name = check_name(&request.name)?;
    let address = check_address(&request.address)?;
    let template = match request.template.as_deref() {
        None => None,
        Some(key) => Some(snmp::template(key).ok_or_else(|| {
            ApiError::Invalid("`template` must be `switch`, `router`, `firewall`, `printer`, `ups`, `nas` or `generic`".into())
        })?),
    };
    let credentials = match (&request.snmp, template) {
        (Some(request), _) => Some(snmp::check(request, None).map_err(ApiError::Invalid)?),
        (None, Some(_)) => {
            return Err(ApiError::Invalid(
                "a `template` needs `snmp` credentials".into(),
            ))
        }
        (None, None) => None,
    };
    let vcenter = request
        .vmware
        .as_ref()
        .map(|v| vmware::check(v, None).map_err(ApiError::Invalid))
        .transpose()?;
    let mut tx = state.pool.begin().await?;
    let (id, peer_id) = insert(&mut tx, site, &name, &address).await?;
    let probe = super::sensors::run_by(
        &mut tx,
        super::sensors::Owner::Server(id),
        request.probe.as_deref(),
    )
    .await?;
    let by = if request.ping {
        add_ping(&mut tx, id, &probe).await?;
        Some(&probe)
    } else {
        None
    };
    if let Some(credentials) = &credentials {
        snmp::save(&mut tx, id, credentials)
            .await
            .map_err(save_error)?;
    }
    if let Some(template) = template {
        add_template(&mut tx, id, template, &probe).await?;
    }
    if let Some(credentials) = &vcenter {
        vmware::save(&mut tx, id, credentials)
            .await
            .map_err(vmware_save_error)?;
        add_vmware(&mut tx, id, &probe).await?;
    }
    record(
        &mut tx,
        &user,
        "device.create",
        id,
        json!({
            "peer_id": peer_id, "site_id": site, "name": name, "address": address,
            "ping": by.map(|b| json!({"probe": b.probe, "probe_relay_id": b.relay})),
            "template": template.map(|t| t.key),
            "snmp": credentials.as_ref().map(snmp::audit),
            "vmware": vcenter.as_ref().map(vmware::audit),
        }),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(user = %user.id, device = %peer_id, "device added");
    Ok((StatusCode::CREATED, Json(view(&state, id).await?)))
}

/// `PUT /api/devices/{id}`: 200 with the device.
pub async fn update(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<DeviceUpdate>, JsonRejection>,
) -> Result<Json<ServerView>, ApiError> {
    let id = parse_id(&id)?;
    let request = body(request)?;
    let mut tx = state.pool.begin().await?;
    let row: Option<(Uuid, String, String, Option<String>)> = sqlx::query_as(
        "SELECT site_id, kind, hostname, address FROM servers
         WHERE id = $1 AND revoked_at IS NULL FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    authorize(&user, row.as_ref().map(|r| r.0), true)?;
    let (_, kind, old_name, old_address) = row.ok_or(ApiError::NotFound)?;
    if kind != "device" {
        return Err(ApiError::NotFound);
    }
    let name = request
        .name
        .as_deref()
        .map(check_name)
        .transpose()?
        .unwrap_or(old_name.clone());
    let address = match request.address.as_deref() {
        Some(address) => check_address(address)?,
        None => old_address.clone().unwrap_or_default(),
    };
    sqlx::query("UPDATE servers SET hostname = $2, address = $3 WHERE id = $1")
        .bind(id)
        .bind(&name)
        .bind(&address)
        .execute(&mut *tx)
        .await?;
    // Its checks follow the address at the probes' next read.
    record(
        &mut tx,
        &user,
        "device.update",
        id,
        json!({
            "before": {"name": old_name, "address": old_address},
            "after": {"name": name, "address": address},
        }),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(view(&state, id).await?))
}

fn save_error(error: snmp::SaveError) -> ApiError {
    match error {
        snmp::SaveError::Database(error) => ApiError::Database(error),
        snmp::SaveError::Seal(error) => ApiError::Internal(error),
    }
}

/// Adds the checks of `template`'s groups, run by `by` every minute, and
/// its thresholds; what the device has already stays.
pub async fn add_template(
    db: &mut PgConnection,
    device: Uuid,
    template: &snmp::Template,
    by: &RunBy,
) -> Result<(), sqlx::Error> {
    for group in template.groups {
        sqlx::query(
            "INSERT INTO sensor_configs (server_id, kind, name, instance, interval_ms, params, probe, probe_relay_id)
             VALUES ($1, 'snmp', $2, $3, 60000, $4, $5, $6)
             ON CONFLICT ON CONSTRAINT sensor_configs_key DO NOTHING",
        )
        .bind(device)
        .bind(snmp::check_name(*group))
        .bind(&by.key)
        .bind(json!({ "what": group.as_str() }))
        .bind(by.probe)
        .bind(by.relay)
        .execute(&mut *db)
        .await?;
    }
    for t in template.thresholds {
        sqlx::query(
            "INSERT INTO thresholds (server_id, metric, comparison, warning, critical, duration_s)
             SELECT $1, $2, $3, $4, $5, $6
             WHERE NOT EXISTS (SELECT 1 FROM thresholds
                               WHERE server_id = $1 AND metric = $2 AND instance IS NULL)",
        )
        .bind(device)
        .bind(t.metric)
        .bind(t.comparison)
        .bind(t.warning)
        .bind(t.critical)
        .bind(t.duration_s)
        .execute(&mut *db)
        .await?;
    }
    Ok(())
}

/// Body of `GET /api/devices/templates`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct TemplateList {
    pub templates: Vec<snmp::Template>,
}

/// `GET /api/devices/templates`: the kinds of SNMP devices. Any role.
pub async fn templates(user: CurrentUser) -> Result<Json<TemplateList>, ApiError> {
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    Ok(Json(TemplateList {
        templates: snmp::TEMPLATES.to_vec(),
    }))
}

/// Body of `POST /api/devices/detect`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DetectRequest {
    pub site_id: Uuid,
    /// An IP address or a DNS name.
    pub address: String,
    /// `central` or a relay of the site; absent: the site's default.
    #[serde(default)]
    pub probe: Option<String>,
    pub snmp: snmp::CredentialsRequest,
}

/// What an address answered over SNMP.
#[derive(Debug, Serialize, PartialEq, utoipa::ToSchema)]
pub struct Detected {
    /// Whether it answered at all with these credentials.
    pub answers: bool,
    /// Why not, in words; `null` when it answered.
    pub error: Option<String>,
    /// sysDescr, sysName and sysObjectID.
    pub description: Option<String>,
    pub name: Option<String>,
    pub object_id: Option<String>,
    /// The groups that answer, among `interfaces`, `host`, `ups`,
    /// `temperature`, `printer`.
    pub groups: Vec<String>,
    /// The template that fits it; `null` when it did not answer.
    pub template: Option<String>,
    /// `central` or the relay that asked.
    pub probe: String,
}

/// What the samples of a `detect` check say.
pub fn detected(samples: &[monitorflow_common::proto::v1::Sample], probe: &str) -> Detected {
    let answers = samples.iter().find(|s| s.sensor.ends_with("answers"));
    let label = |name: &str| {
        answers
            .and_then(|s| s.labels.get(name))
            .filter(|v| !v.is_empty())
            .cloned()
    };
    let groups: Vec<String> = samples
        .iter()
        .filter(|s| s.sensor.ends_with("has") && s.value == 1.0)
        .map(|s| s.instance.clone())
        .collect();
    let has = |g: monitorflow_snmp::Group| groups.iter().any(|x| x == g.as_str());
    Detected {
        answers: answers.is_some(),
        error: None,
        template: answers
            .map(|_| snmp::suggest(&label("descr").unwrap_or_default(), has).to_owned()),
        description: label("descr"),
        name: label("name"),
        object_id: label("object_id"),
        groups,
        probe: probe.to_owned(),
    }
}

/// Who runs a one-off test for `site`: `wanted`, or the site's default.
async fn probe_for(state: &ApiState, site: Uuid, wanted: Option<&str>) -> Result<RunBy, ApiError> {
    let mut conn = state.pool.acquire().await?;
    Ok(match wanted {
        None => default_probe(&mut *conn, site).await?,
        Some(probe) if probe == monitorflow_probe::CENTRAL => RunBy {
            probe: Some("central"),
            key: probe.into(),
            ..RunBy::default()
        },
        Some(relay) => {
            let id: Option<Uuid> = relay.parse().ok();
            let peer: Option<String> = sqlx::query_scalar(
                "SELECT peer_id FROM relays WHERE id = $1 AND site_id = $2 AND revoked_at IS NULL",
            )
            .bind(id)
            .bind(site)
            .fetch_optional(&mut *conn)
            .await?;
            RunBy {
                probe: Some("relay"),
                relay: id,
                key: peer.ok_or_else(|| {
                    ApiError::Invalid(
                        "`probe` must be `central` or the ID of a relay of the site".into(),
                    )
                })?,
                address: None,
            }
        }
    })
}

/// Runs `check` once from `by`: on this central server, or on the relay.
async fn run_once(
    state: &ApiState,
    by: &RunBy,
    check: monitorflow_probe::Check,
) -> Result<Vec<monitorflow_common::proto::v1::Sample>, ApiError> {
    let outcome = match &state.probes {
        Some(probes) => probes.test(&state.registry, &by.key, check).await,
        None if by.probe == Some("central") => {
            monitorflow_probe::test_once(&monitorflow_probe::Builtin, &check)
                .await
                .map(|s| {
                    s.into_iter()
                        .map(monitorflow_probe::sample_to_proto)
                        .collect()
                })
                .map_err(crate::probes::TestError::Failed)
        }
        None => Err(crate::probes::TestError::NotConnected),
    };
    match outcome {
        Ok(samples) => Ok(samples),
        Err(crate::probes::TestError::NotConnected) => Err(ApiError::Conflict(format!(
            "the relay {} is not connected to this central server now; try again in a moment",
            by.key
        ))),
        Err(error) => Err(ApiError::Invalid(error.to_string())),
    }
}

/// `POST /api/devices/detect`: runs a detection once from the probe
/// that would check the address, and suggests a template. Operators of
/// the site, as it sends credentials to the probe. 200 also when nothing
/// answered: `answers` and `error` say so.
pub async fn detect(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<DetectRequest>, JsonRejection>,
) -> Result<Json<Detected>, ApiError> {
    let request = body(request)?;
    let exists: Option<Uuid> = sqlx::query_scalar("SELECT id FROM sites WHERE id = $1")
        .bind(request.site_id)
        .fetch_optional(&state.pool)
        .await?;
    let site = authorize(&user, exists, true)?;
    let address = check_address(&request.address)?;
    let credentials = snmp::check(&request.snmp, None).map_err(ApiError::Invalid)?;
    let by = probe_for(&state, site, request.probe.as_deref()).await?;
    let mut params = credentials.params();
    params.insert("host".into(), address);
    params.insert("what".into(), "detect".into());
    let mut config =
        monitorflow_sensors::SensorConfig::new("snmp", std::time::Duration::from_secs(60));
    config.timeout = Some(std::time::Duration::from_secs(15));
    config.params = params;
    let check = monitorflow_probe::Check {
        device: "detect".into(),
        kind: monitorflow_snmp::KIND.into(),
        config,
    };
    let outcome = match &state.probes {
        Some(probes) => probes.test(&state.registry, &by.key, check).await,
        None if by.probe == Some("central") => {
            monitorflow_probe::test_once(&monitorflow_probe::Builtin, &check)
                .await
                .map(|s| {
                    s.into_iter()
                        .map(monitorflow_probe::sample_to_proto)
                        .collect()
                })
                .map_err(crate::probes::TestError::Failed)
        }
        None => Err(crate::probes::TestError::NotConnected),
    };
    let result = match outcome {
        Ok(samples) => detected(&samples, &by.key),
        Err(crate::probes::TestError::NotConnected) => {
            return Err(ApiError::Conflict(format!(
                "the relay {} is not connected to this central server now; try again in a moment",
                by.key
            )))
        }
        Err(error) => Detected {
            answers: false,
            error: Some(error.to_string()),
            description: None,
            name: None,
            object_id: None,
            groups: Vec::new(),
            template: None,
            probe: by.key.clone(),
        },
    };
    Ok(Json(result))
}

/// The device `id` locked, after checking the user's right on its site.
async fn device_site(
    tx: &mut PgConnection,
    user: &CurrentUser,
    id: Uuid,
    modify: bool,
) -> Result<Uuid, ApiError> {
    let row: Option<(Uuid, String)> =
        sqlx::query_as("SELECT site_id, kind FROM servers WHERE id = $1 AND revoked_at IS NULL")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    authorize(user, row.as_ref().map(|r| r.0), modify)?;
    match row {
        Some((site, kind)) if kind == "device" => Ok(site),
        _ => Err(ApiError::NotFound),
    }
}

/// `GET /api/devices/{id}/snmp`: its credentials without secrets; 404
/// when it has none.
pub async fn snmp(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<snmp::View>, ApiError> {
    let id = parse_id(&id)?;
    let mut conn = state.pool.acquire().await?;
    device_site(&mut conn, &user, id, false).await?;
    Ok(Json(
        snmp::view(&mut conn, id).await?.ok_or(ApiError::NotFound)?,
    ))
}

/// `PUT /api/devices/{id}/snmp`: sets its credentials; a secret left out
/// keeps the stored one when the version stays.
pub async fn set_snmp(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<snmp::CredentialsRequest>, JsonRejection>,
) -> Result<Json<snmp::View>, ApiError> {
    let id = parse_id(&id)?;
    let request = body(request)?;
    let mut tx = state.pool.begin().await?;
    device_site(&mut tx, &user, id, true).await?;
    sqlx::query("SELECT 1 FROM servers WHERE id = $1 FOR UPDATE")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let before = snmp::load(&mut tx, id).await?.and_then(Result::ok);
    let credentials = snmp::check(&request, before.as_ref()).map_err(ApiError::Invalid)?;
    snmp::save(&mut tx, id, &credentials)
        .await
        .map_err(save_error)?;
    record(
        &mut tx,
        &user,
        "device.snmp_update",
        id,
        snmp::audit(&credentials),
    )
    .await?;
    let view = snmp::view(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(Json(view))
}

/// `DELETE /api/devices/{id}/snmp`: 204; its SNMP checks stop.
pub async fn delete_snmp(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    let mut tx = state.pool.begin().await?;
    device_site(&mut tx, &user, id, true).await?;
    let deleted = sqlx::query("DELETE FROM snmp_credentials WHERE server_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if deleted == 0 {
        return Err(ApiError::NotFound);
    }
    record(&mut tx, &user, "device.snmp_delete", id, json!({})).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

fn vmware_save_error(error: vmware::SaveError) -> ApiError {
    match error {
        vmware::SaveError::Database(error) => ApiError::Database(error),
        vmware::SaveError::Seal(error) => ApiError::Internal(error),
    }
}

/// Adds a device's `vmware` check run by `by`, with its alerts.
pub async fn add_vmware(
    db: &mut PgConnection,
    device: Uuid,
    by: &RunBy,
) -> Result<(), sqlx::Error> {
    vmware::add_check(
        db,
        device,
        monitorflow_vmware::KIND,
        vmware::CHECK,
        by,
        &vmware::VMWARE_THRESHOLDS,
    )
    .await
    .map(|_| ())
}

/// Body of `POST /api/devices/vmware/test`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct VmwareTestRequest {
    pub site_id: Uuid,
    /// The vCenter's or ESXi host's IP address or DNS name.
    pub address: String,
    /// `central` or a relay of the site; absent: the site's default.
    #[serde(default)]
    pub probe: Option<String>,
    /// What to log in with; without a user and password, only what
    /// answers and its certificate are shown.
    #[serde(default)]
    pub vmware: vmware::CredentialsRequest,
}

/// The certificate a test met.
#[derive(Debug, Serialize, PartialEq, utoipa::ToSchema)]
pub struct TestedCertificate {
    /// `AB:CD:…`, to compare with the one the vCenter shows and send back
    /// as `fingerprint`.
    pub fingerprint: String,
    /// Whether it is trusted: by the probe's roots, or by the fingerprint
    /// given.
    pub trusted: bool,
    /// Whether the probe's roots trust it without a fingerprint.
    pub by_roots: bool,
    pub subject: String,
    /// When it expires; `null` when it does not read.
    pub not_after: Option<chrono::DateTime<chrono::Utc>>,
}

/// What a vCenter or ESXi host answered.
#[derive(Debug, Serialize, PartialEq, utoipa::ToSchema)]
pub struct VmwareTested {
    /// Whether the vSphere API answered at the address.
    pub answers: bool,
    /// Why not, or why the login failed, in words; `null` otherwise.
    pub error: Option<String>,
    /// Like `VMware vCenter Server 8.0.2 build-22385739`.
    pub product: Option<String>,
    pub version: Option<String>,
    /// `VirtualCenter` or `HostAgent` (a standalone ESXi host).
    pub api_type: Option<String>,
    pub certificate: Option<TestedCertificate>,
    /// Whether the login worked; `null` when it was not tried: no
    /// credentials, or a certificate not trusted yet, as the password is
    /// never sent then.
    pub login: Option<bool>,
    /// Objects the account sees, by kind (`vm`, `host`, `cluster`,
    /// `datastore`), after a login.
    #[schema(value_type = Object)]
    pub objects: std::collections::BTreeMap<String, u64>,
    /// `central` or the relay that asked.
    pub probe: String,
}

/// What the samples of a `vmware` test say.
pub fn vmware_tested(
    samples: &[monitorflow_common::proto::v1::Sample],
    probe: &str,
) -> VmwareTested {
    let find = |name: &str| samples.iter().find(|s| s.sensor.ends_with(name));
    let label = |s: Option<&monitorflow_common::proto::v1::Sample>, name: &str| {
        s.and_then(|s| s.labels.get(name))
            .filter(|v| !v.is_empty())
            .cloned()
    };
    let answers = find("_answers");
    let certificate = find("_certificate_trusted");
    let login = find("_login");
    let up = answers.is_some_and(|s| s.value >= 1.0);
    VmwareTested {
        answers: up,
        error: if up {
            label(login.filter(|l| l.value < 1.0), "error")
        } else {
            label(answers, "error")
        },
        product: label(answers.filter(|_| up), "product"),
        version: label(answers.filter(|_| up), "version"),
        api_type: label(answers.filter(|_| up), "api_type"),
        certificate: certificate.map(|c| TestedCertificate {
            fingerprint: c.labels.get("fingerprint").cloned().unwrap_or_default(),
            trusted: c.value >= 1.0,
            by_roots: c.labels.get("by_roots").is_some_and(|v| v == "true"),
            subject: c.labels.get("subject").cloned().unwrap_or_default(),
            not_after: c
                .labels
                .get("not_after")
                .and_then(|t| t.parse::<i64>().ok())
                .filter(|t| *t > 0)
                .and_then(|t| chrono::DateTime::from_timestamp(t, 0)),
        }),
        login: login.map(|l| l.value >= 1.0),
        objects: samples
            .iter()
            .filter(|s| s.sensor.ends_with("_objects"))
            .map(|s| (s.instance.clone(), s.value.max(0.0) as u64))
            .collect(),
        probe: probe.to_owned(),
    }
}

/// `POST /api/devices/vmware/test`: connects once from the probe that
/// would check the address, shows what answers and its certificate, and
/// logs in only when the certificate is trusted. Operators of the site.
pub async fn test_vmware(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<VmwareTestRequest>, JsonRejection>,
) -> Result<Json<VmwareTested>, ApiError> {
    let request = body(request)?;
    let exists: Option<Uuid> = sqlx::query_scalar("SELECT id FROM sites WHERE id = $1")
        .bind(request.site_id)
        .fetch_optional(&state.pool)
        .await?;
    let site = authorize(&user, exists, true)?;
    let address = check_address(&request.address)?;
    let mut params = std::collections::BTreeMap::new();
    let given = &request.vmware;
    if let Some(port) = given.port {
        params.insert("port".to_string(), port.to_string());
    }
    if let (Some(user), Some(password)) = (&given.user, &given.password) {
        params.insert("user".to_string(), user.trim().to_owned());
        params.insert("password".to_string(), password.clone());
    }
    if let Some(print) = given
        .fingerprint
        .as_deref()
        .filter(|f| !f.trim().is_empty())
    {
        params.insert(
            "fingerprint".to_string(),
            vmware::fingerprint(print).map_err(ApiError::Invalid)?,
        );
    }
    params.insert("host".into(), address);
    params.insert("what".into(), "test".into());
    monitorflow_vmware::parse(&params).map_err(ApiError::Invalid)?;
    let by = probe_for(&state, site, request.probe.as_deref()).await?;
    let mut config =
        monitorflow_sensors::SensorConfig::new("vmware", std::time::Duration::from_secs(60));
    config.timeout = Some(std::time::Duration::from_secs(25));
    config.params = params;
    let check = monitorflow_probe::Check {
        device: "test".into(),
        kind: monitorflow_vmware::KIND.into(),
        config,
    };
    let samples = run_once(&state, &by, check).await?;
    Ok(Json(vmware_tested(&samples, &by.key)))
}

/// `GET /api/devices/{id}/vmware`: its credentials without the password;
/// 404 when it has none.
pub async fn vmware_view(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<vmware::View>, ApiError> {
    let id = parse_id(&id)?;
    let mut conn = state.pool.acquire().await?;
    device_site(&mut conn, &user, id, false).await?;
    Ok(Json(
        vmware::view(&mut conn, id)
            .await?
            .ok_or(ApiError::NotFound)?,
    ))
}

/// `PUT /api/devices/{id}/vmware`: sets its credentials, what is left out
/// kept, and gives the device its `vmware` check if it has none.
pub async fn set_vmware(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<vmware::CredentialsRequest>, JsonRejection>,
) -> Result<Json<vmware::View>, ApiError> {
    let id = parse_id(&id)?;
    let request = body(request)?;
    let mut tx = state.pool.begin().await?;
    device_site(&mut tx, &user, id, true).await?;
    sqlx::query("SELECT 1 FROM servers WHERE id = $1 FOR UPDATE")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let before = vmware::load(&mut tx, id).await?.and_then(Result::ok);
    let credentials = vmware::check(&request, before.as_ref()).map_err(ApiError::Invalid)?;
    vmware::save(&mut tx, id, &credentials)
        .await
        .map_err(vmware_save_error)?;
    let has_check: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM sensor_configs WHERE server_id = $1 AND kind = 'vmware')",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    if !has_check {
        let probe =
            super::sensors::run_by(&mut tx, super::sensors::Owner::Server(id), None).await?;
        add_vmware(&mut tx, id, &probe).await?;
    }
    record(
        &mut tx,
        &user,
        "device.vmware_update",
        id,
        json!({"credentials": vmware::audit(&credentials), "check_added": !has_check}),
    )
    .await?;
    let view = vmware::view(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(Json(view))
}

/// `DELETE /api/devices/{id}/vmware`: 204; its `vmware` checks stop.
pub async fn delete_vmware(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    let mut tx = state.pool.begin().await?;
    device_site(&mut tx, &user, id, true).await?;
    let deleted = sqlx::query("DELETE FROM vmware_credentials WHERE server_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if deleted == 0 {
        return Err(ApiError::NotFound);
    }
    record(&mut tx, &user, "device.vmware_delete", id, json!({})).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/sites/{id}/probes`: where the site's network checks can run
/// from.
pub async fn probes(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<ProbeList>, ApiError> {
    let id = parse_id(&id)?;
    let exists: Option<Uuid> = sqlx::query_scalar("SELECT id FROM sites WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?;
    let site = authorize(&user, exists, false)?;
    let mut conn = state.pool.acquire().await?;
    let default = default_probe(&mut *conn, site).await?;
    let relays: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, peer_id, name FROM relays WHERE site_id = $1 AND revoked_at IS NULL
         ORDER BY name, peer_id",
    )
    .bind(site)
    .fetch_all(&mut *conn)
    .await?;
    let mut probes = vec![Probe {
        probe: "central".into(),
        relay_id: None,
        key: monitorflow_probe::CENTRAL.into(),
        name: None,
        online: true,
        default: default.relay.is_none(),
    }];
    probes.extend(relays.into_iter().map(|(relay, peer, name)| Probe {
        probe: "relay".into(),
        online: state.registry.live(PeerRole::Relay, &peer).is_some(),
        default: default.relay == Some(relay),
        relay_id: Some(relay),
        key: peer,
        name: Some(name),
    }));
    Ok(Json(ProbeList { probes }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_addresses_are_checked() {
        assert_eq!(check_name("  Core switch ").unwrap(), "Core switch");
        assert!(check_name("").is_err());
        assert!(check_name("a\nb").is_err());
        assert!(check_name(&"x".repeat(MAX_NAME + 1)).is_err());
        assert_eq!(check_address("192.0.2.5").unwrap(), "192.0.2.5");
        assert_eq!(check_address("[2001:db8::1]").unwrap(), "2001:db8::1");
        assert_eq!(check_address(" nas.lan ").unwrap(), "nas.lan");
        for bad in ["", "a b", "http://x", "x/y", "a:1"] {
            assert!(check_address(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn requests_take_defaults_and_refuse_unknown_fields() {
        let site = Uuid::nil();
        let request: DeviceRequest =
            serde_json::from_value(json!({"site_id": site, "name": "nas", "address": "nas.lan"}))
                .unwrap();
        assert!(request.ping && request.probe.is_none());
        assert!(serde_json::from_value::<DeviceRequest>(
            json!({"site_id": site, "name": "nas", "address": "x", "os": "y"})
        )
        .is_err());
    }
}
