//! Traffic flows in PostgreSQL: exporters, and the reports probes send.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Duration as Span, TimeZone, Utc};
use monitorflow_common::proto::v1::FlowReport;
use monitorflow_flow::collector::Exporter;
use sqlx::PgPool;

use super::{HOURLY_CONVERSATIONS, HOURLY_TOP, HOURS_KEPT, MINUTES_KEPT};

/// Dimensions a report may hold.
const DIMENSIONS: [&str; 6] = ["src", "dst", "app", "protocol", "in", "out"];
/// Longest key kept, as the table allows.
const MAX_KEY: usize = 64;
/// Keys per dimension, and conversations, taken from one report.
const MAX_TOPS: usize = 50;
const MAX_CONVERSATIONS: usize = 200;
/// How old a report may be: a relay's queue holds an hour of them.
const MAX_AGE: Span = Span::hours(2);
const MAX_AHEAD: Span = Span::minutes(5);

/// A device, its site, its source address, its probe and the probe's
/// relay.
type ExporterRow = (String, String, Option<String>, String, Option<String>);

/// Source, destination, application, input and output.
type ConversationKey<'a> = (&'a str, &'a str, &'a str, i64, i64);

/// The exporters of each probe (`central` or a relay's ID), and each
/// exporter's site name, from the devices still in the fleet whose probe
/// is still theirs.
pub async fn exporters(
    pool: &PgPool,
) -> Result<HashMap<String, (Vec<Exporter>, BTreeMap<String, String>)>, sqlx::Error> {
    let rows: Vec<ExporterRow> = sqlx::query_as(
        "SELECT s.peer_id, si.name, coalesce(f.source_address, s.address), f.probe, r.peer_id
         FROM flow_exporters f
         JOIN servers s ON s.id = f.server_id
         JOIN sites si ON si.id = s.site_id
         LEFT JOIN relays r ON r.id = f.probe_relay_id
         WHERE s.revoked_at IS NULL
           AND (f.probe = 'central' OR (r.revoked_at IS NULL AND r.site_id = s.site_id))
         ORDER BY s.peer_id",
    )
    .fetch_all(pool)
    .await?;
    let mut out: HashMap<String, (Vec<Exporter>, BTreeMap<String, String>)> = HashMap::new();
    for (device, site, address, probe, relay) in rows {
        let Some(address) = address else { continue };
        let key = if probe == "central" {
            monitorflow_probe::CENTRAL.to_owned()
        } else if let Some(relay) = relay {
            relay
        } else {
            continue;
        };
        let entry = out.entry(key).or_default();
        entry.0.push(Exporter {
            device: device.clone(),
            addresses: vec![address],
        });
        entry.1.insert(device, site);
    }
    Ok(out)
}

fn clamp(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

fn key_ok(key: &str) -> bool {
    key.len() <= MAX_KEY && !key.chars().any(char::is_control)
}

/// Writes `reports`; those of devices that are not exporters, too old or
/// ahead of `now`, and malformed keys, are left out.
pub async fn insert(
    pool: &PgPool,
    reports: &[FlowReport],
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    for report in reports {
        let Some(at) = Utc.timestamp_millis_opt(report.start_unix_ms).single() else {
            continue;
        };
        if !(1..=3600).contains(&report.seconds) || at < now - MAX_AGE || at > now + MAX_AHEAD {
            tracing::debug!(device = %report.device.escape_debug(), "a flow report out of time; dropped");
            continue;
        }
        let seconds = i32::try_from(report.seconds).unwrap_or(3600);
        // Merged by key, so one statement never touches a row twice.
        let mut tops: BTreeMap<(&str, &str), (i64, i64)> = BTreeMap::new();
        tops.insert(("total", ""), (clamp(report.bytes), clamp(report.packets)));
        let mut per_dimension: HashMap<&str, usize> = HashMap::new();
        for top in &report.tops {
            let Some(dimension) = DIMENSIONS.iter().find(|d| **d == top.dimension) else {
                continue;
            };
            let count = per_dimension.entry(dimension).or_default();
            if *count >= MAX_TOPS || !key_ok(&top.key) {
                continue;
            }
            *count += 1;
            let entry = tops.entry((dimension, top.key.as_str())).or_default();
            entry.0 = entry.0.saturating_add(clamp(top.bytes));
            entry.1 = entry.1.saturating_add(clamp(top.packets));
        }
        let mut conversations: BTreeMap<ConversationKey<'_>, (i64, i64)> = BTreeMap::new();
        for c in report.conversations.iter().take(MAX_CONVERSATIONS) {
            if !(key_ok(&c.src) && key_ok(&c.dst) && key_ok(&c.app)) {
                continue;
            }
            let entry = conversations
                .entry((
                    c.src.as_str(),
                    c.dst.as_str(),
                    c.app.as_str(),
                    i64::from(c.input),
                    i64::from(c.output),
                ))
                .or_default();
            entry.0 = entry.0.saturating_add(clamp(c.bytes));
            entry.1 = entry.1.saturating_add(clamp(c.packets));
        }
        let mut tx = pool.begin().await?;
        let id: Option<uuid::Uuid> = sqlx::query_scalar(
            "SELECT s.id FROM servers s JOIN flow_exporters f ON f.server_id = s.id
             WHERE s.peer_id = $1 AND s.revoked_at IS NULL",
        )
        .bind(&report.device)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(id) = id else {
            continue;
        };
        let (dimensions, keys): (Vec<&str>, Vec<&str>) = tops.keys().copied().unzip();
        let (bytes, packets): (Vec<i64>, Vec<i64>) = tops.values().copied().unzip();
        sqlx::query(
            "INSERT INTO flow_tops (server_id, at, seconds, dimension, key, bytes, packets)
             SELECT $1, $2, $3, d, k, b, p
             FROM UNNEST($4::text[], $5::text[], $6::bigint[], $7::bigint[]) AS t(d, k, b, p)
             ON CONFLICT (server_id, dimension, at, seconds, key) DO UPDATE
             SET bytes = flow_tops.bytes + EXCLUDED.bytes, packets = flow_tops.packets + EXCLUDED.packets",
        )
        .bind(id)
        .bind(at)
        .bind(seconds)
        .bind(&dimensions)
        .bind(&keys)
        .bind(&bytes)
        .bind(&packets)
        .execute(&mut *tx)
        .await?;
        if !conversations.is_empty() {
            let mut text: [Vec<&str>; 3] = Default::default();
            let mut numbers: [Vec<i64>; 4] = Default::default();
            for ((src, dst, app, input, output), (b, p)) in &conversations {
                for (column, value) in text.iter_mut().zip([*src, *dst, *app]) {
                    column.push(value);
                }
                for (column, value) in numbers.iter_mut().zip([*input, *output, *b, *p]) {
                    column.push(value);
                }
            }
            sqlx::query(
                "INSERT INTO flow_conversations (server_id, at, seconds, src, dst, app, input, output, bytes, packets)
                 SELECT $1, $2, $3, s, d, a, i, o, b, p
                 FROM UNNEST($4::text[], $5::text[], $6::text[], $7::bigint[], $8::bigint[], $9::bigint[], $10::bigint[])
                      AS t(s, d, a, i, o, b, p)
                 ON CONFLICT (server_id, at, seconds, src, dst, app, input, output) DO UPDATE
                 SET bytes = flow_conversations.bytes + EXCLUDED.bytes,
                     packets = flow_conversations.packets + EXCLUDED.packets",
            )
            .bind(id)
            .bind(at)
            .bind(seconds)
            .bind(&text[0])
            .bind(&text[1])
            .bind(&text[2])
            .bind(&numbers[0])
            .bind(&numbers[1])
            .bind(&numbers[2])
            .bind(&numbers[3])
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
    }
    Ok(())
}

/// What one run of [`maintain`] did, in rows.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Maintained {
    pub minutes_summed: u64,
    pub dropped: u64,
}

impl Maintained {
    pub fn any(&self) -> bool {
        self.minutes_summed > 0 || self.dropped > 0
    }
}

/// Sums the minutes of the hours that ended [`MINUTES_KEPT`] before
/// `now` into hours, keeping the largest keys of each, and drops hours
/// older than [`HOURS_KEPT`].
pub async fn maintain(pool: &PgPool, now: DateTime<Utc>) -> Result<Maintained, sqlx::Error> {
    let cut = now - Span::from_std(MINUTES_KEPT).unwrap_or_default();
    let oldest = now - Span::from_std(HOURS_KEPT).unwrap_or_default();
    let mut tx = pool.begin().await?;
    // Two instances never sum the same minutes twice.
    sqlx::query("LOCK TABLE flow_tops, flow_conversations IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO flow_tops (server_id, at, seconds, dimension, key, bytes, packets)
         SELECT server_id, hour, 3600, dimension, key, bytes, packets FROM (
             SELECT server_id, date_trunc('hour', at) AS hour, dimension, key,
                    sum(bytes)::bigint AS bytes, sum(packets)::bigint AS packets,
                    row_number() OVER (PARTITION BY server_id, date_trunc('hour', at), dimension
                                       ORDER BY sum(bytes) DESC, key) AS rank
             FROM flow_tops
             WHERE seconds < 3600 AND at < date_trunc('hour', $1::timestamptz)
             GROUP BY server_id, date_trunc('hour', at), dimension, key
         ) s
         WHERE rank <= $2 OR dimension = 'total'
         ON CONFLICT (server_id, dimension, at, seconds, key) DO UPDATE
         SET bytes = flow_tops.bytes + EXCLUDED.bytes, packets = flow_tops.packets + EXCLUDED.packets",
    )
    .bind(cut)
    .bind(HOURLY_TOP)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO flow_conversations (server_id, at, seconds, src, dst, app, input, output, bytes, packets)
         SELECT server_id, hour, 3600, src, dst, app, input, output, bytes, packets FROM (
             SELECT server_id, date_trunc('hour', at) AS hour, src, dst, app, input, output,
                    sum(bytes)::bigint AS bytes, sum(packets)::bigint AS packets,
                    row_number() OVER (PARTITION BY server_id, date_trunc('hour', at)
                                       ORDER BY sum(bytes) DESC, src, dst, app) AS rank
             FROM flow_conversations
             WHERE seconds < 3600 AND at < date_trunc('hour', $1::timestamptz)
             GROUP BY server_id, date_trunc('hour', at), src, dst, app, input, output
         ) s
         WHERE rank <= $2
         ON CONFLICT (server_id, at, seconds, src, dst, app, input, output) DO UPDATE
         SET bytes = flow_conversations.bytes + EXCLUDED.bytes,
             packets = flow_conversations.packets + EXCLUDED.packets",
    )
    .bind(cut)
    .bind(HOURLY_CONVERSATIONS)
    .execute(&mut *tx)
    .await?;
    let mut done = Maintained::default();
    for (summed, old) in [
        (
            "DELETE FROM flow_tops WHERE seconds < 3600 AND at < date_trunc('hour', $1::timestamptz)",
            "DELETE FROM flow_tops WHERE at < $1",
        ),
        (
            "DELETE FROM flow_conversations WHERE seconds < 3600 AND at < date_trunc('hour', $1::timestamptz)",
            "DELETE FROM flow_conversations WHERE at < $1",
        ),
    ] {
        done.minutes_summed += sqlx::query(summed)
            .bind(cut)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        done.dropped += sqlx::query(old)
            .bind(oldest)
            .execute(&mut *tx)
            .await?
            .rows_affected();
    }
    tx.commit().await?;
    Ok(done)
}
