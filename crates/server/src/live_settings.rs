//! The server settings that change at runtime (ADR-0069): what the
//! configuration sets wins, then the row of `server_settings`, then the
//! defaults. Read on hot paths and from code that cannot wait on the
//! database, so each server process keeps them in memory and polls the
//! row; a change from its own API applies at once.

use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use monitorflow_common::config::{
    AgentInstallSource, ServerConfig, DEFAULT_SESSION_IDLE_TIMEOUT, DEFAULT_SESSION_LIFETIME,
};
use sqlx::PgExecutor;

use crate::api::settings::{self, InstallConfig};

/// How often the row is read, as for AI (ADR-0064).
const POLL: Duration = Duration::from_secs(10);

/// What the configuration says about these settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locks {
    pub session_lifetime: Option<u64>,
    pub session_idle_timeout: Option<u64>,
    pub mirror_public_url: Option<String>,
    /// The default of that URL, not a lock.
    pub mirror_default_url: Option<String>,
    pub install: InstallConfig,
    agent_download_url: String,
}

/// The columns of `server_settings` these settings read.
#[derive(Debug, Clone, Default, PartialEq, Eq, sqlx::FromRow)]
pub struct Stored {
    pub session_lifetime: Option<i32>,
    pub session_idle_timeout: Option<i32>,
    pub mirror_public_url: Option<String>,
    pub agent_install_source: Option<String>,
    pub agent_install_custom_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Values {
    session_lifetime: Duration,
    session_idle_timeout: Duration,
    mirror_public_url: Option<String>,
    agent_download_url: String,
}

fn values(locks: &Locks, stored: &Stored) -> Values {
    let seconds = |locked: Option<u64>, stored: Option<i32>, default| {
        Duration::from_secs(
            locked
                .or(stored.and_then(|s| u64::try_from(s).ok()))
                .unwrap_or(default),
        )
    };
    let install = settings::effective(
        &locks.install,
        &settings::Stored {
            agent_install_source: stored.agent_install_source.clone(),
            agent_install_custom_url: stored.agent_install_custom_url.clone(),
        },
        None,
    );
    // Self-updates fall back to where install commands download from.
    let agent_download_url = match install.custom_url {
        Some(url) if install.source == AgentInstallSource::Custom => url,
        _ => locks.agent_download_url.clone(),
    };
    Values {
        session_lifetime: seconds(
            locks.session_lifetime,
            stored.session_lifetime,
            DEFAULT_SESSION_LIFETIME,
        ),
        session_idle_timeout: seconds(
            locks.session_idle_timeout,
            stored.session_idle_timeout,
            DEFAULT_SESSION_IDLE_TIMEOUT,
        ),
        mirror_public_url: locks
            .mirror_public_url
            .clone()
            .or_else(|| stored.mirror_public_url.clone())
            .or_else(|| locks.mirror_default_url.clone()),
        agent_download_url,
    }
}

/// Reads the settings' row.
pub async fn stored(db: impl PgExecutor<'_>) -> Result<Stored, sqlx::Error> {
    Ok(sqlx::query_as(
        "SELECT session_lifetime, session_idle_timeout, mirror_public_url,
                agent_install_source, agent_install_custom_url
         FROM server_settings",
    )
    .fetch_optional(db)
    .await?
    .unwrap_or_default())
}

/// The settings in force; clones share them.
#[derive(Debug, Clone)]
pub struct Live {
    locks: Arc<Locks>,
    values: Arc<RwLock<Values>>,
}

impl Live {
    pub fn from_config(config: &ServerConfig) -> Self {
        let locks = Locks {
            session_lifetime: config.api_session_lifetime,
            session_idle_timeout: config.api_session_idle_timeout,
            mirror_public_url: config.mirror_public_url.clone(),
            mirror_default_url: config.mirror_default_url.clone(),
            install: InstallConfig::from_config(config),
            agent_download_url: config.agent_download_url.clone(),
        };
        let values = values(&locks, &Stored::default());
        Self {
            locks: Arc::new(locks),
            values: Arc::new(RwLock::new(values)),
        }
    }

    pub fn locks(&self) -> &Locks {
        &self.locks
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Values> {
        self.values.read().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn session_lifetime(&self) -> Duration {
        self.read().session_lifetime
    }

    pub fn session_idle_timeout(&self) -> Duration {
        self.read().session_idle_timeout
    }

    /// Base URL of the mirrors in agents' sources (ADR-0035); `None`
    /// refuses source configuration.
    pub fn mirror_public_url(&self) -> Option<String> {
        self.read().mirror_public_url.clone()
    }

    pub fn agent_download_url(&self) -> String {
        self.read().agent_download_url.clone()
    }

    /// Puts `stored` in force.
    pub fn apply(&self, stored: &Stored) {
        let values = values(&self.locks, stored);
        *self.values.write().unwrap_or_else(PoisonError::into_inner) = values;
    }

    /// Reads the row and puts it in force.
    pub async fn refresh(&self, db: impl PgExecutor<'_>) -> Result<(), sqlx::Error> {
        self.apply(&stored(db).await?);
        Ok(())
    }

    /// Follows the row until the process ends.
    pub fn spawn(self, pool: sqlx::PgPool) {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(POLL).await;
                if let Err(error) = self.refresh(&pool).await {
                    tracing::warn!(%error, "cannot read the server settings");
                }
            }
        });
    }

    /// With the mirror URL `url`, for tests.
    pub fn with_mirror_url(self, url: Option<&str>) -> Self {
        let mut locks = (*self.locks).clone();
        locks.mirror_public_url = url.map(Into::into);
        let values = values(&locks, &Stored::default());
        Self {
            locks: Arc::new(locks),
            values: Arc::new(RwLock::new(values)),
        }
    }

    /// With sessions lasting `lifetime`, for tests: shorter than the
    /// configuration's whole seconds.
    pub fn with_session_lifetime(self, lifetime: Duration) -> Self {
        self.values
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .session_lifetime = lifetime;
        self
    }
}

impl Default for Live {
    fn default() -> Self {
        Self::from_config(&ServerConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored() -> Stored {
        Stored {
            session_lifetime: Some(3600),
            session_idle_timeout: Some(600),
            mirror_public_url: Some("http://stored.example:7480".into()),
            ..Stored::default()
        }
    }

    #[test]
    fn defaults_then_stored_values_then_the_configuration() {
        let mut config = ServerConfig {
            mirror_default_url: Some("http://default.example:7480".into()),
            ..ServerConfig::default()
        };
        let live = Live::from_config(&config);
        assert_eq!(live.session_lifetime(), Duration::from_secs(12 * 60 * 60));
        assert_eq!(live.session_idle_timeout(), Duration::from_secs(30 * 60));
        assert_eq!(
            live.mirror_public_url().as_deref(),
            Some("http://default.example:7480")
        );

        live.apply(&stored());
        assert_eq!(live.session_lifetime(), Duration::from_secs(3600));
        assert_eq!(live.session_idle_timeout(), Duration::from_secs(600));
        assert_eq!(
            live.mirror_public_url().as_deref(),
            Some("http://stored.example:7480")
        );
        // Clones follow.
        let clone = live.clone();
        live.apply(&Stored::default());
        assert_eq!(clone.session_lifetime(), Duration::from_secs(12 * 60 * 60));

        config.api_session_lifetime = Some(60);
        config.mirror_public_url = Some("http://config.example".into());
        let live = Live::from_config(&config);
        live.apply(&stored());
        assert_eq!(live.session_lifetime(), Duration::from_secs(60));
        assert_eq!(live.session_idle_timeout(), Duration::from_secs(600));
        assert_eq!(
            live.mirror_public_url().as_deref(),
            Some("http://config.example")
        );
    }

    #[test]
    fn self_updates_follow_a_custom_install_source() {
        let config = ServerConfig::default();
        let live = Live::from_config(&config);
        let custom = Stored {
            agent_install_source: Some("custom".into()),
            agent_install_custom_url: Some("https://files.example/mf".into()),
            ..Stored::default()
        };
        live.apply(&custom);
        assert_eq!(live.agent_download_url(), "https://files.example/mf");
        // The URL stays stored while another source is chosen.
        live.apply(&Stored {
            agent_install_source: Some("github".into()),
            ..custom.clone()
        });
        assert_eq!(live.agent_download_url(), config.agent_download_url);

        let locked = Live::from_config(&ServerConfig {
            agent_download_url: "https://config.example/mf".into(),
            ..ServerConfig::default()
        });
        locked.apply(&custom);
        assert_eq!(locked.agent_download_url(), "https://config.example/mf");
    }
}
