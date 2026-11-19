//! Writes what [`super::Sink`] gathered, keeps the table small, and
//! matches virtual machines to servers.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::sync::mpsc;
use tokio::time::Instant;
use uuid::Uuid;

use super::matching::{self, ServerFacts, VmFacts};
use super::{gather, Gathered, FLUSH};
use crate::ingest::Series;

/// Objects not seen for this long are removed.
const KEEP: &str = "1 day";
/// How often old objects are removed.
const PRUNE: Duration = Duration::from_secs(600);
/// Least time between two matchings of one source.
const MATCH_EVERY: Duration = Duration::from_secs(60);

pub(super) async fn run(pool: PgPool, mut rx: mpsc::Receiver<Vec<Series>>) {
    let mut batches: Vec<Vec<Series>> = Vec::new();
    let mut tick = tokio::time::interval(FLUSH);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut pruned = Instant::now();
    let mut matched: HashMap<Uuid, Instant> = HashMap::new();
    loop {
        tokio::select! {
            batch = rx.recv() => match batch {
                Some(batch) => batches.push(batch),
                None => return,
            },
            _ = tick.tick() => {
                if !batches.is_empty() {
                    let mut gathered = HashMap::new();
                    gather(&std::mem::take(&mut batches), &mut gathered);
                    for (peer, source) in gathered {
                        match write(&pool, &peer, &source).await {
                            Ok(Some(id)) => {
                                let due = matched.get(&id).is_none_or(|at| at.elapsed() >= MATCH_EVERY);
                                if due {
                                    matched.insert(id, Instant::now());
                                    if let Err(error) = match_source(&pool, id).await {
                                        tracing::warn!(%error, "matching virtual machines failed");
                                    }
                                }
                            }
                            Ok(None) => {}
                            Err(error) => tracing::warn!(%error, source = %peer, "writing a virtual inventory failed"),
                        }
                    }
                }
                if pruned.elapsed() >= PRUNE {
                    pruned = Instant::now();
                    matched.retain(|_, at| at.elapsed() < MATCH_EVERY);
                    if let Err(error) = prune(&pool).await {
                        tracing::warn!(%error, "removing old virtual objects failed");
                    }
                }
            }
        }
    }
}

/// Writes one source's objects; its row ID, or `None` when the peer is
/// not in the inventory.
pub(super) async fn write(
    pool: &PgPool,
    peer: &str,
    gathered: &Gathered,
) -> Result<Option<Uuid>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let source: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM servers WHERE peer_id = $1 AND revoked_at IS NULL")
            .bind(peer)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(source) = source else {
        return Ok(None);
    };
    if let Some(state) = &gathered.source {
        sqlx::query(
            "INSERT INTO virtual_sources (server_id, up, reason, error, details, seen_at)
             VALUES ($1, $2, $3, $4, $5, now())
             ON CONFLICT (server_id) DO UPDATE
             SET up = $2, reason = $3, error = $4, details = $5, seen_at = now()",
        )
        .bind(source)
        .bind(state.up)
        .bind(state.reason.as_deref().map(|r| cut(r, 64)))
        .bind(state.error.as_deref().map(|e| cut(e, 512)))
        .bind(json!(state.details))
        .execute(&mut *tx)
        .await?;
    }
    let row = |kind: &str, instance: &str, o: &super::Object| {
        json!({
            "kind": kind, "instance": instance, "key": o.key.clone().unwrap_or_default(),
            "details": o.details, "metrics": o.metrics,
            "collected_ms": o.collected_ms,
        })
    };
    let (described, numbers): (Vec<Value>, Vec<Value>) = {
        let mut described = Vec::new();
        let mut numbers = Vec::new();
        for ((kind, instance), object) in &gathered.objects {
            if object.details.is_some() {
                described.push(row(kind, instance, object));
            } else {
                numbers.push(row(kind, instance, object));
            }
        }
        (described, numbers)
    };
    if !described.is_empty() {
        // A newer collection replaces the numbers; the same one adds to
        // them, as a large one comes in several batches.
        sqlx::query(
            "INSERT INTO virtual_objects (source_id, kind, instance, key, details, metrics, collected_at, seen_at)
             SELECT $1, r.kind, r.instance, r.key, r.details, r.metrics,
                    to_timestamp(r.collected_ms / 1000.0), now()
             FROM jsonb_to_recordset($2) AS r(kind text, instance text, key text, details jsonb,
                                              metrics jsonb, collected_ms bigint)
             ON CONFLICT (source_id, kind, instance) DO UPDATE
             SET key = EXCLUDED.key, details = EXCLUDED.details, seen_at = now(),
                 metrics = CASE WHEN EXCLUDED.collected_at > virtual_objects.collected_at
                                THEN EXCLUDED.metrics
                                ELSE virtual_objects.metrics || EXCLUDED.metrics END,
                 collected_at = GREATEST(virtual_objects.collected_at, EXCLUDED.collected_at)",
        )
        .bind(source)
        .bind(Value::Array(described))
        .execute(&mut *tx)
        .await?;
    }
    if !numbers.is_empty() {
        sqlx::query(
            "UPDATE virtual_objects o
             SET metrics = CASE WHEN to_timestamp(r.collected_ms / 1000.0) > o.collected_at
                                THEN r.metrics ELSE o.metrics || r.metrics END,
                 collected_at = GREATEST(o.collected_at, to_timestamp(r.collected_ms / 1000.0))
             FROM jsonb_to_recordset($2) AS r(kind text, instance text, metrics jsonb, collected_ms bigint)
             WHERE o.source_id = $1 AND o.kind = r.kind AND o.instance = r.instance",
        )
        .bind(source)
        .bind(Value::Array(numbers))
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(Some(source))
}

fn cut(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

async fn prune(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM virtual_objects WHERE seen_at < now() - $1::interval")
        .bind(KEEP)
        .execute(pool)
        .await?;
    Ok(())
}

/// Matches the virtual machines of `source` that no person tied, to the
/// servers and devices of the source's customer.
pub async fn match_source(pool: &PgPool, source: Uuid) -> Result<usize, sqlx::Error> {
    let mut tx = pool.begin().await?;
    type VmRow = (Uuid, String, Value, Option<Uuid>, Option<String>);
    let vms: Vec<VmRow> = sqlx::query_as(
        "SELECT id, instance, details, server_id, matched_by FROM virtual_objects
         WHERE source_id = $1 AND kind = 'vm' AND NOT link_fixed
         FOR UPDATE",
    )
    .bind(source)
    .fetch_all(&mut *tx)
    .await?;
    if vms.is_empty() {
        return Ok(0);
    }
    type Row = (Uuid, String, Option<Uuid>, Vec<String>, Option<String>);
    let servers: Vec<Row> = sqlx::query_as(
        "SELECT s.id, s.hostname, s.system_uuid, s.mac_addresses, s.address
         FROM servers s JOIN sites si ON si.id = s.site_id
         WHERE s.revoked_at IS NULL AND s.id <> $1
           AND si.customer_id = (SELECT si2.customer_id FROM servers s2
                                 JOIN sites si2 ON si2.id = s2.site_id WHERE s2.id = $1)",
    )
    .bind(source)
    .fetch_all(&mut *tx)
    .await?;
    let servers: Vec<ServerFacts> = servers
        .into_iter()
        .map(|(id, hostname, uuid, macs, address)| ServerFacts {
            id,
            hostname,
            uuid,
            macs,
            address,
        })
        .collect();
    let facts: Vec<VmFacts> = vms
        .iter()
        .map(|(id, instance, details, _, _)| VmFacts::from_details(*id, instance, details))
        .collect();
    let found = matching::matches(&facts, &servers);
    let mut changed = 0;
    for (id, _, _, server, by) in &vms {
        let now = found.get(id).map(|(s, signal)| (*s, signal.as_str()));
        let before = server.zip(by.as_deref());
        if now == before {
            continue;
        }
        sqlx::query("UPDATE virtual_objects SET server_id = $2, matched_by = $3 WHERE id = $1")
            .bind(id)
            .bind(now.map(|n| n.0))
            .bind(now.map(|n| n.1))
            .execute(&mut *tx)
            .await?;
        changed += 1;
    }
    tx.commit().await?;
    Ok(changed)
}
