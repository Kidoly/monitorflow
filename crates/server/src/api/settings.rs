//! Server-wide settings from the web interface: the default source of the
//! agent install commands (ADR-0054). Stored in the one row of
//! `server_settings`; what the server's configuration sets wins and cannot
//! be changed here.

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::Json;
use monitorflow_common::config::{
    valid_mirror_url, AgentInstallSource, ServerConfig, DEFAULT_AGENT_DOWNLOAD_URL,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgExecutor;

use super::access::Scope;
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::sensors::body;
use super::{ApiError, ApiState};
use crate::agent_releases::{CentralRelease, Sources};

/// What the configuration sets about install sources.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstallConfig {
    /// `agent_install_source`.
    pub source: Option<AgentInstallSource>,
    /// `agent_download_url` when it is not GitHub's.
    pub custom_url: Option<String>,
}

impl InstallConfig {
    pub fn from_config(config: &ServerConfig) -> Self {
        Self {
            source: config.agent_install_source,
            custom_url: config.custom_agent_download_url(),
        }
    }
}

/// The row of `server_settings`.
#[derive(Debug, Clone, Default, PartialEq, Eq, sqlx::FromRow)]
pub struct Stored {
    pub agent_install_source: Option<String>,
    pub agent_install_custom_url: Option<String>,
}

/// Body of `GET` and `PUT /api/settings/agent-install`, and the `install`
/// object of a new token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentInstall {
    /// The default source of the install commands.
    pub source: AgentInstallSource,
    /// Base URL of the `custom` source, if any.
    pub custom_url: Option<String>,
    /// Whether the configuration sets `source` or `custom_url`, which the
    /// web interface then cannot change.
    pub source_from_config: bool,
    pub custom_url_from_config: bool,
    /// Base URL of the `github` source.
    pub github_url: &'static str,
    /// The release `central` commands install; `null` with nothing cached.
    pub central: Option<CentralRelease>,
}

/// Configuration first, then the stored values. Without either the source
/// is `custom` when the configuration names a custom URL, as before this
/// setting existed, else `github`; `custom` without a URL falls back to
/// `github`.
pub fn effective(
    config: &InstallConfig,
    stored: &Stored,
    central: Option<CentralRelease>,
) -> AgentInstall {
    let custom_url = config
        .custom_url
        .clone()
        .or_else(|| stored.agent_install_custom_url.clone());
    let stored_source = stored
        .agent_install_source
        .as_deref()
        .and_then(|s| s.parse().ok());
    let mut source = config
        .source
        .or(stored_source)
        .unwrap_or(if config.custom_url.is_some() {
            AgentInstallSource::Custom
        } else {
            AgentInstallSource::Github
        });
    if source == AgentInstallSource::Custom && custom_url.is_none() {
        source = AgentInstallSource::Github;
    }
    AgentInstall {
        source,
        custom_url,
        source_from_config: config.source.is_some(),
        custom_url_from_config: config.custom_url.is_some(),
        github_url: DEFAULT_AGENT_DOWNLOAD_URL,
        central,
    }
}

async fn stored(db: impl PgExecutor<'_>) -> Result<Stored, ApiError> {
    Ok(
        sqlx::query_as(
            "SELECT agent_install_source, agent_install_custom_url FROM server_settings",
        )
        .fetch_optional(db)
        .await?
        .unwrap_or_default(),
    )
}

/// The release `central` would install, if the cache holds one that
/// verifies; a cache that does not is logged and offers nothing.
pub async fn central(sources: &Sources) -> Option<CentralRelease> {
    let cache = sources.cache.clone();
    let keys = sources.keys.clone();
    match tokio::task::spawn_blocking(move || cache.central(&keys)).await {
        Ok(Ok(central)) => central,
        Ok(Err(error)) => {
            tracing::warn!(%error, "the newest cached agent release cannot be offered for installs");
            None
        }
        Err(_) => None,
    }
}

/// The effective install settings, read from the database.
pub async fn agent_install(state: &ApiState) -> Result<AgentInstall, ApiError> {
    let stored = stored(&state.pool).await?;
    let central = central(&state.settings.agent_releases).await;
    Ok(effective(&state.settings.agent_install, &stored, central))
}

/// `GET /api/settings/agent-install`: whoever creates tokens, the
/// administrators of a whole customer.
pub async fn get_agent_install(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<AgentInstall>, ApiError> {
    if user.administered_sites() == Scope::Only(Vec::new()) {
        return Err(ApiError::Forbidden);
    }
    Ok(Json(agent_install(&state).await?))
}

/// Body of `PUT /api/settings/agent-install`: replaces both values.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentInstallRequest {
    pub source: AgentInstallSource,
    #[serde(default)]
    pub custom_url: Option<String>,
}

/// `PUT /api/settings/agent-install`: global administrators, since the
/// setting is server-wide; audited as `settings.agent_install`. A value
/// the configuration sets may only be repeated (409 otherwise).
pub async fn set_agent_install(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<AgentInstallRequest>, JsonRejection>,
) -> Result<Json<AgentInstall>, ApiError> {
    if user.administered_customers() != Scope::All {
        return Err(ApiError::Forbidden);
    }
    let request = body(request)?;
    let config = &state.settings.agent_install;
    let custom_url = request
        .custom_url
        .map(|url| url.trim().trim_end_matches('/').to_owned())
        .filter(|url| !url.is_empty());
    if let Some(url) = &custom_url {
        if !(url.starts_with("https://") && valid_mirror_url(url)) {
            return Err(ApiError::Invalid(
                "custom_url must be an https URL without user, query or fragment".into(),
            ));
        }
    }
    if config.source.is_some_and(|s| s != request.source) {
        return Err(ApiError::Conflict(
            "the install source is set by the server's configuration (agent_install_source)".into(),
        ));
    }
    if config.custom_url.is_some() && custom_url.is_some() && config.custom_url != custom_url {
        return Err(ApiError::Conflict(
            "the custom URL is set by the server's configuration (agent_download_url)".into(),
        ));
    }
    if request.source == AgentInstallSource::Custom
        && custom_url.is_none()
        && config.custom_url.is_none()
    {
        return Err(ApiError::Invalid(
            "the custom source needs custom_url".into(),
        ));
    }
    let mut tx = state.pool.begin().await?;
    // Locks the row, so concurrent changes audit the right old values.
    let old: Stored = sqlx::query_as(
        "SELECT agent_install_source, agent_install_custom_url FROM server_settings FOR UPDATE",
    )
    .fetch_optional(&mut *tx)
    .await?
    .unwrap_or_default();
    sqlx::query(
        "INSERT INTO server_settings (singleton, agent_install_source, agent_install_custom_url)
         VALUES (true, $1, $2)
         ON CONFLICT (singleton) DO UPDATE
         SET agent_install_source = $1, agent_install_custom_url = $2",
    )
    .bind(request.source.as_str())
    .bind(&custom_url)
    .execute(&mut *tx)
    .await?;
    let actor = user.id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: "settings.agent_install",
            target: Some(("settings", "agent_install")),
            details: json!({
                "old": { "source": old.agent_install_source, "custom_url": old.agent_install_custom_url },
                "new": { "source": request.source, "custom_url": custom_url },
            }),
        },
    )
    .await?;
    tx.commit().await?;
    tracing::info!(user = %user.id, source = request.source.as_str(), "changed the default agent install source");
    Ok(Json(agent_install(&state).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(source: Option<&str>, url: Option<&str>) -> Stored {
        Stored {
            agent_install_source: source.map(Into::into),
            agent_install_custom_url: url.map(Into::into),
        }
    }

    #[test]
    fn github_by_default_and_custom_when_configured_before() {
        let none = InstallConfig::default();
        let view = effective(&none, &Stored::default(), None);
        assert_eq!(view.source, AgentInstallSource::Github);
        assert_eq!(view.github_url, DEFAULT_AGENT_DOWNLOAD_URL);
        assert!(!view.source_from_config && !view.custom_url_from_config);
        let legacy = InstallConfig {
            source: None,
            custom_url: Some("https://files.example/mf".into()),
        };
        let view = effective(&legacy, &Stored::default(), None);
        assert_eq!(view.source, AgentInstallSource::Custom);
        assert_eq!(view.custom_url.as_deref(), Some("https://files.example/mf"));
        assert!(view.custom_url_from_config);
        // The stored choice still applies.
        let view = effective(&legacy, &stored(Some("central"), None), None);
        assert_eq!(view.source, AgentInstallSource::Central);
    }

    #[test]
    fn configuration_wins_over_stored_values() {
        let config = InstallConfig {
            source: Some(AgentInstallSource::Central),
            custom_url: None,
        };
        let view = effective(
            &config,
            &stored(Some("custom"), Some("https://stored.example")),
            None,
        );
        assert_eq!(view.source, AgentInstallSource::Central);
        assert_eq!(view.custom_url.as_deref(), Some("https://stored.example"));
        assert!(view.source_from_config && !view.custom_url_from_config);
        let config = InstallConfig {
            source: None,
            custom_url: Some("https://config.example".into()),
        };
        let view = effective(&config, &stored(None, Some("https://stored.example")), None);
        assert_eq!(view.custom_url.as_deref(), Some("https://config.example"));
    }

    #[test]
    fn custom_without_a_url_falls_back_to_github() {
        let view = effective(
            &InstallConfig::default(),
            &stored(Some("custom"), None),
            None,
        );
        assert_eq!(view.source, AgentInstallSource::Github);
        // Unknown stored values, from a later version, are ignored.
        let view = effective(
            &InstallConfig::default(),
            &stored(Some("relay"), None),
            None,
        );
        assert_eq!(view.source, AgentInstallSource::Github);
    }
}
