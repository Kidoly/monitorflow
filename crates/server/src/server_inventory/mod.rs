//! The hardware and software inventory of servers (ADR-0187): what agents
//! report ([`record`]), item by item in `server_inventory_items`, the
//! history of changes in `server_inventory_changes` (with those of the
//! Linux packages, [`package_changes`]), and the fleet search of
//! [`search`].

pub mod search;
pub mod version;

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use monitorflow_common::inventory::{self, Change};
use monitorflow_common::proto::v1::{
    inventory_report, Inventory, InventoryField, InventoryItem, InventoryReport,
};
use serde_json::{Map, Value};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

/// How long changes are kept.
pub const KEEP_DAYS: i32 = 366;

/// What became of a report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// No enrolled server has this agent's ID.
    UnknownServer,
    /// The server holds this inventory already.
    Confirmed,
    /// Stored, with this many changes recorded.
    Stored { changes: usize },
    /// The server cannot use the report: ask the agent for more, saying
    /// it holds this hash (empty for nothing usable).
    Want(Vec<u8>),
}

/// Records the report of the agent `peer_id`.
pub async fn record(
    pool: &PgPool,
    peer_id: &str,
    report: InventoryReport,
) -> Result<Outcome, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let server: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM servers WHERE peer_id = $1 AND revoked_at IS NULL")
            .bind(peer_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(server) = server else {
        return Ok(Outcome::UnknownServer);
    };
    let stored: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT hash FROM server_inventories WHERE server_id = $1 FOR UPDATE")
            .bind(server)
            .fetch_optional(&mut *tx)
            .await?;
    let collected_at = DateTime::<Utc>::from_timestamp_millis(report.collected_at_unix_ms)
        .filter(|at| report.collected_at_unix_ms > 0 && *at <= Utc::now() + chrono::Days::new(1))
        .unwrap_or_else(Utc::now);
    let new = match report.body {
        None => {
            if stored.as_deref() != Some(report.hash.as_slice()) {
                return Ok(Outcome::Want(stored.unwrap_or_default()));
            }
            sqlx::query(
                "UPDATE server_inventories SET checked_at = now(), collected_at = $2
                 WHERE server_id = $1",
            )
            .bind(server)
            .bind(collected_at)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(Outcome::Confirmed);
        }
        Some(inventory_report::Body::Full(full)) => inventory::bound(full).0,
        Some(inventory_report::Body::Delta(delta)) => {
            let Some(stored) = stored.as_ref().filter(|h| **h == delta.base_hash) else {
                return Ok(Outcome::Want(stored.unwrap_or_default()));
            };
            let base = load(&mut tx, server).await?;
            if inventory::hash(&base) != *stored {
                // The rows no longer match their hash: start again.
                return Ok(Outcome::Want(Vec::new()));
            }
            let new = inventory::bound(inventory::apply(&base, &delta)).0;
            if inventory::hash(&new) != report.hash {
                return Ok(Outcome::Want(Vec::new()));
            }
            new
        }
    };
    let changes = store(&mut tx, server, stored.is_some(), new, collected_at).await?;
    tx.commit().await?;
    Ok(Outcome::Stored { changes })
}

/// The server's stored inventory, canonical.
async fn load(tx: &mut Transaction<'_, Postgres>, server: Uuid) -> Result<Inventory, sqlx::Error> {
    let rows: Vec<(String, String, Value)> =
        sqlx::query_as("SELECT kind, key, fields FROM server_inventory_items WHERE server_id = $1")
            .bind(server)
            .fetch_all(&mut **tx)
            .await?;
    Ok(inventory::canonical(Inventory {
        items: rows
            .into_iter()
            .map(|(kind, key, fields)| InventoryItem {
                kind,
                key,
                fields: match fields {
                    Value::Object(map) => map
                        .into_iter()
                        .filter_map(|(name, value)| match value {
                            Value::String(value) => Some(InventoryField { name, value }),
                            _ => None,
                        })
                        .collect(),
                    _ => Vec::new(),
                },
            })
            .collect(),
    }))
}

/// An item's fields as a JSON object.
pub fn fields_json(item: &InventoryItem) -> Value {
    Value::Object(
        item.fields
            .iter()
            .map(|f| (f.name.clone(), Value::String(f.value.clone())))
            .collect::<Map<String, Value>>(),
    )
}

/// Replaces the server's items with `new`, recording what changed unless
/// this is the server's first inventory. Returns the number of changes.
async fn store(
    tx: &mut Transaction<'_, Postgres>,
    server: Uuid,
    had_one: bool,
    new: Inventory,
    collected_at: DateTime<Utc>,
) -> Result<usize, sqlx::Error> {
    let old = load(tx, server).await?;
    let found = inventory::changes(&old, &new);
    let mut gone: (Vec<&str>, Vec<&str>) = (Vec::new(), Vec::new());
    let mut set: (Vec<&str>, Vec<&str>, Vec<Value>) = (Vec::new(), Vec::new(), Vec::new());
    let mut history = History::default();
    for change in &found {
        match change {
            Change::Removed(item) => {
                gone.0.push(&item.kind);
                gone.1.push(&item.key);
                history.push(&item.kind, &item.key, Some(fields_json(item)), None);
            }
            Change::Added(item) => {
                set.0.push(&item.kind);
                set.1.push(&item.key);
                set.2.push(fields_json(item));
                history.push(&item.kind, &item.key, None, Some(fields_json(item)));
            }
            Change::Changed { before, after } => {
                set.0.push(&after.kind);
                set.1.push(&after.key);
                set.2.push(fields_json(after));
                history.push(
                    &after.kind,
                    &after.key,
                    Some(fields_json(before)),
                    Some(fields_json(after)),
                );
            }
        }
    }
    if !gone.0.is_empty() {
        sqlx::query(
            "DELETE FROM server_inventory_items i
             USING UNNEST($2::text[], $3::text[]) AS g (kind, key)
             WHERE i.server_id = $1 AND i.kind = g.kind AND i.key = g.key",
        )
        .bind(server)
        .bind(&gone.0)
        .bind(&gone.1)
        .execute(&mut **tx)
        .await?;
    }
    if !set.0.is_empty() {
        sqlx::query(
            "INSERT INTO server_inventory_items (server_id, kind, key, fields)
             SELECT $1, kind, key, fields FROM UNNEST($2::text[], $3::text[], $4::jsonb[])
                 AS s (kind, key, fields)
             ON CONFLICT (server_id, kind, key) DO UPDATE SET fields = EXCLUDED.fields",
        )
        .bind(server)
        .bind(&set.0)
        .bind(&set.1)
        .bind(&set.2)
        .execute(&mut **tx)
        .await?;
    }
    let changes = found.len();
    if had_one {
        history.write(tx, server).await?;
    }
    sqlx::query(
        "INSERT INTO server_inventories (server_id, hash, collected_at)
         VALUES ($1, $2, $3)
         ON CONFLICT (server_id) DO UPDATE SET
             hash = EXCLUDED.hash, collected_at = EXCLUDED.collected_at, checked_at = now(),
             changed_at = CASE WHEN $4 THEN now() ELSE server_inventories.changed_at END",
    )
    .bind(server)
    .bind(inventory::hash(&new))
    .bind(collected_at)
    .bind(changes > 0)
    .execute(&mut **tx)
    .await?;
    Ok(changes)
}

/// Change rows waiting to be written.
#[derive(Debug, Default)]
pub struct History {
    kinds: Vec<String>,
    keys: Vec<String>,
    changes: Vec<&'static str>,
    before: Vec<Option<Value>>,
    after: Vec<Option<Value>>,
}

impl History {
    pub fn push(&mut self, kind: &str, key: &str, before: Option<Value>, after: Option<Value>) {
        let change = match (&before, &after) {
            (None, _) => "added",
            (_, None) => "removed",
            _ => "changed",
        };
        self.kinds.push(kind.to_owned());
        self.keys.push(key.to_owned());
        self.changes.push(change);
        self.before.push(before);
        self.after.push(after);
    }

    pub fn len(&self) -> usize {
        self.kinds.len()
    }

    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }

    /// Writes the rows, and drops the server's rows older than
    /// [`KEEP_DAYS`].
    pub async fn write(
        self,
        tx: &mut Transaction<'_, Postgres>,
        server: Uuid,
    ) -> Result<(), sqlx::Error> {
        if self.is_empty() {
            return Ok(());
        }
        sqlx::query(
            "INSERT INTO server_inventory_changes (server_id, kind, key, change, before, after)
             SELECT $1, kind, key, change, before, after
             FROM UNNEST($2::text[], $3::text[], $4::text[], $5::jsonb[], $6::jsonb[])
                 AS c (kind, key, change, before, after)",
        )
        .bind(server)
        .bind(&self.kinds)
        .bind(&self.keys)
        .bind(&self.changes)
        .bind(&self.before)
        .bind(&self.after)
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            "DELETE FROM server_inventory_changes
             WHERE server_id = $1 AND at < now() - make_interval(days => $2)",
        )
        .bind(server)
        .bind(KEEP_DAYS)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }
}

/// The changes between two package lists of one server, by name and
/// architecture, as history rows of kind `package`. Entries without an
/// installed version (pending Windows updates) are left out.
pub fn package_changes(old: &[(String, String, String)], new: &[(&str, &str, &str)]) -> History {
    let before: BTreeMap<(&str, &str), &str> = old
        .iter()
        .filter(|(_, _, v)| !v.is_empty())
        .map(|(n, a, v)| ((n.as_str(), a.as_str()), v.as_str()))
        .collect();
    let after: BTreeMap<(&str, &str), &str> = new
        .iter()
        .filter(|(_, _, v)| !v.is_empty())
        .map(|(n, a, v)| ((*n, *a), *v))
        .collect();
    let fields = |arch: &str, version: &str| {
        let mut map = Map::new();
        map.insert("version".into(), Value::String(version.to_owned()));
        if !arch.is_empty() {
            map.insert("arch".into(), Value::String(arch.to_owned()));
        }
        Some(Value::Object(map))
    };
    let mut history = History::default();
    for ((name, arch), version) in &before {
        match after.get(&(name, arch)) {
            None => history.push("package", name, fields(arch, version), None),
            Some(now) if now != version => {
                history.push("package", name, fields(arch, version), fields(arch, now))
            }
            Some(_) => {}
        }
    }
    for ((name, arch), version) in &after {
        if !before.contains_key(&(name, arch)) {
            history.push("package", name, None, fields(arch, version));
        }
    }
    history
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_changes_are_by_name_and_architecture() {
        let old = vec![
            ("openssl".into(), "amd64".into(), "3.0.14-1".into()),
            ("libc6".into(), "amd64".into(), "2.36-9".into()),
            ("libc6".into(), "i386".into(), "2.36-9".into()),
            ("gone".into(), "all".into(), "1".into()),
            ("KB5034441".into(), "".into(), "".into()),
        ];
        let new = vec![
            ("openssl", "amd64", "3.0.15-1"),
            ("libc6", "amd64", "2.36-9"),
            ("libc6", "i386", "2.36-9"),
            ("curl", "amd64", "7.88"),
            ("KB5034441", "", "1"),
        ];
        let history = package_changes(&old, &new);
        let rows: Vec<(&str, &str)> = history
            .keys
            .iter()
            .map(String::as_str)
            .zip(history.changes.iter().copied())
            .collect();
        assert_eq!(
            rows,
            [
                ("gone", "removed"),
                ("openssl", "changed"),
                ("KB5034441", "added"),
                ("curl", "added")
            ]
        );
        assert_eq!(
            history.after[1],
            Some(serde_json::json!({"version": "3.0.15-1", "arch": "amd64"}))
        );
        assert!(package_changes(&old, &[]).len() == 4);
    }
}
