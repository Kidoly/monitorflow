//! Raw reads for the context: PostgreSQL rows, VictoriaMetrics series and
//! VictoriaLogs lines, each bounded by the query itself. Nothing here is
//! redacted yet; [`super::gather`] does that before building sections.

use std::collections::BTreeMap;

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use super::{Limits, Owner, Trigger};
use crate::api::metrics::{build_query, quote, Rollup, Target};
use crate::vm_query::{QueryError, VmQuery};

/// The server row with its site, customer and relay.
#[derive(Debug, Clone, sqlx::FromRow)]
pub(super) struct ServerRow {
    pub id: Uuid,
    pub peer_id: String,
    pub hostname: String,
    pub os_family: String,
    pub os_version: Option<String>,
    pub agent_version: Option<String>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub reboot_required: Option<bool>,
    pub packages_reported_at: Option<DateTime<Utc>>,
    pub site_id: Uuid,
    pub site: String,
    pub customer_id: Uuid,
    pub customer: String,
    pub relay: Option<String>,
    pub package_count: i64,
}

pub(super) async fn server(pool: &PgPool, id: Uuid) -> Result<Option<ServerRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT s.id, s.peer_id, s.hostname, s.os_family, s.os_version, s.agent_version,
                s.last_seen_at, s.revoked_at, s.reboot_required, s.packages_reported_at,
                si.id AS site_id, si.name AS site, c.id AS customer_id, c.name AS customer,
                r.name AS relay,
                (SELECT count(*) FROM server_packages p WHERE p.server_id = s.id) AS package_count
         FROM servers s
         JOIN sites si ON si.id = s.site_id
         JOIN customers c ON c.id = si.customer_id
         LEFT JOIN relays r ON r.id = s.relay_id
         WHERE s.id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Upgradable packages by name, and how many there are in all.
#[derive(Debug, Clone, sqlx::FromRow)]
pub(super) struct PackageRow {
    pub name: String,
    pub arch: String,
    pub version: String,
    pub upgradable_version: String,
    pub total: i64,
}

pub(super) async fn upgradable(
    pool: &PgPool,
    server: Uuid,
    limit: usize,
) -> Result<Vec<PackageRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT name, arch, version, upgradable_version, count(*) OVER () AS total
         FROM server_packages
         WHERE server_id = $1 AND upgradable_version IS NOT NULL
         ORDER BY name, arch
         LIMIT $2",
    )
    .bind(server)
    .bind(i64::try_from(limit).unwrap_or(i64::MAX))
    .fetch_all(pool)
    .await
}

/// Commands of the server issued since `since`, newest first. The server
/// ID is selected so the caller can check it.
#[derive(Debug, Clone, sqlx::FromRow)]
pub(super) struct CommandRow {
    pub id: Uuid,
    pub server_id: Uuid,
    pub kind: String,
    pub payload: Vec<u8>,
    pub state: String,
    pub issued_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub exit_code: Option<i32>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub error: Option<String>,
    pub campaign_id: Option<Uuid>,
    pub total: i64,
}

pub(super) async fn commands(
    pool: &PgPool,
    server: Uuid,
    since: DateTime<Utc>,
    limit: usize,
) -> Result<Vec<CommandRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, server_id, kind, payload, state, issued_at, finished_at, exit_code,
                stdout, stderr, error, campaign_id, count(*) OVER () AS total
         FROM commands
         WHERE server_id = $1 AND issued_at >= $2
         ORDER BY issued_at DESC, id
         LIMIT $3",
    )
    .bind(server)
    .bind(since)
    .bind(i64::try_from(limit).unwrap_or(i64::MAX))
    .fetch_all(pool)
    .await
}

/// Campaigns targeting the server that were active since `since`, newest
/// first. Not filtered by customer: the caller asserts it.
#[derive(Debug, Clone, sqlx::FromRow)]
pub(super) struct CampaignRow {
    pub id: Uuid,
    pub customer_id: Uuid,
    pub name: String,
    pub state: String,
    pub packages: Vec<String>,
    pub security_only: bool,
    pub allow_reboot: bool,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    pub wave: i32,
    pub total: i64,
}

pub(super) async fn campaigns(
    pool: &PgPool,
    server: Uuid,
    since: DateTime<Utc>,
    limit: usize,
) -> Result<Vec<CampaignRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT c.id, c.customer_id, c.name, c.state, c.packages, c.security_only,
                c.allow_reboot, c.created_at, c.started_at, c.finished_at, c.error, t.wave,
                count(*) OVER () AS total
         FROM campaign_targets t
         JOIN campaigns c ON c.id = t.campaign_id
         WHERE t.server_id = $1 AND (c.finished_at IS NULL OR c.finished_at >= $2)
         ORDER BY c.created_at DESC, c.id
         LIMIT $3",
    )
    .bind(server)
    .bind(since)
    .bind(i64::try_from(limit).unwrap_or(i64::MAX))
    .fetch_all(pool)
    .await
}

/// One series of the breached metric.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct RawSeries {
    pub labels: BTreeMap<String, String>,
    /// `(unix seconds, value)`, finite values only, oldest first.
    pub points: Vec<(i64, f64)>,
}

/// The breached series over `[start, end]` with `step`-second points,
/// each the worst value of its step. Same filters as the metric proxy
/// (ADR-0018): `peer` and `site` matchers and `extra_label`s.
pub(super) async fn history(
    vm: &VmQuery,
    owner: &Owner,
    trigger: &Trigger,
    (start, end, step): (i64, i64, i64),
) -> Result<Vec<RawSeries>, QueryError> {
    let target = Target {
        peer: owner.peer.clone(),
        site: owner.site.clone(),
    };
    let above = trigger.threshold["comparison"]
        .as_str()
        .is_none_or(|c| c.starts_with('>'));
    let rollup = if above { Rollup::Max } else { Rollup::Min };
    let promql = build_query(
        &trigger.metric,
        &target,
        Some(&trigger.instance),
        Some((rollup, step)),
        None,
    );
    let params = [
        ("query", promql),
        ("start", start.to_string()),
        ("end", end.to_string()),
        ("step", step.to_string()),
        ("extra_label", format!("peer={}", target.peer)),
        ("extra_label", format!("site={}", target.site)),
    ];
    let data = vm.get("/api/v1/query_range", &params).await?;
    let malformed = || QueryError::Malformed("unexpected query result".into());
    let Value::Array(results) = &data["result"] else {
        return Err(malformed());
    };
    results
        .iter()
        .map(|r| {
            let labels: BTreeMap<String, String> =
                serde_json::from_value(r["metric"].clone()).map_err(|_| malformed())?;
            let Value::Array(values) = &r["values"] else {
                return Err(malformed());
            };
            let points = values
                .iter()
                .filter_map(|p| {
                    let time = p.get(0)?.as_f64()?;
                    let value = p.get(1)?.as_str()?.parse::<f64>().ok()?;
                    value.is_finite().then_some((time as i64, value))
                })
                .collect();
            Ok(RawSeries { labels, points })
        })
        .collect()
}

/// One VictoriaLogs record.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct RawLog {
    pub time: DateTime<Utc>,
    pub msg: String,
    /// Every field but `_msg`, `_time` and the other `_` fields.
    pub fields: BTreeMap<String, String>,
}

/// The newest `limit` records of the server's stream in `[start, end)`,
/// oldest first. The stream filter names both `peer` and `site`, the
/// stream fields of ADR-0013.
pub(super) async fn logs(
    vl: &VmQuery,
    owner: &Owner,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    limit: usize,
) -> Result<Vec<RawLog>, QueryError> {
    let query = format!(
        "{{peer={},site={}}} | sort by (_time desc) | limit {limit}",
        quote(&owner.peer),
        quote(&owner.site)
    );
    let params = [
        ("query", query),
        ("start", start.to_rfc3339_opts(SecondsFormat::Secs, true)),
        ("end", end.to_rfc3339_opts(SecondsFormat::Secs, true)),
        ("limit", limit.to_string()),
    ];
    let body = vl.get_raw("/select/logsql/query", &params).await?;
    let text = String::from_utf8_lossy(&body);
    let mut records = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let object: BTreeMap<String, Value> = serde_json::from_str(line)
                .map_err(|e| QueryError::Malformed(format!("log line: {e}")))?;
            let time = object
                .get("_time")
                .and_then(Value::as_str)
                .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                .ok_or_else(|| QueryError::Malformed("log line without `_time`".into()))?
                .to_utc();
            let as_text = |v: &Value| match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let msg = object.get("_msg").map(as_text).unwrap_or_default();
            let fields = object
                .iter()
                .filter(|(k, _)| !k.starts_with('_'))
                .map(|(k, v)| (k.clone(), as_text(v)))
                .collect();
            Ok(RawLog { time, msg, fields })
        })
        .collect::<Result<Vec<_>, QueryError>>()?;
    records.sort_by(|a, b| (a.time, &a.msg, &a.fields).cmp(&(b.time, &b.msg, &b.fields)));
    Ok(records)
}

/// `(start, end, step)` in Unix seconds for `limits.history_points` points
/// ending at `now`.
pub(super) fn history_range(limits: &Limits, now: DateTime<Utc>) -> (i64, i64, i64) {
    let end = now.timestamp();
    let window = limits.history_window.num_seconds().max(1);
    let points = i64::try_from(limits.history_points.max(1)).unwrap_or(i64::MAX);
    let step = ((window + points - 1) / points).max(1);
    (end - window, end, step)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_steps_cover_the_window_in_the_point_budget() {
        let limits = Limits::default();
        let now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let (start, end, step) = history_range(&limits, now);
        assert_eq!(end, 1_800_000_000);
        assert_eq!(end - start, limits.history_window.num_seconds());
        assert!((end - start) / step <= i64::try_from(limits.history_points).unwrap());
    }
}
