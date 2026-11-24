//! Traffic flows of a device (ADR-0184): which probe receives them, and
//! what they say over a period.
//!
//! | Method | Path | Right | Notes |
//! | --- | --- | --- | --- |
//! | GET | `/api/devices/{id}/flow-exporter` | read | [`ExporterView`]; 404 when its flows are not received |
//! | PUT | `/api/devices/{id}/flow-exporter` | modify | body [`ExporterRequest`]; audited |
//! | DELETE | `/api/devices/{id}/flow-exporter` | modify | 204; stored flows stay until they expire; audited |
//! | GET | `/api/devices/{id}/flows` | read | [`FlowParams`]; [`query::FlowView`] |

use std::collections::HashMap;
use std::net::IpAddr;

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgConnection;
use uuid::Uuid;

use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::devices::{device_site, probe_for};
use super::metrics::{self, backend_error, Target};
use super::sensors::{body, parse_id};
use super::{ApiError, ApiState};
use crate::flows::query::{self, FlowView, DIMENSIONS};

/// Longest period one request reads: what is kept.
const MAX_PERIOD: Duration = Duration::days(31);
/// Keys per dimension, by default and at most.
const DEFAULT_LIMIT: usize = 10;
const MAX_LIMIT: usize = 50;

/// Who receives a device's flows.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ExporterView {
    /// `central`, or the ID of the relay of its site that receives them.
    pub probe: String,
    /// The relay's name; absent for the central server.
    pub relay_name: Option<String>,
    /// Where its flows come from when not the device's address.
    pub source_address: Option<String>,
    /// The address flows are taken from: `source_address`, else the
    /// device's.
    pub address: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The start of the newest window received; absent before the first.
    pub last_flow_at: Option<DateTime<Utc>>,
}

/// Receive a device's flows.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ExporterRequest {
    /// `central`, or the ID of a relay of the device's site; absent: the
    /// relay of the site added first, else the central server.
    #[serde(default)]
    pub probe: Option<String>,
    /// The IP address or DNS name its flows come from, when not its own
    /// address (a router exporting from a loopback, for instance).
    #[serde(default)]
    pub source_address: Option<String>,
}

/// What `GET /api/devices/{id}/flows` reads.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct FlowParams {
    /// Start, RFC 3339; default an hour before `to`.
    pub from: Option<DateTime<Utc>>,
    /// End, RFC 3339; default now. At most 31 days after `from`.
    pub to: Option<DateTime<Utc>>,
    /// Only the traffic this IP address sent or received.
    pub address: Option<String>,
    /// The dimension of the chart: `src` (default), `dst`, `app`,
    /// `protocol`, `in` or `out`.
    pub by: Option<String>,
    /// Keys per dimension, 1 to 50; default 10.
    pub limit: Option<usize>,
}

fn valid_source(address: &str) -> bool {
    !address.is_empty()
        && address.len() <= 253
        && (address.parse::<IpAddr>().is_ok()
            || address
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-'))
}

async fn load_view(db: &mut PgConnection, id: Uuid) -> Result<Option<ExporterView>, sqlx::Error> {
    #[allow(clippy::type_complexity)]
    let row: Option<(
        String,
        Option<Uuid>,
        Option<String>,
        Option<String>,
        Option<String>,
        DateTime<Utc>,
        DateTime<Utc>,
    )> = sqlx::query_as(
        "SELECT f.probe, f.probe_relay_id, r.name, f.source_address,
                coalesce(f.source_address, s.address), f.created_at, f.updated_at
         FROM flow_exporters f JOIN servers s ON s.id = f.server_id
         LEFT JOIN relays r ON r.id = f.probe_relay_id
         WHERE f.server_id = $1",
    )
    .bind(id)
    .fetch_optional(&mut *db)
    .await?;
    let Some((probe, relay, relay_name, source_address, address, created_at, updated_at)) = row
    else {
        return Ok(None);
    };
    let last_flow_at: Option<DateTime<Utc>> = sqlx::query_scalar(
        "SELECT max(at) FROM flow_tops WHERE server_id = $1 AND dimension = 'total'
           AND at > now() - interval '31 days'",
    )
    .bind(id)
    .fetch_one(&mut *db)
    .await?;
    Ok(Some(ExporterView {
        probe: match (probe.as_str(), relay) {
            ("relay", Some(relay)) => relay.to_string(),
            _ => monitorflow_probe::CENTRAL.into(),
        },
        relay_name,
        source_address,
        address,
        created_at,
        updated_at,
        last_flow_at,
    }))
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

/// `GET /api/devices/{id}/flow-exporter`.
pub async fn exporter(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<ExporterView>, ApiError> {
    let id = parse_id(&id)?;
    let mut conn = state.pool.acquire().await?;
    device_site(&mut conn, &user, id, false).await?;
    Ok(Json(
        load_view(&mut conn, id).await?.ok_or(ApiError::NotFound)?,
    ))
}

/// `PUT /api/devices/{id}/flow-exporter`: its probe receives its flows
/// from the next configuration it is sent, within seconds.
pub async fn set_exporter(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<ExporterRequest>, JsonRejection>,
) -> Result<Json<ExporterView>, ApiError> {
    let id = parse_id(&id)?;
    let request = body(request)?;
    let source = request
        .source_address
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    if source.as_deref().is_some_and(|s| !valid_source(s)) {
        return Err(ApiError::Invalid(
            "`source_address` must be an IP address or a DNS name".into(),
        ));
    }
    let site = {
        let mut conn = state.pool.acquire().await?;
        device_site(&mut conn, &user, id, true).await?
    };
    let by = probe_for(&state, site, request.probe.as_deref()).await?;
    let mut tx = state.pool.begin().await?;
    device_site(&mut tx, &user, id, true).await?;
    let address: Option<String> = sqlx::query_scalar("SELECT address FROM servers WHERE id = $1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    if source.is_none() && address.is_none() {
        return Err(ApiError::Invalid(
            "the device has no address: give `source_address`".into(),
        ));
    }
    sqlx::query(
        "INSERT INTO flow_exporters (server_id, probe, probe_relay_id, source_address)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (server_id) DO UPDATE
         SET probe = EXCLUDED.probe, probe_relay_id = EXCLUDED.probe_relay_id,
             source_address = EXCLUDED.source_address, updated_at = now()",
    )
    .bind(id)
    .bind(by.probe.unwrap_or("central"))
    .bind(by.relay)
    .bind(&source)
    .execute(&mut *tx)
    .await?;
    record(
        &mut tx,
        &user,
        "device.flow_exporter_update",
        id,
        json!({"probe": by.key, "source_address": source}),
    )
    .await?;
    let view = load_view(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(Json(view))
}

/// `DELETE /api/devices/{id}/flow-exporter`: 204; its probe stops taking
/// its flows. What was stored stays until it expires.
pub async fn delete_exporter(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    let mut tx = state.pool.begin().await?;
    device_site(&mut tx, &user, id, true).await?;
    let deleted = sqlx::query("DELETE FROM flow_exporters WHERE server_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if deleted == 0 {
        return Err(ApiError::NotFound);
    }
    record(&mut tx, &user, "device.flow_exporter_delete", id, json!({})).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The checked request of `params` at `now`.
pub fn request(
    device: Uuid,
    params: FlowParams,
    now: DateTime<Utc>,
) -> Result<query::Request, ApiError> {
    let to = params.to.unwrap_or(now).min(now + Duration::minutes(5));
    let from = params.from.unwrap_or(to - Duration::hours(1));
    if from >= to {
        return Err(ApiError::Invalid("`from` must be before `to`".into()));
    }
    if to - from > MAX_PERIOD {
        return Err(ApiError::Invalid("the period is at most 31 days".into()));
    }
    let by = params.by.unwrap_or_else(|| "src".into());
    if !DIMENSIONS.contains(&by.as_str()) {
        return Err(ApiError::Invalid(
            "`by` must be src, dst, app, protocol, in or out".into(),
        ));
    }
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(ApiError::Invalid("`limit` must be 1 to 50".into()));
    }
    let address = match params
        .address
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())
    {
        None => None,
        Some(a) => Some(
            a.parse::<IpAddr>()
                .map_err(|_| ApiError::Invalid("`address` must be an IP address".into()))?
                .to_canonical()
                .to_string(),
        ),
    };
    Ok(query::Request {
        device,
        from,
        to,
        address,
        by,
        limit,
    })
}

/// `GET /api/devices/{id}/flows`: the tops and the chart of a period.
pub async fn flows(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    params: Result<Query<FlowParams>, QueryRejection>,
) -> Result<Json<FlowView>, ApiError> {
    let id = parse_id(&id)?;
    let params = params
        .map(|Query(p)| p)
        .map_err(|rejection| ApiError::Invalid(rejection.body_text()))?;
    let now = Utc::now();
    let request = request(id, params, now)?;
    let (peer, site): (String, String) = {
        let mut conn = state.pool.acquire().await?;
        device_site(&mut conn, &user, id, false).await?;
        sqlx::query_as(
            "SELECT s.peer_id, si.name FROM servers s JOIN sites si ON si.id = s.site_id WHERE s.id = $1",
        )
        .bind(id)
        .fetch_one(&mut *conn)
        .await?
    };
    let interfaces = interface_names(&state, peer, site).await;
    Ok(Json(
        query::load(&state.pool, &request, &interfaces, now).await?,
    ))
}

/// The device's interfaces by ifIndex, from the `if_index` series of its
/// SNMP interfaces check (ADR-0149); none when it has none.
async fn interface_names(state: &ApiState, peer: String, site: String) -> HashMap<u32, String> {
    let Some(vm) = state.metrics.clone() else {
        return HashMap::new();
    };
    let target = Target { peer, site };
    let selector = format!(
        "{{__name__=~\"monitorflow_[a-z0-9_]*_if_index\",{}}}",
        target.matchers()
    );
    let params: Vec<(&str, String)> = [
        ("query", format!("last_over_time({selector}[1d])")),
        ("time", Utc::now().timestamp().to_string()),
    ]
    .into_iter()
    .chain(target.extra_labels())
    .collect();
    let series = match vm
        .get("/api/v1/query", &params)
        .await
        .map_err(backend_error)
        .and_then(metrics::series)
    {
        Ok(series) => series,
        Err(error) => {
            tracing::debug!(?error, "no interface names for traffic flows");
            return HashMap::new();
        }
    };
    let mut names = HashMap::new();
    for s in series {
        let (Some(index), Some(name)) =
            (s.points.last().and_then(|p| p.1), s.labels.get("instance"))
        else {
            continue;
        };
        if index >= 0.0 && index <= f64::from(u32::MAX) {
            names.insert(index as u32, name.clone());
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_checked() {
        let now = Utc::now();
        let id = Uuid::nil();
        let r = request(id, FlowParams::default(), now).unwrap();
        assert_eq!((r.to - r.from).num_hours(), 1);
        assert_eq!((r.by.as_str(), r.limit, r.address), ("src", 10, None));
        let r = request(
            id,
            FlowParams {
                address: Some("::ffff:192.0.2.1".into()),
                by: Some("app".into()),
                ..Default::default()
            },
            now,
        )
        .unwrap();
        assert_eq!(r.address.as_deref(), Some("192.0.2.1"));
        let bad = |p: FlowParams| request(id, p, now).is_err();
        assert!(bad(FlowParams {
            by: Some("port".into()),
            ..Default::default()
        }));
        assert!(bad(FlowParams {
            limit: Some(0),
            ..Default::default()
        }));
        assert!(bad(FlowParams {
            address: Some("host.example".into()),
            ..Default::default()
        }));
        assert!(bad(FlowParams {
            from: Some(now - Duration::days(40)),
            ..Default::default()
        }));
        assert!(bad(FlowParams {
            from: Some(now),
            to: Some(now - Duration::hours(1)),
            ..Default::default()
        }));
        assert!(valid_source("router-1.example.net"));
        assert!(valid_source("2001:db8::1"));
        assert!(!valid_source("bad name"));
    }
}
