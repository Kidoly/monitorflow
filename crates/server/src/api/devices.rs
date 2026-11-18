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
    let mut tx = state.pool.begin().await?;
    let (id, peer_id) = insert(&mut tx, site, &name, &address).await?;
    let by = if request.ping {
        let by = super::sensors::run_by(
            &mut tx,
            super::sensors::Owner::Server(id),
            request.probe.as_deref(),
        )
        .await?;
        add_ping(&mut tx, id, &by).await?;
        Some(by)
    } else {
        None
    };
    record(
        &mut tx,
        &user,
        "device.create",
        id,
        json!({
            "peer_id": peer_id, "site_id": site, "name": name, "address": address,
            "ping": by.as_ref().map(|b| json!({"probe": b.probe, "probe_relay_id": b.relay})),
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
