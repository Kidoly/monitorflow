//! The Settings page's own settings (ADR-0069): session lifetimes, the
//! mirror address and how long the stores keep data. The configuration
//! locks a value; everything here is for global administrators and audited.

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use chrono::Utc;
use monitorflow_common::config::{
    valid_mirror_url, DEFAULT_SESSION_IDLE_TIMEOUT, DEFAULT_SESSION_LIFETIME,
};
use monitorflow_common::server_update::retention::{
    valid_days, Kept, RetentionRequest, DEFAULT_LOGS, DEFAULT_METRICS, MAX_DAYS,
    RETENTION_REQUEST_FILE,
};
use monitorflow_common::server_update::{write_atomic, Status};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::sensors::body;
use super::server_update::{alive, read_state, require_global_admin, INBOX};
use super::{ApiError, ApiState};
use crate::live_settings::{stored, Locks};

/// Shortest and longest session in the web interface: signing out within
/// minutes could lock administrators out of this very page.
const LIFETIME_RANGE: std::ops::RangeInclusive<u64> = 3600..=30 * 24 * 3600;
const IDLE_RANGE: std::ops::RangeInclusive<u64> = 300..=24 * 3600;

/// A setting of the page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Setting<T> {
    /// The value in force.
    pub value: T,
    pub default: T,
    /// The configuration sets it; the page cannot change it.
    pub locked: bool,
}

/// How long one store keeps data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Stores {
    pub metrics: Kept,
    pub logs: Kept,
    pub default_metrics: &'static str,
    pub default_logs: &'static str,
    pub max_days: u32,
    /// A change can be made: the updater runs.
    pub updater_alive: bool,
    /// A change waits for the updater or is being applied.
    pub applying: bool,
    /// Why the last change failed.
    pub error: Option<String>,
}

/// Body of `GET` and `PUT /api/settings/server`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServerSettings {
    /// Seconds.
    pub session_lifetime: Setting<u64>,
    pub session_idle_timeout: Setting<u64>,
    pub mirror_public_url: Setting<Option<String>>,
    /// `null` without an updater that reports it, as outside the compose
    /// deployment: the stores' flags are then not this server's to set.
    pub retention: Option<Stores>,
}

fn sessions_view(locks: &Locks, stored: &crate::live_settings::Stored) -> [Setting<u64>; 2] {
    let setting = |locked: Option<u64>, stored: Option<i32>, default: u64| Setting {
        value: locked
            .or(stored.and_then(|s| u64::try_from(s).ok()))
            .unwrap_or(default),
        default,
        locked: locked.is_some(),
    };
    [
        setting(
            locks.session_lifetime,
            stored.session_lifetime,
            DEFAULT_SESSION_LIFETIME,
        ),
        setting(
            locks.session_idle_timeout,
            stored.session_idle_timeout,
            DEFAULT_SESSION_IDLE_TIMEOUT,
        ),
    ]
}

fn stores_view(status: Option<Status>, requested: bool) -> Option<Stores> {
    let status = status?;
    let updater_alive = alive(&status);
    let retention = status.retention?;
    Some(Stores {
        metrics: retention.metrics,
        logs: retention.logs,
        default_metrics: DEFAULT_METRICS,
        default_logs: DEFAULT_LOGS,
        max_days: MAX_DAYS,
        updater_alive,
        applying: requested || retention.applying,
        error: retention.error,
    })
}

async fn read_stores(state: &ApiState) -> Result<Option<Stores>, ApiError> {
    let dir = state.settings.server_update.dir.clone();
    let requested = dir
        .as_ref()
        .is_some_and(|d| d.join(INBOX).join(RETENTION_REQUEST_FILE).exists());
    let (status, _) = read_state(dir).await?;
    Ok(stores_view(status, requested))
}

async fn view(state: &ApiState) -> Result<ServerSettings, ApiError> {
    let live = &state.settings.live;
    let locks = live.locks();
    let row = stored(&state.pool).await?;
    let [session_lifetime, session_idle_timeout] = sessions_view(locks, &row);
    Ok(ServerSettings {
        session_lifetime,
        session_idle_timeout,
        mirror_public_url: Setting {
            value: locks
                .mirror_public_url
                .clone()
                .or(row.mirror_public_url)
                .or_else(|| locks.mirror_default_url.clone()),
            default: locks.mirror_default_url.clone(),
            locked: locks.mirror_public_url.is_some(),
        },
        retention: read_stores(state).await?,
    })
}

/// `GET /api/settings/server`.
pub async fn get(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<ServerSettings>, ApiError> {
    require_global_admin(&user)?;
    Ok(Json(view(&state).await?))
}

/// Body of `PUT /api/settings/server`: replaces all three; `null` is the
/// default.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetRequest {
    pub session_lifetime: Option<u64>,
    pub session_idle_timeout: Option<u64>,
    pub mirror_public_url: Option<String>,
}

/// 409 when `requested` would change what the configuration sets.
fn check_lock<T: PartialEq>(
    locked: Option<&T>,
    requested: Option<&T>,
    name: &str,
) -> Result<(), ApiError> {
    match (locked, requested) {
        (Some(locked), Some(requested)) if locked != requested => Err(ApiError::Conflict(format!(
            "{name} is set by the server's configuration"
        ))),
        _ => Ok(()),
    }
}

/// What `PUT` stores: both lifetimes and the mirror URL.
type Columns = (Option<i32>, Option<i32>, Option<String>);

/// `request` checked and as stored, with what the configuration locks left
/// as `old` has it. The default is stored as NULL, so it follows new
/// releases.
fn validated(
    locks: &Locks,
    old: &crate::live_settings::Stored,
    request: SetRequest,
) -> Result<Columns, ApiError> {
    let url = request
        .mirror_public_url
        .map(|url| url.trim().trim_end_matches('/').to_owned())
        .filter(|url| !url.is_empty());
    if url.as_deref().is_some_and(|url| !valid_mirror_url(url)) {
        return Err(ApiError::Invalid(
            "the mirror address must be an http or https URL without user, query or fragment"
                .into(),
        ));
    }
    let locked_url = locks
        .mirror_public_url
        .as_deref()
        .map(|u| u.trim_end_matches('/'));
    check_lock(
        locked_url.as_ref(),
        url.as_deref().as_ref(),
        "the mirror address",
    )?;
    check_lock(
        locks.session_lifetime.as_ref(),
        request.session_lifetime.as_ref(),
        "the session lifetime",
    )?;
    check_lock(
        locks.session_idle_timeout.as_ref(),
        request.session_idle_timeout.as_ref(),
        "the idle timeout",
    )?;
    let in_range =
        |value: Option<u64>, range: std::ops::RangeInclusive<u64>, name: &str| match value {
            Some(value) if !range.contains(&value) => Err(ApiError::Invalid(format!(
                "{name} must be between {} and {} seconds",
                range.start(),
                range.end()
            ))),
            _ => Ok(()),
        };
    if locks.session_lifetime.is_none() {
        in_range(
            request.session_lifetime,
            LIFETIME_RANGE,
            "the session lifetime",
        )?;
    }
    if locks.session_idle_timeout.is_none() {
        in_range(request.session_idle_timeout, IDLE_RANGE, "the idle timeout")?;
    }
    let lifetime = locks
        .session_lifetime
        .or(request.session_lifetime)
        .unwrap_or(DEFAULT_SESSION_LIFETIME);
    let idle = locks
        .session_idle_timeout
        .or(request.session_idle_timeout)
        .unwrap_or(DEFAULT_SESSION_IDLE_TIMEOUT);
    if idle > lifetime {
        return Err(ApiError::Invalid(
            "the idle timeout cannot be longer than the session lifetime".into(),
        ));
    }
    let stored = |locked: bool, old: Option<i32>, new: Option<u64>, default| {
        if locked {
            old
        } else {
            new.filter(|new| *new != default)
                .and_then(|new| i32::try_from(new).ok())
        }
    };
    Ok((
        stored(
            locks.session_lifetime.is_some(),
            old.session_lifetime,
            request.session_lifetime,
            DEFAULT_SESSION_LIFETIME,
        ),
        stored(
            locks.session_idle_timeout.is_some(),
            old.session_idle_timeout,
            request.session_idle_timeout,
            DEFAULT_SESSION_IDLE_TIMEOUT,
        ),
        if locks.mirror_public_url.is_some() {
            old.mirror_public_url.clone()
        } else {
            url.filter(|url| Some(url) != locks.mirror_default_url.as_ref())
        },
    ))
}

/// `PUT /api/settings/server`: audited as `settings.server`. Sessions
/// already open follow a shorter lifetime at once; a longer one applies
/// from the next login.
pub async fn set(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<SetRequest>, JsonRejection>,
) -> Result<Json<ServerSettings>, ApiError> {
    require_global_admin(&user)?;
    let request = body(request)?;
    let live = &state.settings.live;
    let mut tx = state.pool.begin().await?;
    sqlx::query("INSERT INTO server_settings (singleton) VALUES (true) ON CONFLICT DO NOTHING")
        .execute(&mut *tx)
        .await?;
    // Locks the row, so concurrent changes audit the right old values.
    sqlx::query("SELECT 1 FROM server_settings FOR UPDATE")
        .execute(&mut *tx)
        .await?;
    let old = stored(&mut *tx).await?;
    let (lifetime, idle, url) = validated(live.locks(), &old, request)?;
    sqlx::query(
        "UPDATE server_settings
         SET session_lifetime = $1, session_idle_timeout = $2, mirror_public_url = $3",
    )
    .bind(lifetime)
    .bind(idle)
    .bind(&url)
    .execute(&mut *tx)
    .await?;
    let new_lifetime = live
        .locks()
        .session_lifetime
        .or(lifetime.and_then(|s| u64::try_from(s).ok()))
        .unwrap_or(DEFAULT_SESSION_LIFETIME);
    sqlx::query(
        "UPDATE sessions SET expires_at = created_at + make_interval(secs => $1)
         WHERE expires_at > created_at + make_interval(secs => $1)",
    )
    .bind(new_lifetime as f64)
    .execute(&mut *tx)
    .await?;
    let actor = user.id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: "settings.server",
            target: Some(("settings", "server")),
            details: json!({
                "old": {
                    "session_lifetime": old.session_lifetime,
                    "session_idle_timeout": old.session_idle_timeout,
                    "mirror_public_url": old.mirror_public_url,
                },
                "new": {
                    "session_lifetime": lifetime,
                    "session_idle_timeout": idle,
                    "mirror_public_url": url,
                },
            }),
        },
    )
    .await?;
    tx.commit().await?;
    live.refresh(&state.pool).await?;
    tracing::info!(user = %user.id, "changed the server settings");
    Ok(Json(view(&state).await?))
}

/// Body of `PUT /api/settings/retention`, in days; `null` leaves a store
/// alone.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetentionBody {
    pub metrics_days: Option<u32>,
    pub logs_days: Option<u32>,
}

/// `PUT /api/settings/retention`: 202, the updater applies it (ADR-0069);
/// 409 without a running updater, while another change runs or against
/// `.env`; audited as `settings.retention`.
pub async fn set_retention(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<RetentionBody>, JsonRejection>,
) -> Result<(StatusCode, Json<ServerSettings>), ApiError> {
    require_global_admin(&user)?;
    let request = body(request)?;
    let days = [request.metrics_days, request.logs_days];
    if days.into_iter().flatten().any(|days| !valid_days(days)) {
        return Err(ApiError::Invalid(format!(
            "retention must be between 1 and {MAX_DAYS} days"
        )));
    }
    let Some(stores) = read_stores(&state).await? else {
        return Err(ApiError::Conflict(
            "this server has no updater service, which applies retention".into(),
        ));
    };
    if !stores.updater_alive {
        return Err(ApiError::Conflict("the updater is not running".into()));
    }
    if stores.applying {
        return Err(ApiError::Conflict(
            "a retention change is already in progress".into(),
        ));
    }
    if (stores.metrics.locked && request.metrics_days.is_some())
        || (stores.logs.locked && request.logs_days.is_some())
    {
        return Err(ApiError::Conflict(
            "that retention is set by the server's .env".into(),
        ));
    }
    let id = Uuid::new_v4().to_string();
    let file = RetentionRequest {
        id: id.clone(),
        metrics_days: request.metrics_days,
        logs_days: request.logs_days,
        requested_by: user.username.clone(),
        requested_at: Utc::now(),
    };
    let bytes = serde_json::to_vec(&file).expect("a request serializes");
    let dir = state
        .settings
        .server_update
        .dir
        .clone()
        .ok_or_else(|| ApiError::Internal("no update directory".into()))?;
    let inbox = dir.join(INBOX);
    tokio::task::spawn_blocking(move || {
        write_atomic(&inbox, RETENTION_REQUEST_FILE, &bytes, 0o600)
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
    .map_err(|e| ApiError::Internal(format!("writing the retention request: {e}")))?;
    audit::record(
        &state.pool,
        Event {
            actor_kind: "user",
            actor_id: &user.id.to_string(),
            action: "settings.retention",
            target: Some(("settings", "retention")),
            details: json!({
                "request_id": id,
                "old": { "metrics": stores.metrics.value, "logs": stores.logs.value },
                "new": { "metrics_days": request.metrics_days, "logs_days": request.logs_days },
            }),
        },
    )
    .await?;
    tracing::info!(%id, by = %user.username, "retention change requested");
    Ok((StatusCode::ACCEPTED, Json(view(&state).await?)))
}

#[cfg(test)]
mod tests {
    use monitorflow_common::config::ServerConfig;

    use super::*;
    use crate::live_settings::{Live, Stored};

    fn request(lifetime: Option<u64>, idle: Option<u64>, url: Option<&str>) -> SetRequest {
        SetRequest {
            session_lifetime: lifetime,
            session_idle_timeout: idle,
            mirror_public_url: url.map(Into::into),
        }
    }

    fn locks(config: ServerConfig) -> Locks {
        Live::from_config(&config).locks().clone()
    }

    #[test]
    fn defaults_are_stored_as_null_and_urls_are_cleaned() {
        let locks = locks(ServerConfig {
            mirror_default_url: Some("http://mf.example:7480".into()),
            ..ServerConfig::default()
        });
        let old = Stored::default();
        assert_eq!(
            validated(&locks, &old, request(Some(43200), Some(1800), None)).unwrap(),
            (None, None, None)
        );
        assert_eq!(
            validated(
                &locks,
                &old,
                request(Some(7200), None, Some(" http://mf.example:7480/ "))
            )
            .unwrap(),
            (Some(7200), None, None)
        );
        assert_eq!(
            validated(
                &locks,
                &old,
                request(None, Some(600), Some("https://m.example/"))
            )
            .unwrap(),
            (None, Some(600), Some("https://m.example".into()))
        );
    }

    #[test]
    fn values_out_of_range_are_refused() {
        let locks = locks(ServerConfig::default());
        let old = Stored::default();
        for bad in [
            request(Some(60), None, None),
            request(Some(31 * 24 * 3600), None, None),
            request(None, Some(10), None),
            request(Some(3600), Some(7200), None),
            request(None, None, Some("ftp://x")),
            request(None, None, Some("http://x/?a=1")),
        ] {
            assert!(matches!(
                validated(&locks, &old, bad),
                Err(ApiError::Invalid(_))
            ));
        }
    }

    #[test]
    fn the_configuration_locks_a_value() {
        let locks = locks(ServerConfig {
            api_session_lifetime: Some(600),
            mirror_public_url: Some("http://config.example".into()),
            ..ServerConfig::default()
        });
        let old = Stored {
            session_lifetime: Some(7200),
            mirror_public_url: Some("http://stored.example".into()),
            ..Stored::default()
        };
        for bad in [
            request(Some(7200), Some(300), None),
            request(None, Some(300), Some("http://other.example")),
        ] {
            assert!(matches!(
                validated(&locks, &old, bad),
                Err(ApiError::Conflict(_))
            ));
        }
        // Repeating it is fine, and what is stored stays for when the lock
        // goes; the idle timeout is checked against the locked lifetime.
        assert_eq!(
            validated(
                &locks,
                &old,
                request(Some(600), Some(300), Some("http://config.example/"))
            )
            .unwrap(),
            (Some(7200), Some(300), Some("http://stored.example".into()))
        );
        assert!(matches!(
            validated(&locks, &old, request(None, Some(900), None)),
            Err(ApiError::Invalid(_))
        ));
    }
}
