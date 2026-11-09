//! Administration › Plugins (ADR-0103). Installing, updating, placing,
//! turning off and removing plugins, and the publisher keys that may sign
//! them, are for global administrators only: a plugin runs on the central
//! server or on every agent it is given to. What may be used where is read
//! by anyone, limited to the places they see.

use std::collections::BTreeSet;

use axum::body::Bytes;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use monitorflow_plugin::keys::PublicKey;
use monitorflow_plugin::manifest::Setting;
use monitorflow_plugin::package::{self, PackageError, Verified};
use monitorflow_plugin::Manifest;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgExecutor, Row};
use uuid::Uuid;

use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::sensors::{body, parse_id};
use super::server_update::require_global_admin;
use super::{ApiError, ApiState};
use crate::plugins::{self, Place, OFFICIAL_PUBLISHER};

/// A plugin as Administration › Plugins shows it.
#[derive(Debug, Serialize)]
pub struct PluginView {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub version: String,
    pub title: String,
    pub description: String,
    pub official: bool,
    /// MonitorFlow, the publisher's name, or `null` for a key no longer
    /// trusted.
    pub publisher: Option<String>,
    pub fingerprint: String,
    /// What it does and asks for, in words.
    pub capabilities: Vec<String>,
    pub settings: Vec<Setting>,
    pub enabled: bool,
    pub places: Vec<PlaceView>,
    pub channels: i64,
    pub checks: i64,
    pub installed_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Where a plugin may be used: everything when both IDs are null.
#[derive(Debug, Clone, Serialize)]
pub struct PlaceView {
    pub customer_id: Option<Uuid>,
    pub customer_name: Option<String>,
    pub site_id: Option<Uuid>,
    pub site_name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PluginList {
    pub plugins: Vec<PluginView>,
}

/// What an uploaded file is, before it is installed.
#[derive(Debug, Serialize)]
pub struct Inspection {
    pub name: String,
    pub kind: String,
    pub version: String,
    pub title: String,
    pub description: String,
    pub official: bool,
    pub publisher: String,
    pub fingerprint: String,
    pub capabilities: Vec<String>,
    pub settings: Vec<Setting>,
    /// Hex; `POST /api/plugins` takes it back so what is installed is what
    /// was shown.
    pub sha256: String,
    /// Set when it replaces an installed version.
    pub update: Option<UpdateView>,
}

#[derive(Debug, Serialize)]
pub struct UpdateView {
    pub from_version: String,
    /// Capabilities the new version asks for and the old one did not.
    pub adds: Vec<String>,
    /// Capabilities it no longer asks for.
    pub drops: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallQuery {
    pub sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlacesRequest {
    pub places: Vec<Place>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnabledRequest {
    pub enabled: bool,
}

/// Most places of one plugin.
const MAX_PLACES: usize = 200;

fn package_error(error: PackageError) -> ApiError {
    ApiError::Invalid(match error {
        PackageError::UntrustedKey(key) => format!(
            "This plugin is signed by a key you do not trust yet (fingerprint {}). \
             If you know its publisher, add their key under Publishers first.",
            key.fingerprint()
        ),
        other => other.to_string(),
    })
}

/// Reads and verifies an uploaded file against the trusted keys.
async fn verify(state: &ApiState, file: &[u8]) -> Result<Verified, ApiError> {
    let trusted = plugins::trusted_keys(&state.pool).await?;
    let file = file.to_vec();
    tokio::task::spawn_blocking(move || package::verify_file(&file, &trusted))
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .map_err(package_error)
}

/// The installed version of `name`: its version, key and capabilities.
async fn current(
    db: impl PgExecutor<'_>,
    name: &str,
) -> Result<Option<(Uuid, Manifest, Vec<u8>)>, ApiError> {
    let row =
        sqlx::query("SELECT id, manifest, public_key FROM plugins WHERE name = $1 FOR UPDATE")
            .bind(name)
            .fetch_optional(db)
            .await?;
    let Some(row) = row else { return Ok(None) };
    let manifest = serde_json::from_value(row.try_get("manifest")?)
        .map_err(|e| ApiError::Internal(format!("stored manifest: {e}")))?;
    Ok(Some((
        row.try_get("id")?,
        manifest,
        row.try_get("public_key")?,
    )))
}

/// What an update changes, or why it cannot replace `old`.
fn update_of(old: &Manifest, old_key: &[u8], new: &Verified) -> Result<UpdateView, ApiError> {
    if new.manifest.kind != old.kind {
        return Err(ApiError::Invalid(format!(
            "{} is installed as a {}; this file is a {}. Remove it first.",
            old.name,
            old.kind.as_str(),
            new.manifest.kind.as_str()
        )));
    }
    if old_key != new.key.0 {
        return Err(ApiError::Invalid(format!(
            "{} is installed from another publisher. Remove it first to install this one.",
            old.name
        )));
    }
    if new.manifest.version() <= old.version() {
        return Err(ApiError::Invalid(format!(
            "{} {} is installed; this file has {}. An update must be newer.",
            old.name, old.version, new.manifest.version
        )));
    }
    let before: BTreeSet<String> = old.capabilities_in_words().into_iter().collect();
    let after: BTreeSet<String> = new.manifest.capabilities_in_words().into_iter().collect();
    Ok(UpdateView {
        from_version: old.version.clone(),
        adds: after.difference(&before).cloned().collect(),
        drops: before.difference(&after).cloned().collect(),
    })
}

/// `POST /api/plugins/inspect`: the plugin file as the body. Stores
/// nothing.
pub async fn inspect(
    State(state): State<ApiState>,
    user: CurrentUser,
    file: Bytes,
) -> Result<Json<Inspection>, ApiError> {
    require_global_admin(&user)?;
    let verified = verify(&state, &file).await?;
    let update = match current(&state.pool, &verified.manifest.name).await? {
        Some((_, old, key)) => Some(update_of(&old, &key, &verified)?),
        None => None,
    };
    let publisher = plugins::publisher_name(&state.pool, &verified.key)
        .await?
        .unwrap_or_default();
    let manifest = &verified.manifest;
    Ok(Json(Inspection {
        name: manifest.name.clone(),
        kind: manifest.kind.as_str().into(),
        version: manifest.version.clone(),
        title: manifest.title.clone(),
        description: manifest.description.clone(),
        official: verified.official,
        publisher,
        fingerprint: verified.key.fingerprint(),
        capabilities: manifest.capabilities_in_words(),
        settings: manifest.settings.clone(),
        sha256: package::hex(&verified.sha256),
        update,
    }))
}

/// `POST /api/plugins?sha256=…`: installs or updates the plugin of the
/// body, which must be the file inspected. A new plugin is used nowhere
/// until places are set; an update keeps them.
pub async fn install(
    State(state): State<ApiState>,
    user: CurrentUser,
    Query(query): Query<InstallQuery>,
    file: Bytes,
) -> Result<(StatusCode, Json<PluginView>), ApiError> {
    require_global_admin(&user)?;
    let verified = verify(&state, &file).await?;
    if !query
        .sha256
        .eq_ignore_ascii_case(&package::hex(&verified.sha256))
    {
        return Err(ApiError::Invalid(
            "This is not the file that was shown. Upload it again.".into(),
        ));
    }
    let manifest = &verified.manifest;
    let manifest_json =
        serde_json::to_value(manifest).map_err(|e| ApiError::Internal(e.to_string()))?;
    let mut tx = state.pool.begin().await?;
    if !verified.official {
        // Holds off a removal of the publisher until this commits.
        let trusted: Option<i32> =
            sqlx::query_scalar("SELECT 1 FROM plugin_publishers WHERE public_key = $1 FOR SHARE")
                .bind(verified.key.0.as_slice())
                .fetch_optional(&mut *tx)
                .await?;
        if trusted.is_none() {
            return Err(package_error(PackageError::UntrustedKey(verified.key)));
        }
    }
    let (id, action, from) = match current(&mut *tx, &manifest.name).await? {
        Some((id, old, key)) => {
            update_of(&old, &key, &verified)?;
            sqlx::query(
                "UPDATE plugins SET version = $2, title = $3, manifest = $4, package = $5,
                        sha256 = $6, official = $7, updated_at = now()
                 WHERE id = $1",
            )
            .bind(id)
            .bind(&manifest.version)
            .bind(&manifest.title)
            .bind(&manifest_json)
            .bind(file.as_ref())
            .bind(verified.sha256.as_slice())
            .bind(verified.official)
            .execute(&mut *tx)
            .await?;
            (id, "plugin.update", Some(old.version))
        }
        None => {
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO plugins (name, kind, version, title, manifest, package, sha256,
                                      public_key, official, installed_by)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
            )
            .bind(&manifest.name)
            .bind(manifest.kind.as_str())
            .bind(&manifest.version)
            .bind(&manifest.title)
            .bind(&manifest_json)
            .bind(file.as_ref())
            .bind(verified.sha256.as_slice())
            .bind(verified.key.0.as_slice())
            .bind(verified.official)
            .bind(user.id)
            .fetch_one(&mut *tx)
            .await?;
            (id, "plugin.install", None)
        }
    };
    let actor = user.id.to_string();
    let target = id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action,
            target: Some(("plugin", &target)),
            details: json!({
                "name": manifest.name,
                "kind": manifest.kind.as_str(),
                "version": manifest.version,
                "from_version": from,
                "official": verified.official,
                "fingerprint": verified.key.fingerprint(),
                "sha256": package::hex(&verified.sha256),
                "capabilities": manifest.capabilities_in_words(),
            }),
        },
    )
    .await?;
    tx.commit().await?;
    tracing::info!(user = %user.id, plugin = %manifest.name, version = %manifest.version, action, "plugin installed");
    let status = if from.is_some() {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((status, Json(view(&state, id).await?)))
}

/// `GET /api/plugins`.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<PluginList>, ApiError> {
    require_global_admin(&user)?;
    let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM plugins ORDER BY title, name")
        .fetch_all(&state.pool)
        .await?;
    let mut plugins = Vec::with_capacity(ids.len());
    for id in ids {
        plugins.push(view(&state, id).await?);
    }
    Ok(Json(PluginList { plugins }))
}

async fn places(db: impl PgExecutor<'_>, id: Uuid) -> Result<Vec<PlaceView>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT COALESCE(pp.customer_id, si.customer_id) AS customer_id, c.name AS customer_name,
                pp.site_id, si.name AS site_name
         FROM plugin_places pp
         LEFT JOIN sites si ON si.id = pp.site_id
         LEFT JOIN customers c ON c.id = COALESCE(pp.customer_id, si.customer_id)
         WHERE pp.plugin_id = $1
         ORDER BY c.name NULLS FIRST, si.name NULLS FIRST",
    )
    .bind(id)
    .fetch_all(db)
    .await?;
    rows.iter()
        .map(|row| {
            Ok(PlaceView {
                customer_id: row.try_get("customer_id")?,
                customer_name: row.try_get("customer_name")?,
                site_id: row.try_get("site_id")?,
                site_name: row.try_get("site_name")?,
            })
        })
        .collect()
}

async fn view(state: &ApiState, id: Uuid) -> Result<PluginView, ApiError> {
    let row = sqlx::query(
        "SELECT name, kind, version, title, manifest, public_key, official, enabled,
                installed_at, updated_at
         FROM plugins WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(ApiError::NotFound)?;
    let manifest: Manifest = serde_json::from_value(row.try_get("manifest")?)
        .map_err(|e| ApiError::Internal(format!("stored manifest: {e}")))?;
    let key: Vec<u8> = row.try_get("public_key")?;
    let key = PublicKey::from_bytes(&key).map_err(|e| ApiError::Internal(e.to_string()))?;
    let name: String = row.try_get("name")?;
    let (channels, checks) = plugins::uses(&state.pool, &name).await?;
    let official: bool = row.try_get("official")?;
    let publisher = match official {
        true => Some(OFFICIAL_PUBLISHER.to_owned()),
        false => plugins::publisher_name(&state.pool, &key).await?,
    };
    Ok(PluginView {
        id,
        name,
        kind: row.try_get("kind")?,
        version: row.try_get("version")?,
        title: row.try_get("title")?,
        description: manifest.description.clone(),
        official,
        publisher,
        fingerprint: key.fingerprint(),
        capabilities: manifest.capabilities_in_words(),
        settings: manifest.settings,
        enabled: row.try_get("enabled")?,
        places: places(&state.pool, id).await?,
        channels,
        checks,
        installed_at: row.try_get("installed_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// Locks the plugin `id` and returns its name.
async fn lock(db: impl PgExecutor<'_>, id: Uuid) -> Result<String, ApiError> {
    sqlx::query_scalar("SELECT name FROM plugins WHERE id = $1 FOR UPDATE")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(ApiError::NotFound)
}

/// `PUT /api/plugins/{id}/places`: replaces where the plugin may be used.
/// None: nowhere.
pub async fn set_places(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<PlacesRequest>, JsonRejection>,
) -> Result<Json<PluginView>, ApiError> {
    require_global_admin(&user)?;
    let id = parse_id(&id)?;
    let mut places = body(request)?.places;
    places.sort_by_key(|p| (p.customer_id, p.site_id));
    places.dedup();
    if places.len() > MAX_PLACES {
        return Err(ApiError::Invalid(format!("At most {MAX_PLACES} places.")));
    }
    if places
        .iter()
        .any(|p| p.customer_id.is_some() && p.site_id.is_some())
    {
        return Err(ApiError::Invalid(
            "A place is a customer or a site, not both.".into(),
        ));
    }
    let customers: Vec<Uuid> = places.iter().filter_map(|p| p.customer_id).collect();
    let sites: Vec<Uuid> = places.iter().filter_map(|p| p.site_id).collect();
    let mut tx = state.pool.begin().await?;
    let name = lock(&mut *tx, id).await?;
    let (found_customers, found_sites): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM customers WHERE id = ANY($1)),
                (SELECT count(*) FROM sites WHERE id = ANY($2))",
    )
    .bind(&customers)
    .bind(&sites)
    .fetch_one(&mut *tx)
    .await?;
    if found_customers as usize != customers.len() || found_sites as usize != sites.len() {
        return Err(ApiError::Invalid(
            "A customer or site does not exist.".into(),
        ));
    }
    sqlx::query("DELETE FROM plugin_places WHERE plugin_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    for place in &places {
        sqlx::query(
            "INSERT INTO plugin_places (plugin_id, customer_id, site_id) VALUES ($1, $2, $3)",
        )
        .bind(id)
        .bind(place.customer_id)
        .bind(place.site_id)
        .execute(&mut *tx)
        .await?;
    }
    let actor = user.id.to_string();
    let target = id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: "plugin.places",
            target: Some(("plugin", &target)),
            details: json!({ "name": name, "places": places }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(Json(view(&state, id).await?))
}

/// `PUT /api/plugins/{id}/enabled`: turns every use on or off at once.
pub async fn set_enabled(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<EnabledRequest>, JsonRejection>,
) -> Result<Json<PluginView>, ApiError> {
    require_global_admin(&user)?;
    let id = parse_id(&id)?;
    let enabled = body(request)?.enabled;
    let mut tx = state.pool.begin().await?;
    let name = lock(&mut *tx, id).await?;
    sqlx::query("UPDATE plugins SET enabled = $2 WHERE id = $1 AND enabled <> $2")
        .bind(id)
        .bind(enabled)
        .execute(&mut *tx)
        .await?;
    let actor = user.id.to_string();
    let target = id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: if enabled {
                "plugin.enable"
            } else {
                "plugin.disable"
            },
            target: Some(("plugin", &target)),
            details: json!({ "name": name }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(Json(view(&state, id).await?))
}

/// `DELETE /api/plugins/{id}`: refused while a channel or a check uses it.
pub async fn remove(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_global_admin(&user)?;
    let id = parse_id(&id)?;
    let mut tx = state.pool.begin().await?;
    let name = lock(&mut *tx, id).await?;
    let (channels, checks) = plugins::uses(&mut *tx, &name).await?;
    if channels + checks > 0 {
        return Err(ApiError::Conflict(format!(
            "{name} is used by {channels} channel(s) and {checks} check(s). Remove those first, \
             or turn the plugin off."
        )));
    }
    sqlx::query("DELETE FROM plugins WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let actor = user.id.to_string();
    let target = id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: "plugin.remove",
            target: Some(("plugin", &target)),
            details: json!({ "name": name }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// A plugin as those who use it see it: what it is and the places they
/// can see where it may be used.
#[derive(Debug, Serialize)]
pub struct UsableView {
    pub name: String,
    pub kind: String,
    pub version: String,
    pub title: String,
    pub description: String,
    pub capabilities: Vec<String>,
    pub settings: Vec<Setting>,
    /// May be used for everything.
    pub everywhere: bool,
    pub customer_ids: Vec<Uuid>,
    pub site_ids: Vec<Uuid>,
}

#[derive(Debug, Serialize)]
pub struct UsableList {
    pub plugins: Vec<UsableView>,
}

/// `GET /api/plugins/usable`: plugins turned on where the user can see,
/// for the channel and check forms. Places the user cannot read are left
/// out, and so is a plugin that has none left.
pub async fn usable(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<UsableList>, ApiError> {
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let sites = user.readable_sites();
    let customers = user.visible_customers();
    let rows = sqlx::query(
        "SELECT p.name, p.kind, p.version, p.title, p.manifest, pp.customer_id, pp.site_id
         FROM plugins p JOIN plugin_places pp ON pp.plugin_id = p.id
         WHERE p.enabled
         ORDER BY p.title, p.name",
    )
    .fetch_all(&state.pool)
    .await?;
    let mut plugins: Vec<UsableView> = Vec::new();
    for row in rows {
        let customer: Option<Uuid> = row.try_get("customer_id")?;
        let site: Option<Uuid> = row.try_get("site_id")?;
        let visible = match (customer, site) {
            (None, None) => true,
            (Some(c), _) => customers.contains(c),
            (None, Some(s)) => sites.contains(s),
        };
        if !visible {
            continue;
        }
        let name: String = row.try_get("name")?;
        if plugins.last().is_none_or(|p| p.name != name) {
            let manifest: Manifest = serde_json::from_value(row.try_get("manifest")?)
                .map_err(|e| ApiError::Internal(format!("stored manifest: {e}")))?;
            plugins.push(UsableView {
                name,
                kind: row.try_get("kind")?,
                version: row.try_get("version")?,
                title: row.try_get("title")?,
                description: manifest.description.clone(),
                capabilities: manifest.capabilities_in_words(),
                settings: manifest.settings,
                everywhere: false,
                customer_ids: Vec::new(),
                site_ids: Vec::new(),
            });
        }
        let view = plugins.last_mut().expect("pushed above");
        match (customer, site) {
            (None, None) => view.everywhere = true,
            (Some(c), _) => view.customer_ids.push(c),
            (None, Some(s)) => view.site_ids.push(s),
        }
    }
    Ok(Json(UsableList { plugins }))
}

/// A trusted publisher key.
#[derive(Debug, Serialize)]
pub struct PublisherView {
    pub id: Uuid,
    pub name: String,
    pub public_key: String,
    pub fingerprint: String,
    /// Installed plugins it signed.
    pub plugins: Vec<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct PublisherList {
    pub publishers: Vec<PublisherView>,
    /// Fingerprints of the catalogue keys built in; none until the
    /// project publishes one.
    pub official: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublisherRequest {
    pub name: String,
    /// The key `monitorflow-plugin keygen` printed: base64 or PEM.
    pub public_key: String,
}

async fn publisher_views(
    db: impl PgExecutor<'_>,
    id: Option<Uuid>,
) -> Result<Vec<PublisherView>, ApiError> {
    let rows = sqlx::query(
        "SELECT pb.id, pb.name, pb.public_key, pb.created_at,
                COALESCE(array_agg(p.name ORDER BY p.name) FILTER (WHERE p.name IS NOT NULL), '{}') AS plugins
         FROM plugin_publishers pb
         LEFT JOIN plugins p ON p.public_key = pb.public_key
         WHERE $1::uuid IS NULL OR pb.id = $1
         GROUP BY pb.id ORDER BY pb.name",
    )
    .bind(id)
    .fetch_all(db)
    .await?;
    rows.iter()
        .map(|row| {
            let key: Vec<u8> = row.try_get("public_key")?;
            let key = PublicKey::from_bytes(&key).map_err(|e| ApiError::Internal(e.to_string()))?;
            Ok(PublisherView {
                id: row.try_get("id")?,
                name: row.try_get("name")?,
                public_key: key.to_base64(),
                fingerprint: key.fingerprint(),
                plugins: row.try_get("plugins")?,
                created_at: row.try_get("created_at")?,
            })
        })
        .collect()
}

/// `GET /api/plugins/publishers`.
pub async fn publishers(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<PublisherList>, ApiError> {
    require_global_admin(&user)?;
    Ok(Json(PublisherList {
        publishers: publisher_views(&state.pool, None).await?,
        official: monitorflow_plugin::keys::official()
            .iter()
            .map(PublicKey::fingerprint)
            .collect(),
    }))
}

/// `POST /api/plugins/publishers`: trusts a key to sign plugins.
pub async fn add_publisher(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<PublisherRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<PublisherView>), ApiError> {
    require_global_admin(&user)?;
    let request = body(request)?;
    let name = request.name.trim();
    if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err(ApiError::Invalid("The name is 1 to 80 characters.".into()));
    }
    let key = PublicKey::parse(&request.public_key).map_err(|_| {
        ApiError::Invalid(
            "This is not a public key from monitorflow-plugin keygen: paste the line it printed."
                .into(),
        )
    })?;
    if monitorflow_plugin::keys::official().contains(&key) {
        return Err(ApiError::Conflict(
            "This is MonitorFlow's own key, trusted already.".into(),
        ));
    }
    let mut tx = state.pool.begin().await?;
    let id: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO plugin_publishers (name, public_key, created_by) VALUES ($1, $2, $3)
         ON CONFLICT (public_key) DO NOTHING RETURNING id",
    )
    .bind(name)
    .bind(key.0.as_slice())
    .bind(user.id)
    .fetch_optional(&mut *tx)
    .await?;
    let id = id.ok_or_else(|| ApiError::Conflict("This key is trusted already.".into()))?;
    let actor = user.id.to_string();
    let target = id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: "plugin_publisher.add",
            target: Some(("plugin_publisher", &target)),
            details: json!({ "name": name, "fingerprint": key.fingerprint() }),
        },
    )
    .await?;
    tx.commit().await?;
    let view = publisher_views(&state.pool, Some(id))
        .await?
        .pop()
        .ok_or(ApiError::NotFound)?;
    Ok((StatusCode::CREATED, Json(view)))
}

/// `DELETE /api/plugins/publishers/{id}`: refused while a plugin it signed
/// is installed.
pub async fn remove_publisher(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_global_admin(&user)?;
    let id = parse_id(&id)?;
    let mut tx = state.pool.begin().await?;
    let row: Option<(String, Vec<u8>)> =
        sqlx::query_as("SELECT name, public_key FROM plugin_publishers WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let (name, key) = row.ok_or(ApiError::NotFound)?;
    // Installs hold the publisher row while they write, so none with this
    // key slips in between this check and the delete.
    let signed: Vec<String> =
        sqlx::query_scalar("SELECT name FROM plugins WHERE public_key = $1 ORDER BY name")
            .bind(&key)
            .fetch_all(&mut *tx)
            .await?;
    if !signed.is_empty() {
        return Err(ApiError::Conflict(format!(
            "{name} signed {}. Remove those plugins first.",
            signed.join(", ")
        )));
    }
    sqlx::query("DELETE FROM plugin_publishers WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let fingerprint = PublicKey::from_bytes(&key)
        .map(|k| k.fingerprint())
        .unwrap_or_default();
    let actor = user.id.to_string();
    let target = id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: "plugin_publisher.remove",
            target: Some(("plugin_publisher", &target)),
            details: json!({ "name": name, "fingerprint": fingerprint }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(version: &str, http: &[&str]) -> Manifest {
        Manifest::parse(
            &json!({
                "name": "web", "version": version, "kind": "check", "title": "Web", "abi": 1,
                "capabilities": {"http": http}
            })
            .to_string(),
        )
        .unwrap()
    }

    fn verified(manifest: Manifest, key: [u8; 32]) -> Verified {
        Verified {
            manifest,
            module: Vec::new(),
            key: PublicKey(key),
            official: false,
            sha256: [0; 32],
        }
    }

    #[test]
    fn an_update_is_newer_from_the_same_signer_and_says_what_changes() {
        let old = manifest("1.0.0", &["a.example"]);
        let update = update_of(
            &old,
            &[1; 32],
            &verified(manifest("1.1.0", &["b.example"]), [1; 32]),
        )
        .unwrap();
        assert_eq!(update.from_version, "1.0.0");
        assert_eq!(update.adds, ["Connects to b.example"]);
        assert_eq!(update.drops, ["Connects to a.example"]);
        let same = update_of(&old, &[1; 32], &verified(manifest("1.0.0", &[]), [1; 32]));
        assert!(matches!(same, Err(ApiError::Invalid(m)) if m.contains("must be newer")));
        let other = update_of(&old, &[1; 32], &verified(manifest("2.0.0", &[]), [2; 32]));
        assert!(matches!(other, Err(ApiError::Invalid(m)) if m.contains("another publisher")));
    }

    #[test]
    fn untrusted_keys_say_what_to_do() {
        let error = package_error(PackageError::UntrustedKey(PublicKey([3; 32])));
        assert!(
            matches!(error, ApiError::Invalid(m) if m.contains("add their key under Publishers"))
        );
    }
}
