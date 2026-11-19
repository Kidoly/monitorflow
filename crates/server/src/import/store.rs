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
use super::{ChannelSecret, CheckRef, Create, Item, PREVIEW_HOURS};
use crate::dependencies::{self, Node};
use crate::enroll::identity::{self, Pending};
use crate::enroll::install_key::peer_id_from_hostname;
use crate::notify::maintenance::write;
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
    /// `window`, `network_scan`, `channel`.
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
    let rows: Vec<(Uuid, String, String, Uuid, bool)> = sqlx::query_as(
        "SELECT id, peer_id, hostname, site_id, kind = 'device' FROM servers
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
    for (id, peer_id, hostname, site, device) in rows {
        peer_of.insert(id, peer_id.to_ascii_lowercase());
        existing.servers.push(ExistingServer {
            peer_id,
            hostname,
            site_id: site,
            may_change: may_change(site),
            device,
        });
    }
    let checks: Vec<(Uuid, String)> = sqlx::query_as(
        // Any instance: a check run by a probe has the probe as its
        // instance (ADR-0148), and is the same check.
        "SELECT DISTINCT server_id, name FROM sensor_configs WHERE server_id = ANY($1)",
    )
    .bind(&server_ids)
    .fetch_all(pool)
    .await?;
    existing.checks = checks
        .into_iter()
        .filter_map(|(id, name)| Some((peer_of.get(&id)?.clone(), name)))
        .collect();
    let thresholds: Vec<(Uuid, String, Option<String>, String)> = sqlx::query_as(
        "SELECT server_id, metric, instance, kind FROM thresholds WHERE server_id = ANY($1)",
    )
    .bind(&server_ids)
    .fetch_all(pool)
    .await?;
    existing.thresholds = thresholds
        .into_iter()
        .filter_map(|(id, metric, instance, kind)| {
            Some((peer_of.get(&id)?.clone(), metric, instance, kind))
        })
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
    let mut conn = pool.acquire().await?;
    existing.windows = existing_windows(&mut conn, site_id).await?;
    existing.scans = existing_scans(&mut conn, site_id).await?;
    existing.probe = crate::api::sensors::default_probe(&mut *conn, site_id)
        .await?
        .key;
    existing.services = sqlx::query_scalar::<_, String>(
        "SELECT lower(b.name) FROM business_services b JOIN sites s ON s.customer_id = b.customer_id
         WHERE s.id = $1",
    )
    .bind(site_id)
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .collect();
    let channels = covering(&mut conn, site_id, None).await?;
    drop(conn);
    existing.channels.extend(
        channels
            .into_iter()
            .filter_map(|(kind, settings, sealed)| stored_key(&kind, &settings, sealed)),
    );
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
/// for someone who is not a global administrator. `may_change` says
/// which sites the person who confirms administers: a server already
/// there is used only in one of them, whoever made the preview and
/// whatever moved since.
pub async fn confirm(
    pool: &PgPool,
    id: Uuid,
    skip: &HashSet<String>,
    user: Uuid,
    channel_public_only: bool,
    now: DateTime<Utc>,
    may_change: impl Fn(Uuid) -> bool + Sync,
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
            &may_change,
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

/// The server of ID `peer_id`, not removed; an error when it is in a site
/// `may_change` refuses.
async fn server_id(
    tx: &mut PgConnection,
    peer_id: &str,
    may_change: &(dyn Fn(Uuid) -> bool + Sync),
) -> Result<Option<Uuid>, String> {
    let found: Option<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT id, site_id FROM servers WHERE lower(peer_id) = lower($1) AND revoked_at IS NULL",
    )
    .bind(peer_id)
    .fetch_optional(tx)
    .await
    .map_err(db_error)?;
    match found {
        Some((_, site)) if !may_change(site) => Err(format!(
            "the server {peer_id} is in a site you do not administer"
        )),
        found => Ok(found.map(|(id, _)| id)),
    }
}

fn db_error(error: sqlx::Error) -> String {
    match &error {
        sqlx::Error::Database(e) => e.message().to_owned(),
        _ => error.to_string(),
    }
}

/// The lookup `lookup` stands for: the site's customer's lookup of that
/// name, or a new one (ADR-0157). Lookups stay after an undo: other checks
/// may use them.
async fn imported_lookup(
    tx: &mut Transaction<'_, Postgres>,
    site_id: Uuid,
    lookup: &super::ImportedLookup,
    user: Uuid,
) -> Result<Uuid, String> {
    let definition: monitorflow_snmp::custom::Lookup =
        serde_json::from_value(lookup.definition.clone())
            .map_err(|e| format!("its lookup {} does not read: {e}", lookup.name))?;
    definition
        .validate()
        .map_err(|e| format!("its lookup {}: {e}", lookup.name))?;
    let customer: Uuid = sqlx::query_scalar("SELECT customer_id FROM sites WHERE id = $1")
        .bind(site_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(db_error)?;
    let found: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM snmp_lookups WHERE customer_id = $1 AND name = $2")
            .bind(customer)
            .bind(&lookup.name)
            .fetch_optional(&mut **tx)
            .await
            .map_err(db_error)?;
    if let Some(id) = found {
        return Ok(id);
    }
    let value = serde_json::to_value(&definition).map_err(|e| e.to_string())?;
    sqlx::query_scalar(
        "INSERT INTO snmp_lookups (customer_id, name, definition, created_by) VALUES ($1, $2, $3, $4)
         RETURNING id",
    )
    .bind(customer)
    .bind(&lookup.name)
    .bind(value)
    .bind(user)
    .fetch_one(&mut **tx)
    .await
    .map_err(db_error)
}

/// The server `peer_id` an item is on: made by this confirmation, or
/// there already in a site `may_change` allows.
async fn server_of(
    tx: &mut PgConnection,
    peer_id: &str,
    servers: &HashMap<String, Uuid>,
    may_change: &(dyn Fn(Uuid) -> bool + Sync),
) -> Result<Uuid, String> {
    match servers.get(&peer_id.to_ascii_lowercase()) {
        Some(id) => Ok(*id),
        None => server_id(tx, peer_id, may_change)
            .await?
            .ok_or_else(|| "its server is not there".to_owned()),
    }
}

/// Creates one object; `None` when it exists already.
#[allow(clippy::too_many_arguments)]
async fn create_one(
    tx: &mut Transaction<'_, Postgres>,
    site_id: Uuid,
    create: &Create,
    secret: Option<&ChannelSecret>,
    servers: &mut HashMap<String, Uuid>,
    user: Uuid,
    public_only: bool,
    may_change: &(dyn Fn(Uuid) -> bool + Sync),
) -> Result<Option<Uuid>, String> {
    match create {
        Create::Server {
            peer_id,
            hostname,
            os_family,
            ..
        } => {
            identity::lock(tx).await.map_err(db_error)?;
            if let Some(id) = server_id(tx, peer_id, may_change).await? {
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
        Create::Device {
            peer_id,
            hostname,
            address,
        } => {
            if let Some(id) = server_id(tx, peer_id, may_change).await? {
                servers.insert(peer_id.to_ascii_lowercase(), id);
                return Ok(None);
            }
            let address = crate::api::devices::check_address(address)
                .map_err(|_| "its address is not valid")?;
            let (id, made) = crate::api::devices::insert(tx, site_id, hostname, &address)
                .await
                .map_err(|e| e.to_string())?;
            servers.insert(peer_id.to_ascii_lowercase(), id);
            servers.insert(made.to_ascii_lowercase(), id);
            Ok(Some(id))
        }
        Create::Check {
            server: peer_id,
            kind,
            name,
            interval_ms,
            timeout_ms,
            params,
            probe,
            lookups,
        } => {
            let id = server_of(tx, peer_id, servers, may_change).await?;
            // Never a secret: an imported website check has no password
            // (the person sets it after the import).
            // A probe check runs from the site's default probe (ADR-0148).
            let by = if *probe {
                crate::api::sensors::default_probe(&mut **tx, site_id)
                    .await
                    .map_err(db_error)?
            } else {
                crate::api::sensors::RunBy::default()
            };
            let address: Option<String> =
                sqlx::query_scalar("SELECT address FROM servers WHERE id = $1")
                    .bind(id)
                    .fetch_one(&mut **tx)
                    .await
                    .map_err(db_error)?;
            // A custom SNMP check's lookups: the customer's of the same
            // name, or new ones (ADR-0157).
            let mut params = params.clone();
            for lookup in lookups {
                let id = imported_lookup(tx, site_id, lookup, user).await?;
                params.insert(
                    format!("lookup{}", lookup.channel),
                    Value::String(id.to_string()),
                );
            }
            let valid = crate::api::sensors::validate_with_host(
                crate::api::sensors::EntryRequest {
                    kind: kind.clone(),
                    name: Some(name.clone()),
                    instance: String::new(),
                    interval_ms: *interval_ms,
                    timeout_ms: *timeout_ms,
                    params,
                    enabled: true,
                    probe: None,
                },
                address.as_deref(),
            )?;
            sqlx::query_scalar(
                "INSERT INTO sensor_configs (server_id, kind, name, instance, interval_ms, timeout_ms,
                                             params, probe, probe_relay_id)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                 ON CONFLICT ON CONSTRAINT sensor_configs_key DO NOTHING RETURNING id",
            )
            .bind(id)
            .bind(&valid.kind)
            .bind(&valid.name)
            .bind(if *probe { by.key.as_str() } else { "" })
            .bind(valid.interval_ms)
            .bind(valid.timeout_ms)
            .bind(&valid.params)
            .bind(by.probe)
            .bind(by.relay)
            .fetch_optional(&mut **tx)
            .await
            .map_err(db_error)
        }
        Create::Dependency {
            server: peer_id,
            check,
            parent,
            parent_check,
        } => {
            let child_id = server_of(tx, peer_id, servers, may_change).await?;
            let parent_id = server_of(tx, parent, servers, may_change).await?;
            let node = |id: Uuid, check: &Option<CheckRef>| match check {
                Some(c) => Node::Check(id, c.name.clone(), c.instance.clone()),
                None => Node::Server(id),
            };
            let (child, parent) = (node(child_id, check), node(parent_id, parent_check));
            let edges = dependencies::store::edges(tx).await.map_err(db_error)?;
            if dependencies::graph::cycle(&edges, &[(child.clone(), parent.clone())]).is_some() {
                return Err("it would put something behind itself".into());
            }
            sqlx::query_scalar(
                "INSERT INTO dependencies (server_id, check_name, check_instance, parent_server_id,
                                           parent_check_name, parent_check_instance, created_by)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)
                 ON CONFLICT ON CONSTRAINT dependencies_key DO NOTHING RETURNING id",
            )
            .bind(child_id)
            .bind(check.as_ref().map(|c| c.name.as_str()))
            .bind(check.as_ref().map_or("", |c| c.instance.as_str()))
            .bind(parent_id)
            .bind(parent_check.as_ref().map(|c| c.name.as_str()))
            .bind(parent_check.as_ref().map_or("", |c| c.instance.as_str()))
            .bind(user)
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
            richer,
        } => {
            let id = server_of(tx, peer_id, servers, may_change).await?;
            let valid =
                crate::api::thresholds::validate_entry(crate::api::thresholds::EntryRequest {
                    metric: metric.clone(),
                    instance: instance.clone(),
                    comparison: Some(comparison.clone()),
                    warning: *warning,
                    critical: *critical,
                    duration_s: *duration_s,
                    hysteresis: *hysteresis,
                    ..super::plan::entry_fields(richer)
                })?;
            sqlx::query_scalar(
                "INSERT INTO thresholds (server_id, metric, instance, comparison, warning,
                                         critical, duration_s, hysteresis, kind, function,
                                         window_s, offset_s, target, conditions)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
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
            .bind(valid.kind.as_str())
            .bind(valid.function.map(|f| f.as_str()))
            .bind(valid.window_s)
            .bind(valid.offset_s)
            .bind(valid.target)
            .bind(serde_json::json!(valid.conditions))
            .fetch_optional(&mut **tx)
            .await
            .map_err(db_error)
        }
        Create::Silence {
            server: peer_id,
            starts_at,
            ends_at,
            reason,
            checks,
        } => {
            let server_id = match peer_id {
                Some(p) => Some(server_of(tx, p, servers, may_change).await?),
                None => None,
            };
            let site = server_id.is_none().then_some(site_id);
            let checks_json = (!checks.is_empty()).then(|| sqlx::types::Json(checks.clone()));
            let starts_at = (*starts_at).max(Utc::now());
            if *ends_at <= starts_at {
                return Err("the window is over".into());
            }
            let same: Option<Uuid> = sqlx::query_scalar(
                "SELECT id FROM silences
                 WHERE server_id IS NOT DISTINCT FROM $1 AND site_id IS NOT DISTINCT FROM $2
                   AND relay_id IS NULL AND customer_id IS NULL AND ended_at IS NULL
                   AND window_id IS NULL AND checks IS NOT DISTINCT FROM $5
                   AND ends_at = $3 AND reason = $4 LIMIT 1",
            )
            .bind(server_id)
            .bind(site)
            .bind(ends_at)
            .bind(reason)
            .bind(&checks_json)
            .fetch_optional(&mut **tx)
            .await
            .map_err(db_error)?;
            if same.is_some() {
                return Ok(None);
            }
            sqlx::query_scalar(
                "INSERT INTO silences (server_id, site_id, starts_at, ends_at, reason, created_by,
                                       checks)
                 VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
            )
            .bind(server_id)
            .bind(site)
            .bind(starts_at)
            .bind(ends_at)
            .bind(reason)
            .bind(user)
            .bind(&checks_json)
            .fetch_one(&mut **tx)
            .await
            .map(Some)
            .map_err(db_error)
        }
        Create::Window { servers: peers, .. } => {
            let mut ids = Vec::new();
            for peer in peers {
                ids.push(server_of(tx, peer, servers, may_change).await?);
            }
            create_window(tx, site_id, create, &ids, user).await
        }
        Create::NetworkScan { .. } => create_scan(tx, site_id, create, secret, user).await,
        Create::Service {
            name,
            at_least,
            target,
            members,
        } => {
            let mut ids = Vec::new();
            for member in members {
                ids.push(server_of(tx, &member.server, servers, may_change).await?);
            }
            create_service(tx, site_id, name, *at_least, *target, members, &ids, user).await
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
            if channel_exists(tx, site_id, &destination, secret.as_deref())
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

/// A business service of the site's customer, with `members` as the
/// servers `ids`; `None` when the customer has one of that name.
#[allow(clippy::too_many_arguments)]
async fn create_service(
    tx: &mut Transaction<'_, Postgres>,
    site_id: Uuid,
    name: &str,
    at_least: Option<u32>,
    target: Option<f64>,
    members: &[super::ServiceMember],
    ids: &[Uuid],
    user: Uuid,
) -> Result<Option<Uuid>, String> {
    let id: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO business_services (customer_id, name, rule, at_least, target, created_by)
         SELECT customer_id, $2, $3, $4, $5, $6 FROM sites WHERE id = $1
         ON CONFLICT ON CONSTRAINT business_services_name_key DO NOTHING
         RETURNING id",
    )
    .bind(site_id)
    .bind(name)
    .bind(if at_least.is_some() {
        "at_least"
    } else {
        "all"
    })
    .bind(at_least.and_then(|n| i32::try_from(n).ok()))
    .bind(target)
    .bind(user)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_error)?;
    let Some(id) = id else {
        return Ok(None);
    };
    for (position, (member, server)) in members.iter().zip(ids).enumerate() {
        sqlx::query(
            "INSERT INTO business_service_members
                 (service_id, server_id, check_name, check_instance, position)
             VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .bind(server)
        .bind(member.check.as_ref().map(|c| &c.name))
        .bind(member.check.as_ref().map_or("", |c| c.instance.as_str()))
        .bind(i32::try_from(position).unwrap_or(0))
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    }
    Ok(Some(id))
}

/// A maintenance window of the site with the servers `ids`, and its
/// occurrences; `None` when the same one is there (ADR-0151).
async fn create_window(
    tx: &mut Transaction<'_, Postgres>,
    site_id: Uuid,
    create: &Create,
    ids: &[Uuid],
    user: Uuid,
) -> Result<Option<Uuid>, String> {
    let Create::Window {
        checks,
        repeat,
        every,
        weekdays,
        month_day,
        month_week,
        months,
        start_time,
        duration_minutes,
        time_zone,
        starts_on,
        ends_on,
        reason,
        ..
    } = create
    else {
        return Err("not a window".into());
    };
    if let Some(key) = super::plan::window_key(create) {
        let existing = existing_windows(tx, site_id).await.map_err(db_error)?;
        if existing.contains(&key) {
            return Ok(None);
        }
    }
    let index = |names: &[&str], value: &str| names.iter().position(|n| *n == value);
    let weekdays: i16 = weekdays
        .iter()
        .filter_map(|d| index(&WEEKDAYS, d))
        .map(|i| 1i16 << i)
        .sum();
    let months: i16 = match months.iter().map(|m| 1i16 << (m - 1)).sum() {
        0 => 0xfff,
        m => m,
    };
    let start =
        chrono::NaiveTime::parse_from_str(start_time, "%H:%M").map_err(|e| e.to_string())?;
    let starts_on = starts_on.unwrap_or_else(|| Utc::now().date_naive());
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO maintenance_windows (site_id, checks, repeat, every, weekdays, month_day,
                                          month_week, months, start_time, duration_minutes,
                                          time_zone, starts_on, ends_on, reason, created_by,
                                          whole_site)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)
         RETURNING id",
    )
    .bind(site_id)
    .bind((!checks.is_empty()).then(|| sqlx::types::Json(checks.clone())))
    .bind(repeat)
    .bind(i16::try_from(*every).unwrap_or(1))
    .bind(weekdays)
    .bind(month_day.map(i16::from))
    .bind(month_week.map(i16::from))
    .bind(months)
    .bind(start)
    .bind(i32::try_from(*duration_minutes).unwrap_or(1))
    .bind(time_zone)
    .bind(starts_on)
    .bind(ends_on)
    .bind(reason)
    .bind(user)
    .bind(ids.is_empty())
    .fetch_one(&mut **tx)
    .await
    .map_err(db_error)?;
    sqlx::query(
        "INSERT INTO maintenance_window_servers (window_id, server_id)
         SELECT $1, unnest($2::uuid[])",
    )
    .bind(id)
    .bind(ids)
    .execute(&mut **tx)
    .await
    .map_err(db_error)?;
    let stored = write::load(tx, id)
        .await
        .map_err(db_error)?
        .ok_or("the window is gone")?;
    write::write(tx, &stored, Utc::now())
        .await
        .map_err(db_error)?;
    Ok(Some(id))
}

/// The lowercase names of the site's network scans.
async fn existing_scans(
    db: &mut PgConnection,
    site_id: Uuid,
) -> Result<HashSet<String>, sqlx::Error> {
    let names: Vec<String> =
        sqlx::query_scalar("SELECT lower(name) FROM network_scans WHERE site_id = $1")
            .bind(site_id)
            .fetch_all(db)
            .await?;
    Ok(names.into_iter().collect())
}

/// A network scan of the site from its default probe, notifications off
/// (ADR-0154); `None` when one of that name is there. Checked as the
/// network scans API checks one; it runs on its schedule, not now.
async fn create_scan(
    tx: &mut Transaction<'_, Postgres>,
    site_id: Uuid,
    create: &Create,
    secret: Option<&ChannelSecret>,
    user: Uuid,
) -> Result<Option<Uuid>, String> {
    use crate::api::network_scans::MAX_PER_SITE;
    use crate::netscan::targets;
    let Create::NetworkScan {
        name,
        targets: entries,
        ports,
        every,
        ..
    } = create
    else {
        return Err("not a network scan".into());
    };
    sqlx::query("SELECT 1 FROM sites WHERE id = $1 FOR UPDATE")
        .bind(site_id)
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    if existing_scans(tx, site_id)
        .await
        .map_err(db_error)?
        .contains(&name.to_lowercase())
    {
        return Ok(None);
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM network_scans WHERE site_id = $1")
        .bind(site_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(db_error)?;
    if count >= MAX_PER_SITE {
        return Err(format!("a site has at most {MAX_PER_SITE} scans"));
    }
    let (entries, _) = targets::expand(entries)?;
    let ports = targets::ports(Some(ports))?;
    if every
        .as_deref()
        .is_some_and(|e| !matches!(e, "hour" | "day" | "week"))
    {
        return Err("its schedule is not hour, day or week".into());
    }
    let sealed = match secret.and_then(|s| s.secret.as_deref()) {
        Some(json) => {
            let request: crate::snmp::CredentialsRequest =
                serde_json::from_str(json).map_err(|_| "its SNMP credentials do not read")?;
            let credentials = crate::snmp::check(&request, None)?;
            Some(crate::netscan::seal(&credentials)?)
        }
        None => None,
    };
    let by = crate::api::sensors::default_probe(&mut **tx, site_id)
        .await
        .map_err(db_error)?;
    sqlx::query_scalar(
        "INSERT INTO network_scans (site_id, name, targets, ports, probe, probe_relay_id, every,
                                    notify, snmp_secret, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, false, $8, $9) RETURNING id",
    )
    .bind(site_id)
    .bind(name)
    .bind(&entries)
    .bind(ports.iter().map(|p| i32::from(*p)).collect::<Vec<_>>())
    .bind(by.probe)
    .bind(by.relay)
    .bind(every)
    .bind(&sealed)
    .bind(user)
    .fetch_one(&mut **tx)
    .await
    .map(Some)
    .map_err(db_error)
}

const WEEKDAYS: [&str; 7] = [
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];

/// The [`window_key`](super::plan::window_key)s of the site's windows not
/// removed.
async fn existing_windows(
    db: &mut PgConnection,
    site_id: Uuid,
) -> Result<HashSet<String>, sqlx::Error> {
    // SQL-safe: interpolates only the constant SELECT.
    let rows: Vec<write::Stored> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{} WHERE w.site_id = $1 AND w.removed_at IS NULL",
        write::SELECT
    )))
    .bind(site_id)
    .fetch_all(&mut *db)
    .await?;
    let mut keys = HashSet::new();
    for row in rows {
        // A window whose servers were all removed covers nothing.
        if !row.whole_site && row.servers.is_empty() {
            continue;
        }
        let peers: Vec<String> =
            sqlx::query_scalar("SELECT peer_id FROM servers WHERE id = ANY($1)")
                .bind(&row.servers)
                .fetch_all(&mut *db)
                .await?;
        let bits = |value: i16, names: &[&str]| -> Vec<String> {
            names
                .iter()
                .enumerate()
                .filter(|(i, _)| value & (1 << i) != 0)
                .map(|(_, n)| (*n).to_owned())
                .collect()
        };
        let create = Create::Window {
            servers: peers,
            checks: row
                .checks
                .as_ref()
                .and_then(|c| serde_json::from_value(c.clone()).ok())
                .unwrap_or_default(),
            repeat: row.repeat.clone(),
            every: u16::try_from(row.every).unwrap_or(1),
            weekdays: bits(row.weekdays, &WEEKDAYS),
            month_day: row.month_day.and_then(|v| u8::try_from(v).ok()),
            month_week: row.month_week.and_then(|v| u8::try_from(v).ok()),
            months: (1..=12u8)
                .filter(|m| row.months & (1 << (m - 1)) != 0)
                .collect(),
            start_time: row.start_time.format("%H:%M").to_string(),
            duration_minutes: u32::try_from(row.duration_minutes).unwrap_or(0),
            time_zone: row.time_zone.clone(),
            starts_on: None,
            ends_on: None,
            reason: row.reason.clone(),
        };
        keys.extend(super::plan::window_key(&create));
    }
    Ok(keys)
}

/// `(kind, settings, sealed secret)` of the channels whose messages cover
/// `site_id`, of `kind` only when given: those of every event, of the
/// site's customer and of the site. Another customer's channel to the same
/// destination is not this site's, and saying it exists would tell its
/// destination.
async fn covering(
    db: &mut PgConnection,
    site_id: Uuid,
    kind: Option<&str>,
) -> Result<Vec<(String, Value, Option<String>)>, sqlx::Error> {
    let customer: Option<Uuid> = sqlx::query_scalar("SELECT customer_id FROM sites WHERE id = $1")
        .bind(site_id)
        .fetch_optional(&mut *db)
        .await?;
    #[allow(clippy::type_complexity)]
    let rows: Vec<(String, Value, Option<String>, Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
        "SELECT kind, settings, secret, customer_id, site_id FROM notification_channels
         WHERE $1::text IS NULL OR kind = $1",
    )
    .bind(kind)
    .fetch_all(&mut *db)
    .await?;
    Ok(rows
        .into_iter()
        .filter(|(_, _, _, customer_id, site)| {
            Coverage::from_columns(*customer_id, *site)
                .is_some_and(|c| c.includes(customer, Some(site_id)))
        })
        .map(|(kind, settings, secret, _, _)| (kind, settings, secret))
        .collect())
}

/// Whether a channel of `site_id` with the same destination exists, so a
/// second import adds none. The same comparison as the preview's.
async fn channel_exists(
    tx: &mut PgConnection,
    site_id: Uuid,
    destination: &Destination,
    secret: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let kind = destination.kind();
    let Some(wanted) = destination_key(kind, &destination.settings(), secret) else {
        return Ok(false);
    };
    let rows = covering(tx, site_id, Some(kind)).await?;
    Ok(rows.into_iter().any(|(kind, settings, sealed)| {
        stored_key(&kind, &settings, sealed) == Some(wanted.clone())
    }))
}

/// The [`channel_key`] of a destination: an email channel by its server
/// and recipients, a webhook by its URL, a chat by its secret URL.
fn destination_key(kind: &str, settings: &Value, secret: Option<&str>) -> Option<String> {
    match kind {
        "email" => {
            let mut to: Vec<&str> = settings["to"]
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            to.sort_unstable();
            let host = settings["host"].as_str().unwrap_or_default();
            Some(channel_key("email", &format!("{host}|{}", to.join(","))))
        }
        "webhook" => settings["url"].as_str().map(|u| channel_key("webhook", u)),
        "slack" | "teams" | "discord" => secret.map(|url| channel_key(kind, url)),
        _ => None,
    }
}

/// The [`channel_key`] of a stored channel; a chat's secret is opened
/// only for this.
fn stored_key(kind: &str, settings: &Value, sealed: Option<String>) -> Option<String> {
    let opened = match kind {
        "slack" | "teams" | "discord" => {
            secrets::open_stored(Column::NotificationChannels, sealed).and_then(|o| o.ok())
        }
        _ => None,
    };
    destination_key(kind, settings, opened.as_ref().map(|s| s.expose()))
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
         WHERE c.run_id = $1 AND c.object = 'server' AND NOT s.awaiting_agent
           AND s.kind = 'agent'",
    )
    .bind(run.id)
    .fetch_all(&mut *tx)
    .await?;
    let kept_ids: Vec<Uuid> = kept.iter().map(|k| k.0).collect();
    outcome.kept_servers = kept.into_iter().map(|k| k.1).collect();
    let mut removed = BTreeMap::new();
    for (object, sql) in [
        (
            "dependency",
            "DELETE FROM dependencies WHERE id IN
                 (SELECT object_id FROM import_created WHERE run_id = $1 AND object = 'dependency')",
        ),
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
            "window",
            "DELETE FROM maintenance_windows WHERE id IN
                 (SELECT object_id FROM import_created WHERE run_id = $1 AND object = 'window')",
        ),
        (
            "network_scan",
            "DELETE FROM network_scans WHERE id IN
                 (SELECT object_id FROM import_created WHERE run_id = $1 AND object = 'network_scan')",
        ),
        (
            "service",
            "DELETE FROM business_services WHERE id IN
                 (SELECT object_id FROM import_created WHERE run_id = $1 AND object = 'service')",
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
               AND (awaiting_agent OR kind = 'device')",
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The preview and the confirmation compare channels the same way:
    /// recipients in any order, addresses in any case.
    #[test]
    fn destinations_compare_as_the_preview_does() {
        let email = |to: Value| json!({"host": "smtp.example.com", "to": to});
        assert_eq!(
            destination_key(
                "email",
                &email(json!(["b@example.com", "a@example.com"])),
                None
            ),
            destination_key(
                "email",
                &email(json!(["a@example.com", "b@example.com"])),
                None
            ),
        );
        assert_ne!(
            destination_key("email", &email(json!(["a@example.com"])), None),
            destination_key("email", &email(json!(["b@example.com"])), None),
        );
        assert_eq!(
            destination_key(
                "webhook",
                &json!({"url": "https://Hooks.example.com/x"}),
                None
            ),
            Some(channel_key("webhook", "https://hooks.example.com/x")),
        );
        assert_eq!(
            destination_key("slack", &json!({}), Some("https://hooks.slack.com/T/B/x")),
            Some(channel_key("slack", "https://hooks.slack.com/T/B/x")),
        );
        assert_eq!(destination_key("slack", &json!({}), None), None);
        assert_eq!(destination_key("plugin", &json!({}), Some("x")), None);
    }

    #[test]
    fn a_stored_chat_is_compared_by_its_opened_secret() {
        secrets::install_for_tests();
        let url = "https://hooks.slack.com/T/B/x";
        let sealed = secrets::seal(Column::NotificationChannels, url).unwrap();
        assert_eq!(
            stored_key("slack", &json!({"name": "Ops"}), Some(sealed)),
            destination_key("slack", &json!({}), Some(url)),
        );
        assert_eq!(stored_key("teams", &json!({}), None), None);
        let webhook = json!({"url": "https://hooks.example.com/x"});
        assert_eq!(
            stored_key("webhook", &webhook, None),
            destination_key("webhook", &webhook, None)
        );
    }
}
