//! What the Flows tab and `GET /api/devices/{id}/flows` show: over a
//! period, the largest keys of each dimension and a chart of the largest
//! ones of one dimension over time. Without an address, from the stored
//! tops, whose totals are exact; with one, from the conversations it is
//! part of.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, TimeZone, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

/// Dimensions in the order they are shown.
pub const DIMENSIONS: [&str; 6] = ["src", "dst", "app", "protocol", "in", "out"];
/// Keys of the chart; the rest is drawn as one band.
pub const CHART_KEYS: usize = 5;
/// Rows read for an address over its period, at most.
const MAX_ROWS: i64 = 200_000;
/// Steps of the chart, in seconds: the first one giving at most
/// [`POINTS`] points.
const STEPS: [i64; 12] = [
    60, 120, 300, 600, 900, 1800, 3600, 7200, 10_800, 21_600, 43_200, 86_400,
];
const POINTS: i64 = 120;

/// One key of a dimension over the period.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct KeyView {
    /// An address, an application (`tcp/443`), a protocol (`tcp`), or an
    /// ifIndex of the exporter for `in` and `out` (`0`: not said).
    pub key: String,
    /// The application's or the interface's name, when known.
    pub label: Option<String>,
    pub bytes: i64,
    pub packets: i64,
    /// Average over the period.
    pub bits_per_second: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct DimensionView {
    /// `src`, `dst`, `app`, `protocol`, `in` or `out`.
    pub dimension: String,
    /// Largest first.
    pub keys: Vec<KeyView>,
    /// Bytes of the period in no key above.
    pub other_bytes: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct ChartKey {
    pub key: String,
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct ChartPoint {
    /// Start of the step.
    pub at: DateTime<Utc>,
    /// Bits per second of each key of the chart, in its order.
    pub values: Vec<f64>,
    /// Bits per second of the rest.
    pub other: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct ChartView {
    /// The dimension drawn.
    pub by: String,
    pub keys: Vec<ChartKey>,
    pub points: Vec<ChartPoint>,
}

#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct FlowView {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    /// Seconds of one point of the chart.
    pub step_s: i64,
    /// The address the views are filtered on.
    pub address: Option<String>,
    pub bytes: i64,
    pub packets: i64,
    pub bits_per_second: f64,
    pub dimensions: Vec<DimensionView>,
    pub chart: ChartView,
}

/// What to show.
#[derive(Debug, Clone)]
pub struct Request {
    pub device: Uuid,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub address: Option<String>,
    /// One of [`DIMENSIONS`].
    pub by: String,
    /// Keys per dimension.
    pub limit: usize,
}

/// The chart's step for `from..to`; at least an hour where minutes were
/// already summed into hours.
pub fn step(from: DateTime<Utc>, to: DateTime<Utc>, now: DateTime<Utc>) -> i64 {
    let span = (to - from).num_seconds().max(60);
    let mut step = STEPS
        .iter()
        .copied()
        .find(|s| span / s <= POINTS)
        .unwrap_or(86_400);
    let summed = now - chrono::Duration::from_std(super::MINUTES_KEPT).unwrap_or_default();
    if from < summed + chrono::Duration::hours(1) {
        step = step.max(3600);
    }
    step
}

/// Names a key: an application's name, or an interface's.
fn label(dimension: &str, key: &str, interfaces: &HashMap<u32, String>) -> Option<String> {
    match dimension {
        "app" => monitorflow_flow::apps::app_name(key).map(str::to_owned),
        "in" | "out" => key
            .parse::<u32>()
            .ok()
            .and_then(|i| interfaces.get(&i).cloned()),
        _ => None,
    }
}

/// Counts of one key.
type Counts = BTreeMap<(String, String), (i64, i64)>;

fn bucket(at: DateTime<Utc>, step: i64) -> i64 {
    let t = at.timestamp();
    t - t.rem_euclid(step)
}

/// The view of `request`, interfaces named from `interfaces` (ifIndex to
/// name).
pub async fn load(
    pool: &PgPool,
    request: &Request,
    interfaces: &HashMap<u32, String>,
    now: DateTime<Utc>,
) -> Result<FlowView, sqlx::Error> {
    let step = step(request.from, request.to, now);
    // Per dimension and key; and per bucket, the chart's dimension's keys
    // and the total.
    let (total, counts, by_bucket) = match &request.address {
        None => unfiltered(pool, request, step).await?,
        Some(address) => filtered(pool, request, address, step).await?,
    };
    let seconds = (request.to - request.from).num_seconds().max(1) as f64;
    let rate = |bytes: i64| bytes as f64 * 8.0 / seconds;
    let mut dimensions = Vec::new();
    let mut chart_keys = Vec::new();
    for dimension in DIMENSIONS {
        let mut keys: Vec<KeyView> = counts
            .iter()
            .filter(|((d, _), _)| d == dimension)
            .map(|((_, key), (bytes, packets))| KeyView {
                key: key.clone(),
                label: label(dimension, key, interfaces),
                bytes: *bytes,
                packets: *packets,
                bits_per_second: rate(*bytes),
            })
            .collect();
        keys.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.key.cmp(&b.key)));
        if dimension == request.by {
            chart_keys = keys
                .iter()
                .take(CHART_KEYS)
                .map(|k| ChartKey {
                    key: k.key.clone(),
                    label: k.label.clone(),
                })
                .collect();
        }
        keys.truncate(request.limit);
        let shown: i64 = keys.iter().map(|k| k.bytes).sum();
        dimensions.push(DimensionView {
            dimension: dimension.into(),
            keys,
            other_bytes: (total.0 - shown).max(0),
        });
    }
    let mut points = Vec::new();
    let mut at = bucket(request.from, step);
    let per_step = |bytes: i64| bytes as f64 * 8.0 / step as f64;
    while at < request.to.timestamp() {
        let empty = HashMap::new();
        let here = by_bucket.get(&at).unwrap_or(&empty);
        let values: Vec<f64> = chart_keys
            .iter()
            .map(|k| per_step(here.get(&k.key).copied().unwrap_or(0)))
            .collect();
        let keyed: i64 = chart_keys
            .iter()
            .map(|k| here.get(&k.key).copied().unwrap_or(0))
            .sum();
        let all = here.get("").copied().unwrap_or(0);
        points.push(ChartPoint {
            at: Utc.timestamp_opt(at, 0).single().unwrap_or(request.from),
            values,
            other: per_step((all - keyed).max(0)),
        });
        at += step;
    }
    Ok(FlowView {
        from: request.from,
        to: request.to,
        step_s: step,
        address: request.address.clone(),
        bytes: total.0,
        packets: total.1,
        bits_per_second: rate(total.0),
        dimensions,
        chart: ChartView {
            by: request.by.clone(),
            keys: chart_keys,
            points,
        },
    })
}

/// Bucket to key to bytes; the key `""` is the total.
type Buckets = HashMap<i64, HashMap<String, i64>>;

async fn unfiltered(
    pool: &PgPool,
    request: &Request,
    step: i64,
) -> Result<((i64, i64), Counts, Buckets), sqlx::Error> {
    let rows: Vec<(String, String, i64, i64)> = sqlx::query_as(
        "SELECT dimension, key, sum(bytes)::bigint, sum(packets)::bigint FROM flow_tops
         WHERE server_id = $1 AND at >= $2 AND at < $3
         GROUP BY dimension, key",
    )
    .bind(request.device)
    .bind(request.from)
    .bind(request.to)
    .fetch_all(pool)
    .await?;
    let mut total = (0, 0);
    let mut counts = Counts::new();
    for (dimension, key, bytes, packets) in rows {
        if dimension == "total" {
            total = (bytes, packets);
        } else {
            counts.insert((dimension, key), (bytes, packets));
        }
    }
    let mut chart: Vec<(&String, i64)> = counts
        .iter()
        .filter(|((d, _), _)| *d == request.by)
        .map(|((_, k), (b, _))| (k, *b))
        .collect();
    chart.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let keys: Vec<String> = chart
        .iter()
        .take(CHART_KEYS)
        .map(|(k, _)| (*k).clone())
        .collect();
    let rows: Vec<(DateTime<Utc>, String, String, i64)> = sqlx::query_as(
        "SELECT at, dimension, key, bytes FROM flow_tops
         WHERE server_id = $1 AND at >= $2 AND at < $3
           AND (dimension = 'total' OR (dimension = $4 AND key = ANY($5)))",
    )
    .bind(request.device)
    .bind(request.from)
    .bind(request.to)
    .bind(&request.by)
    .bind(&keys)
    .fetch_all(pool)
    .await?;
    let mut buckets = Buckets::new();
    for (at, dimension, key, bytes) in rows {
        let key = if dimension == "total" {
            String::new()
        } else {
            key
        };
        *buckets
            .entry(bucket(at, step))
            .or_default()
            .entry(key)
            .or_default() += bytes;
    }
    Ok((total, counts, buckets))
}

/// The key of a conversation in `dimension`.
fn conversation_key(
    dimension: &str,
    src: &str,
    dst: &str,
    app: &str,
    input: i64,
    output: i64,
) -> String {
    match dimension {
        "src" => src.to_owned(),
        "dst" => dst.to_owned(),
        "app" => app.to_owned(),
        "protocol" => app.split('/').next().unwrap_or(app).to_owned(),
        "in" => input.to_string(),
        _ => output.to_string(),
    }
}

async fn filtered(
    pool: &PgPool,
    request: &Request,
    address: &str,
    step: i64,
) -> Result<((i64, i64), Counts, Buckets), sqlx::Error> {
    #[allow(clippy::type_complexity)]
    let rows: Vec<(DateTime<Utc>, String, String, String, i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT at, src, dst, app, input, output, bytes, packets FROM flow_conversations
         WHERE server_id = $1 AND at >= $2 AND at < $3 AND (src = $4 OR dst = $4)
         LIMIT $5",
    )
    .bind(request.device)
    .bind(request.from)
    .bind(request.to)
    .bind(address)
    .bind(MAX_ROWS)
    .fetch_all(pool)
    .await?;
    let mut total = (0i64, 0i64);
    let mut counts = Counts::new();
    let mut per_key: Vec<(i64, String, i64)> = Vec::with_capacity(rows.len());
    for (at, src, dst, app, input, output, bytes, packets) in rows {
        total.0 = total.0.saturating_add(bytes);
        total.1 = total.1.saturating_add(packets);
        for dimension in DIMENSIONS {
            let key = conversation_key(dimension, &src, &dst, &app, input, output);
            let entry = counts.entry((dimension.to_owned(), key)).or_default();
            entry.0 = entry.0.saturating_add(bytes);
            entry.1 = entry.1.saturating_add(packets);
        }
        per_key.push((
            bucket(at, step),
            conversation_key(&request.by, &src, &dst, &app, input, output),
            bytes,
        ));
    }
    let mut buckets = Buckets::new();
    for (at, key, bytes) in per_key {
        let here = buckets.entry(at).or_default();
        *here.entry(String::new()).or_default() += bytes;
        *here.entry(key).or_default() += bytes;
    }
    Ok((total, counts, buckets))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_fit_the_period_and_the_stored_resolution() {
        let now = Utc.timestamp_opt(1_800_000_000, 0).unwrap();
        let hours = |h: i64| now - chrono::Duration::hours(h);
        assert_eq!(step(hours(1), now, now), 60);
        assert_eq!(step(hours(6), now, now), 300);
        assert_eq!(step(hours(24), now, now), 900);
        assert_eq!(step(hours(24 * 7), now, now), 7200);
        assert_eq!(step(hours(48), hours(47), now), 3600, "already hours");
    }

    #[test]
    fn keys_of_a_conversation() {
        let key = |d| conversation_key(d, "192.0.2.1", "198.51.100.1", "tcp/443", 3, 0);
        assert_eq!(key("protocol"), "tcp");
        assert_eq!(key("in"), "3");
        assert_eq!(key("out"), "0");
        assert_eq!(conversation_key("protocol", "a", "b", "icmp", 0, 0), "icmp");
        let names = HashMap::from([(3, "Gi0/3".to_string())]);
        assert_eq!(label("in", "3", &names).as_deref(), Some("Gi0/3"));
        assert_eq!(label("app", "tcp/443", &names).as_deref(), Some("HTTPS"));
        assert_eq!(label("src", "192.0.2.1", &names), None);
    }
}
