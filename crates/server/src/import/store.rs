//! Import runs in the database (ADR-0146): what MonitorFlow has already,
//! the preview, the confirmation and the undo.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, TimeDelta, Utc};
use monitorflow_common::redact::Secret;
use monitorflow_common::tls::PeerRole;
use monitorflow_import::Inventory;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgConnection, PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::plan::{channel_key, Existing, ExistingServer};
use super::{ChannelSecret, Create, Item, PREVIEW_HOURS};
use crate::enroll::identity::{self, Pending};
use crate::enroll::install_key::peer_id_from_hostname;
use crate::notify::{self, Coverage, Destination};
use crate::secrets::{self, Column};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error("{0}")]
    Secrets(String),
    /// A sentence for the person, about the run's state.
    #[error("{0}")]
    State(String),
}

/// What a confirmation or an undo did.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, utoipa::ToSchema)]
#[schema(as = ImportResult)]
pub struct Outcome {
    /// Created, by object: `server`, `check`, `threshold`, `silence`,
    /// `channel`.
    #[serde(default)]
    pub created: BTreeMap<String, u32>,
    /// Items unticked, or that need an unticked one.
    #[serde(default)]
    pub skipped: u32,
    /// Items MonitorFlow had already, then or by the confirmation.
    #[serde(default)]
    pub present: u32,
    /// Items that could not be created, and why.
    #[serde(default)]
    pub failed: Vec<Failure>,
    /// Removed by the undo, by object.
    #[serde(default)]
    pub removed: BTreeMap<String, u32>,
    /// Servers an agent took over, which the undo keeps with what was
    /// created on them.
    #[serde(default)]
    pub kept_servers: Vec<String>,
}

/// An item that could not be created.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[schema(as = ImportFailure)]
pub struct Failure {
    pub item: String,
    /// In English.
    pub error: String,
}

/// One run as stored.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Run {
    pub id: Uuid,
    pub source: String,
    pub site_id: Uuid,
    pub input: String,
    pub state: String,
    pub items: sqlx::types::Json<Vec<Item>>,
    pub secrets: Option<String>,
    pub result: Option<sqlx::types::Json<Outcome>>,
    pub created_at: DateTime<Utc>,
    pub created_by: Option<Uuid>,
    pub confirmed_at: Option<DateTime<Utc>>,
    pub undone_at: Option<DateTime<Utc>>,
}

const RUN_COLUMNS: &str = "id, source, site_id, input, state, items, secrets, result,
    created_at, created_by, confirmed_at, undone_at";

/// What MonitorFlow has of what `inventory` describes. `may_change` says
/// which sites the person administers.
pub async fn existing(
    pool: &PgPool,
    inventory: &Inventory,
    site_id: Uuid,
    may_change: impl Fn(Uuid) -> bool,
) -> Result<Existing, StoreError> {
    let ids: Vec<String> = inventory
        .hosts
        .iter()
        .map(|h| peer_id_from_hostname(&h.name).to_ascii_lowercase())
        .collect();
    let names: Vec<String> = inventory
        .hosts
        .iter()
        .map(|h| h.name.trim().to_ascii_lowercase())
        .collect();
    let rows: Vec<(Uuid, String, String, Uuid)> = sqlx::query_as(
        "SELECT id, peer_id, hostname, site_id FROM servers
         WHERE revoked_at IS NULL
           AND (lower(peer_id) = ANY($1) OR lower(hostname) = ANY($2))",
    )
    .bind(&ids)
    .bind(&names)
    .fetch_all(pool)
    .await?;
    let mut existing = Existing::default();
    let server_ids: Vec<Uuid> = rows.iter().map(|r| r.0).collect();
    let mut peer_of: HashMap<Uuid, String> = HashMap::new();
    for (id, peer_id, hostname, site) in rows {
        peer_of.insert(id, peer_id.to_ascii_lowercase());
        existing.servers.push(ExistingServer {
            peer_id,
            hostname,
            site_id: site,
            may_change: may_change(site),
        });
    }
    let checks: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT server_id, name FROM sensor_configs WHERE server_id = ANY($1) AND instance = ''",
    )
    .bind(&server_ids)
    .fetch_all(pool)
    .await?;
    existing.checks = checks
        .into_iter()
        .filter_map(|(id, name)| Some((peer_of.get(&id)?.clone(), name)))
        .collect();
    let thresholds: Vec<(Uuid, String, Option<String>)> = sqlx::query_as(
        "SELECT server_id, metric, instance FROM thresholds WHERE server_id = ANY($1)",
    )
    .bind(&server_ids)
    .fetch_all(pool)
    .await?;
    existing.thresholds = thresholds
        .into_iter()
        .filter_map(|(id, metric, instance)| Some((peer_of.get(&id)?.clone(), metric, instance)))
        .collect();
    let silences: Vec<(Option<Uuid>, DateTime<Utc>, String)> = sqlx::query_as(
        "SELECT server_id, ends_at, reason FROM silences
         WHERE ended_at IS NULL AND ends_at > now()
           AND (server_id = ANY($1) OR site_id = $2)",
    )
    .bind(&server_ids)
    .bind(site_id)
    .fetch_all(pool)
    .await?;
    existing.silences = silences
        .into_iter()
        .map(|(id, ends, reason)| (id.and_then(|id| peer_of.get(&id).cloned()), ends, reason))
        .collect();
    let channels: Vec<(String, Value, Option<String>)> =
        sqlx::query_as("SELECT kind, settings, secret FROM notification_channels")
            .fetch_all(pool)
            .await?;
    for (kind, settings, secret) in channels {
        let key = match kind.as_str() {
            "email" => {
                let mut to: Vec<String> = settings["to"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                to.sort();
                let host = settings["host"].as_str().unwrap_or_default();
                Some(channel_key("email", &format!("{host}|{}", to.join(","))))
            }
            "webhook" => settings["url"].as_str().map(|u| channel_key("webhook", u)),
            "slack" | "teams" | "discord" => {
                secrets::open_stored(Column::NotificationChannels, secret)
                    .and_then(|o| o.ok())
                    .map(|url| channel_key(&kind, url.expose()))
            }
            _ => None,
        };
        existing.channels.extend(key);
    }
    Ok(existing)
}

/// Stores a preview; returns its ID. Old previews nobody confirmed go.
pub async fn create(
    pool: &PgPool,
    source: &str,
    site_id: Uuid,
    input: &str,
    items: &[Item],
    channel_secrets: &BTreeMap<String, ChannelSecret>,
    user: Uuid,
) -> Result<Uuid, StoreError> {
    let sealed = if channel_secrets.is_empty() {
        None
    } else {
        let json = serde_json::to_string(channel_secrets).unwrap_or_default();
        Some(
            secrets::seal(Column::ImportRuns, &json)
                .map_err(|e| StoreError::Secrets(e.to_string()))?,
        )
    };
    let input: String = input.chars().take(1024).collect();
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM import_runs WHERE state = 'preview' AND created_at < now() - interval '7 days'")
        .execute(&mut *tx)
        .await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO import_runs (source, site_id, input, items, secrets, created_by)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(source)
    .bind(site_id)
    .bind(if input.is_empty() {
        "?".to_owned()
    } else {
        input
    })
    .bind(sqlx::types::Json(items))
    .bind(sealed)
    .bind(user)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(id)
}

/// The run `id`.
pub async fn get(db: &mut PgConnection, id: Uuid, lock: bool) -> Result<Option<Run>, StoreError> {
    let lock = if lock { " FOR UPDATE" } else { "" };
    // SQL-safe: interpolates only constants.
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {RUN_COLUMNS} FROM import_runs WHERE id = $1{lock}"
    )))
    .bind(id)
    .fetch_optional(db)
    .await?)
}

/// The latest runs of `sites` (every site with `all`).
pub async fn list(pool: &PgPool, all: bool, sites: &[Uuid]) -> Result<Vec<Run>, StoreError> {
    // SQL-safe: interpolates only constants.
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {RUN_COLUMNS} FROM import_runs WHERE $1 OR site_id = ANY($2)
         ORDER BY created_at DESC LIMIT 50"
    )))
    .bind(all)
    .bind(sites)
    .fetch_all(pool)
    .await?)
}

/// The items to create: not unticked, not present, and whose needs are
/// created or present too. Items come in dependency order.
pub fn chosen(items: &[Item], skip: &HashSet<String>) -> (Vec<Item>, u32, u32) {
    let mut kept: HashSet<&str> = HashSet::new();
    let mut out = Vec::new();
    let (mut skipped, mut present) = (0, 0);
    for item in items {
        if item.create.is_none() || item.list == super::List::NotMovable {
            continue;
        }
        let needs_ok = item.needs.iter().all(|n| kept.contains(n.as_str()));
        if skip.contains(&item.id) || !needs_ok {
            skipped += 1;
            continue;
        }
        kept.insert(&item.id);
        if item.present {
            present += 1;
        } else {
            out.push(item.clone());
        }
    }
    (out, skipped, present)
}

/// Creates what `run` planned but `skip`, in one transaction; records
/// what it created. `channel_public_only` is what the channels page sets
/// for someone who is not a global administrator.
pub async fn confirm(
    pool: &PgPool,
    id: Uuid,
    skip: &HashSet<String>,
    user: Uuid,
    channel_public_only: bool,
    now: DateTime<Utc>,
) -> Result<(Run, Outcome), StoreError> {
    let mut tx = pool.begin().await?;
    let run = get(&mut tx, id, true)
        .await?
        .ok_or_else(|| StoreError::State("There is no such import.".into()))?;
    if run.state != "preview" {
        return Err(StoreError::State(
            "This import was confirmed already.".into(),
        ));
    }
    if now - run.created_at > TimeDelta::hours(PREVIEW_HOURS) {
        return Err(StoreError::State(
            "This preview is more than a day old: read the source again.".into(),
        ));
    }
    let channel_secrets: BTreeMap<String, ChannelSecret> = match &run.secrets {
        Some(sealed) => {
            let opened = secrets::open(Column::ImportRuns, sealed)
                .map_err(|e| StoreError::Secrets(e.to_string()))?;
            serde_json::from_str(opened.expose()).unwrap_or_default()
        }
        None => BTreeMap::new(),
    };
    let (items, skipped, present) = chosen(&run.items, skip);
    let mut outcome = Outcome {
        skipped,
        present,
        ..Outcome::default()
    };
    let mut servers: HashMap<String, Uuid> = HashMap::new();
    for item in &items {
        let create = item.create.as_ref().expect("chosen items create");
        let mut savepoint = sqlx::Acquire::begin(&mut *tx).await?;
        let result = create_one(
            &mut savepoint,
            run.site_id,
            create,
            channel_secrets.get(&item.id),
            &mut servers,
            user,
            channel_public_only,
        )
        .await;
        match result {
            Ok(Some(object_id)) => {
                sqlx::query(
                    "INSERT INTO import_created (run_id, item_id, object, object_id)
                     VALUES ($1, $2, $3, $4)",
                )
                .bind(run.id)
                .bind(&item.id)
                .bind(create.object())
                .bind(object_id)
                .execute(&mut *savepoint)
                .await?;
                savepoint.commit().await?;
                *outcome
                    .created
                    .entry(create.object().to_owned())
                    .or_default() += 1;
            }
            Ok(None) => {
                savepoint.commit().await?;
                outcome.present += 1;
            }
            Err(error) => {
                savepoint.rollback().await?;
                outcome.failed.push(Failure {
                    item: item.id.clone(),
                    error,
                });
            }
        }
    }
    sqlx::query(
        "UPDATE import_runs SET state = 'confirmed', confirmed_at = now(), confirmed_by = $2,
                secrets = NULL, result = $3
         WHERE id = $1",
    )
    .bind(run.id)
    .bind(user)
    .bind(sqlx::types::Json(&outcome))
    .execute(&mut *tx)
    .await?;
    crate::api::audit::record(
        &mut *tx,
        crate::api::audit::Event {
            actor_kind: "user",
            actor_id: &user.to_string(),
            action: "import.confirm",
            target: Some(("import", &run.id.to_string())),
            details: json!({
                "source": run.source,
                "site_id": run.site_id,
                "created": outcome.created,
                "skipped": outcome.skipped,
                "present": outcome.present,
                "failed": outcome.failed.len(),
            }),
        },
    )
    .await?;
    let run = get(&mut tx, id, false).await?.expect("the run exists");
    tx.commit().await?;
    Ok((run, outcome))
}

/// The server of ID `peer_id`, not removed.
async fn server_id(tx: &mut PgConnection, peer_id: &str) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT id FROM servers WHERE lower(peer_id) = lower($1) AND revoked_at IS NULL",
    )
    .bind(peer_id)
    .fetch_optional(tx)
    .await
}

fn db_error(error: sqlx::Error) -> String {
    match &error {
        sqlx::Error::Database(e) => e.message().to_owned(),
        _ => error.to_string(),
    }
}

/// Creates one object; `None` when it exists already.
async fn create_one(
    tx: &mut Transaction<'_, Postgres>,
    site_id: Uuid,
    create: &Create,
    secret: Option<&ChannelSecret>,
    servers: &mut HashMap<String, Uuid>,
    user: Uuid,
    public_only: bool,
) -> Result<Option<Uuid>, String> {
    let server = |peer_id: &str, servers: &HashMap<String, Uuid>| {
        servers.get(&peer_id.to_ascii_lowercase()).copied()
    };
    match create {
        Create::Server {
            peer_id,
            hostname,
            os_family,
            ..
        } => {
            identity::lock(tx).await.map_err(db_error)?;
            if let Some(id) = server_id(tx, peer_id).await.map_err(db_error)? {
                servers.insert(peer_id.to_ascii_lowercase(), id);
                return Ok(None);
            }
            let taken = identity::held_by_other_role(tx, PeerRole::Agent, peer_id, Pending::Counts)
                .await
                .map_err(db_error)?
                || sqlx::query_scalar::<_, bool>(
                    "SELECT EXISTS (SELECT 1 FROM servers WHERE lower(peer_id) = lower($1))",
                )
                .bind(peer_id)
                .fetch_one(&mut **tx)
                .await
                .map_err(db_error)?;
            if taken {
                return Err(format!(
                    "the ID {peer_id} is taken by a relay or a removed server"
                ));
            }
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO servers (peer_id, site_id, hostname, os_family, awaiting_agent,
                                      package_updates)
                 VALUES ($1, $2, $3, $4, true, false) RETURNING id",
            )
            .bind(peer_id)
            .bind(site_id)
            .bind(hostname)
            .bind(os_family)
            .fetch_one(&mut **tx)
            .await
            .map_err(db_error)?;
            servers.insert(peer_id.to_ascii_lowercase(), id);
            Ok(Some(id))
        }
        Create::Check {
            server: peer_id,
            kind,
            name,
            interval_ms,
            params,
        } => {
            let id = match server(peer_id, servers) {
                Some(id) => id,
                None => server_id(tx, peer_id)
                    .await
                    .map_err(db_error)?
                    .ok_or("its server is not there")?,
            };
            let valid = crate::api::sensors::validate_entry(crate::api::sensors::EntryRequest {
                kind: kind.clone(),
                name: Some(name.clone()),
                instance: String::new(),
                interval_ms: *interval_ms,
                timeout_ms: None,
                params: params.clone(),
                enabled: true,
                probe: None,
            })?;
            sqlx::query_scalar(
                "INSERT INTO sensor_configs (server_id, kind, name, instance, interval_ms, params)
                 VALUES ($1, $2, $3, '', $4, $5)
                 ON CONFLICT ON CONSTRAINT sensor_configs_key DO NOTHING RETURNING id",
            )
            .bind(id)
            .bind(&valid.kind)
            .bind(&valid.name)
            .bind(valid.interval_ms)
            .bind(&valid.params)
            .fetch_optional(&mut **tx)
            .await
            .map_err(db_error)
        }
        Create::Threshold {
            server: peer_id,
            metric,
            instance,
            comparison,
            warning,
            critical,
            duration_s,
            hysteresis,
        } => {
            let id = match server(peer_id, servers) {
                Some(id) => id,
                None => server_id(tx, peer_id)
                    .await
                    .map_err(db_error)?
                    .ok_or("its server is not there")?,
            };
            let valid =
                crate::api::thresholds::validate_entry(crate::api::thresholds::EntryRequest {
                    metric: metric.clone(),
                    instance: instance.clone(),
                    comparison: comparison.clone(),
                    warning: *warning,
                    critical: *critical,
                    duration_s: *duration_s,
                    hysteresis: *hysteresis,
                    enabled: true,
                })?;
            sqlx::query_scalar(
                "INSERT INTO thresholds (server_id, metric, instance, comparison, warning,
                                         critical, duration_s, hysteresis)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                 ON CONFLICT ON CONSTRAINT thresholds_key DO NOTHING RETURNING id",
            )
            .bind(id)
            .bind(&valid.metric)
            .bind(&valid.instance)
            .bind(valid.comparison.as_str())
            .bind(valid.warning)
            .bind(valid.critical)
            .bind(valid.duration_s)
            .bind(valid.hysteresis)
            .fetch_optional(&mut **tx)
            .await
            .map_err(db_error)
        }
        Create::Silence {
            server: peer_id,
            starts_at,
            ends_at,
            reason,
        } => {
            let server_id = match peer_id {
                Some(p) => Some(match server(p, servers) {
                    Some(id) => id,
                    None => server_id(tx, p)
                        .await
                        .map_err(db_error)?
                        .ok_or("its server is not there")?,
                }),
                None => None,
            };
            let site = server_id.is_none().then_some(site_id);
            let starts_at = (*starts_at).max(Utc::now());
            if *ends_at <= starts_at {
                return Err("the window is over".into());
            }
            let same: Option<Uuid> = sqlx::query_scalar(
                "SELECT id FROM silences
                 WHERE server_id IS NOT DISTINCT FROM $1 AND site_id IS NOT DISTINCT FROM $2
                   AND relay_id IS NULL AND customer_id IS NULL AND ended_at IS NULL
                   AND ends_at = $3 AND reason = $4 LIMIT 1",
            )
            .bind(server_id)
            .bind(site)
            .bind(ends_at)
            .bind(reason)
            .fetch_optional(&mut **tx)
            .await
            .map_err(db_error)?;
            if same.is_some() {
                return Ok(None);
            }
            sqlx::query_scalar(
                "INSERT INTO silences (server_id, site_id, starts_at, ends_at, reason, created_by)
                 VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
            )
            .bind(server_id)
            .bind(site)
            .bind(starts_at)
            .bind(ends_at)
            .bind(reason)
            .bind(user)
            .fetch_one(&mut **tx)
            .await
            .map(Some)
            .map_err(db_error)
        }
        Create::Channel {
            kind,
            settings,
            site_id: covered,
            ..
        } => {
            let mut settings = Value::Object(settings.clone());
            let secret = secret.cloned().unwrap_or_default();
            if let Some(url) = &secret.url {
                settings["url"] = Value::String(url.clone());
            }
            let (destination, secret) =
                crate::api::notifications::checked(kind, settings, secret.secret, None)
                    .map_err(|e| e.to_string())?;
            let coverage = covered.map_or(Coverage::Everything, Coverage::Site);
            if channel_exists(tx, &destination, secret.as_deref())
                .await
                .map_err(db_error)?
            {
                return Ok(None);
            }
            let secret = secret.map(Secret::new);
            notify::insert(
                &mut **tx,
                &destination,
                secret.as_ref(),
                coverage,
                public_only,
            )
            .await
            .map(Some)
            .map_err(db_error)
        }
    }
}

/// Whether a channel with the same destination exists, so a second
/// import adds none.
async fn channel_exists(
    tx: &mut PgConnection,
    destination: &Destination,
    secret: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let rows: Vec<(Value, Option<String>)> =
        sqlx::query_as("SELECT settings, secret FROM notification_channels WHERE kind = $1")
            .bind(destination.kind())
            .fetch_all(tx)
            .await?;
    let wanted = destination.settings();
    Ok(rows
        .into_iter()
        .any(|(settings, stored)| match destination {
            Destination::Email(_) => {
                settings["host"] == wanted["host"] && settings["to"] == wanted["to"]
            }
            Destination::Webhook(_) => settings["url"] == wanted["url"],
            _ => {
                let stored =
                    secrets::open_stored(Column::NotificationChannels, stored).and_then(|o| o.ok());
                stored.is_some_and(|s| Some(s.expose()) == secret)
            }
        }))
}

/// Removes what run `id` created and still exists; servers an agent took
/// over stay, with what was created on them.
pub async fn undo(pool: &PgPool, id: Uuid, user: Uuid) -> Result<(Run, Outcome), StoreError> {
    let mut tx = pool.begin().await?;
    let run = get(&mut tx, id, true)
        .await?
        .ok_or_else(|| StoreError::State("There is no such import.".into()))?;
    match run.state.as_str() {
        "preview" => {
            return Err(StoreError::State(
                "This import was not confirmed: nothing to undo.".into(),
            ))
        }
        "undone" => return Err(StoreError::State("This import was undone already.".into())),
        _ => {}
    }
    let mut outcome = run.result.as_ref().map(|r| r.0.clone()).unwrap_or_default();
    // Servers the run made that an agent took over.
    let kept: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT s.id, s.hostname FROM import_created c JOIN servers s ON s.id = c.object_id
         WHERE c.run_id = $1 AND c.object = 'server' AND NOT s.awaiting_agent",
    )
    .bind(run.id)
    .fetch_all(&mut *tx)
    .await?;
    let kept_ids: Vec<Uuid> = kept.iter().map(|k| k.0).collect();
    outcome.kept_servers = kept.into_iter().map(|k| k.1).collect();
    let mut removed = BTreeMap::new();
    for (object, sql) in [
        (
            "check",
            "DELETE FROM sensor_configs WHERE id IN
                 (SELECT object_id FROM import_created WHERE run_id = $1 AND object = 'check')
               AND NOT (server_id = ANY($2))",
        ),
        (
            "threshold",
            "DELETE FROM thresholds WHERE id IN
                 (SELECT object_id FROM import_created WHERE run_id = $1 AND object = 'threshold')
               AND NOT (server_id = ANY($2))",
        ),
        (
            "silence",
            "DELETE FROM silences WHERE id IN
                 (SELECT object_id FROM import_created WHERE run_id = $1 AND object = 'silence')
               AND (server_id IS NULL OR NOT (server_id = ANY($2)))",
        ),
        (
            "channel",
            "DELETE FROM notification_channels WHERE id IN
                 (SELECT object_id FROM import_created WHERE run_id = $1 AND object = 'channel')",
        ),
        (
            "server",
            "DELETE FROM servers WHERE id IN
                 (SELECT object_id FROM import_created WHERE run_id = $1 AND object = 'server')
               AND awaiting_agent",
        ),
    ] {
        let done = sqlx::query(sql)
            .bind(run.id)
            .bind(&kept_ids)
            .execute(&mut *tx)
            .await?;
        removed.insert(
            object.to_owned(),
            u32::try_from(done.rows_affected()).unwrap_or(u32::MAX),
        );
    }
    outcome.removed = removed;
    sqlx::query(
        "UPDATE import_runs SET state = 'undone', undone_at = now(), undone_by = $2, result = $3
         WHERE id = $1",
    )
    .bind(run.id)
    .bind(user)
    .bind(sqlx::types::Json(&outcome))
    .execute(&mut *tx)
    .await?;
    crate::api::audit::record(
        &mut *tx,
        crate::api::audit::Event {
            actor_kind: "user",
            actor_id: &user.to_string(),
            action: "import.undo",
            target: Some(("import", &run.id.to_string())),
            details: json!({
                "source": run.source,
                "site_id": run.site_id,
                "removed": outcome.removed,
                "kept_servers": outcome.kept_servers,
            }),
        },
    )
    .await?;
    let run = get(&mut tx, id, false).await?.expect("the run exists");
    tx.commit().await?;
    Ok((run, outcome))
}
