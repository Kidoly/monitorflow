//! AI setup in the web interface (ADR-0064): API key, model, test and
//! enable, without a file or a restart. The key is written, never read
//! back: views say `has_key`. `[ai] enabled = true` in the configuration
//! wins and locks all of it.

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::Json;
use monitorflow_ai::claude::{
    self, ClaudeOptions, ClaudeProvider, Model, DEFAULT_BASE_URL, MODELS,
};
use monitorflow_ai::AiError;
use monitorflow_common::config::{AiConfig, ServerConfig};
use monitorflow_common::redact::Secret;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgExecutor;

use super::access::Scope;
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::notifications::TestResult;
use super::sensors::body;
use super::{ApiError, ApiState};

/// Longest accepted API key.
const MAX_KEY: usize = 1024;

const LOCKED: &str = "AI is set by the server's configuration ([ai] in server.toml)";

/// The configuration, and where the API is.
#[derive(Debug, Clone)]
pub struct AiSettings {
    pub config: AiConfig,
    /// Tests point this at a local server.
    pub base_url: String,
}

impl AiSettings {
    pub fn from_config(config: &ServerConfig) -> Self {
        Self {
            config: config.ai.clone(),
            base_url: DEFAULT_BASE_URL.into(),
        }
    }

    fn provider(&self, api_key: Secret, model: &str) -> Result<ClaudeProvider, AiError> {
        let mut options = ClaudeOptions::from_config(api_key, &self.config);
        options.model = model.to_owned();
        options.base_url.clone_from(&self.base_url);
        ClaudeProvider::new(options)
    }

    async fn check(&self, api_key: Secret, model: &str) -> Result<(), String> {
        let provider = self
            .provider(api_key, model)
            .map_err(|e| claude::explain(&e))?;
        provider.check().await.map_err(|e| claude::explain(&e))
    }
}

/// The AI columns of `server_settings`.
#[derive(Default, sqlx::FromRow)]
struct Stored {
    ai_enabled: bool,
    ai_api_key: Option<String>,
    ai_model: Option<String>,
}

impl Stored {
    fn key(&self) -> Option<Secret> {
        self.ai_api_key.clone().map(Secret::new)
    }

    /// The stored model while it is still offered.
    fn model(&self) -> &str {
        self.ai_model
            .as_deref()
            .filter(|m| offered(m))
            .unwrap_or(MODELS[0].id)
    }
}

fn offered(model: &str) -> bool {
    MODELS.iter().any(|m| m.id == model)
}

/// Body of `GET`, `PUT` and `DELETE /api/settings/ai`. Readable by every
/// user, since approvals say when AI is off; holds no key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AiView {
    pub enabled: bool,
    /// `[ai] enabled = true` in the configuration, which takes precedence
    /// and locks this page.
    pub from_config: bool,
    /// Whether an API key is stored, or configured.
    pub has_key: bool,
    pub model: String,
    /// What the web interface offers, the default first.
    pub models: &'static [Model],
}

fn view(config: &AiConfig, stored: &Stored) -> AiView {
    if config.enabled {
        return AiView {
            enabled: true,
            from_config: true,
            has_key: true,
            model: config.model.clone(),
            models: MODELS,
        };
    }
    AiView {
        enabled: stored.ai_enabled,
        from_config: false,
        has_key: stored.ai_api_key.is_some(),
        model: stored.model().to_owned(),
        models: MODELS,
    }
}

async fn stored(db: impl PgExecutor<'_>, lock: bool) -> Result<Stored, ApiError> {
    let query = format!(
        "SELECT ai_enabled, ai_api_key, ai_model FROM server_settings{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    Ok(sqlx::query_as(&query)
        .fetch_optional(db)
        .await?
        .unwrap_or_default())
}

async fn current(state: &ApiState) -> Result<AiView, ApiError> {
    let stored = stored(&state.pool, false).await?;
    Ok(view(&state.settings.ai.config, &stored))
}

fn require_global_admin(user: &CurrentUser) -> Result<(), ApiError> {
    if user.administered_customers() == Scope::All {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

/// A pasted key: `None` when empty, which keeps the stored one.
fn pasted(key: Option<Secret>) -> Result<Option<Secret>, ApiError> {
    let Some(key) = key else { return Ok(None) };
    let key = key.expose().trim();
    if key.len() > MAX_KEY {
        return Err(ApiError::Invalid(format!(
            "the API key is longer than {MAX_KEY} bytes"
        )));
    }
    Ok((!key.is_empty()).then(|| Secret::new(key)))
}

fn known_model(model: &str) -> Result<(), ApiError> {
    if offered(model) {
        Ok(())
    } else {
        Err(ApiError::Invalid(format!("unknown model `{model}`")))
    }
}

/// `GET /api/settings/ai`: every user.
pub async fn get(State(state): State<ApiState>, _: CurrentUser) -> Result<Json<AiView>, ApiError> {
    Ok(Json(current(&state).await?))
}

/// Body of `PUT /api/settings/ai`: replaces everything but the key, which
/// is absent, `null` or empty to keep the stored one.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetRequest {
    #[serde(default)]
    pub api_key: Option<Secret>,
    pub model: String,
    pub enabled: bool,
}

/// `PUT /api/settings/ai`: global administrators; audited as `settings.ai`,
/// without the key. 409 while the configuration sets AI. Turning AI on, or
/// changing the key or model while on, tests them first and answers 400
/// in plain words when they do not work.
pub async fn set(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<SetRequest>, JsonRejection>,
) -> Result<Json<AiView>, ApiError> {
    require_global_admin(&user)?;
    let settings = &state.settings.ai;
    if settings.config.enabled {
        return Err(ApiError::Conflict(LOCKED.into()));
    }
    let request = body(request)?;
    known_model(&request.model)?;
    let key = pasted(request.api_key)?;
    let before = stored(&state.pool, false).await?;
    if request.enabled {
        let Some(usable) = key.clone().or_else(|| before.key()) else {
            return Err(ApiError::Invalid("enter an API key first".into()));
        };
        let same = before.ai_enabled && key.is_none() && before.model() == request.model;
        if !same {
            settings
                .check(usable, &request.model)
                .await
                .map_err(ApiError::Invalid)?;
        }
    }
    let mut tx = state.pool.begin().await?;
    // Apart from the update: the constraint would judge an insert's row
    // without the stored key.
    sqlx::query("INSERT INTO server_settings (singleton) VALUES (true) ON CONFLICT DO NOTHING")
        .execute(&mut *tx)
        .await?;
    // Locks the row, so concurrent changes audit the right old values.
    let old = stored(&mut *tx, true).await?;
    sqlx::query(
        "UPDATE server_settings
         SET ai_enabled = $1, ai_api_key = COALESCE($2, ai_api_key), ai_model = $3",
    )
    .bind(request.enabled)
    .bind(key.as_ref().map(Secret::expose))
    .bind(&request.model)
    .execute(&mut *tx)
    .await
    .map_err(no_key_left)?;
    let details = json!({
        "old": { "enabled": old.ai_enabled, "model": old.ai_model, "has_key": old.ai_api_key.is_some() },
        "new": { "enabled": request.enabled, "model": request.model, "key_changed": key.is_some() },
    });
    record(&mut *tx, &user, details).await?;
    tx.commit().await?;
    tracing::info!(user = %user.id, enabled = request.enabled, model = %request.model, "changed the AI settings");
    Ok(Json(current(&state).await?))
}

/// A concurrent `DELETE` took the key between the check and the write.
fn no_key_left(error: sqlx::Error) -> ApiError {
    match &error {
        sqlx::Error::Database(e) if e.constraint() == Some("server_settings_ai_needs_a_key") => {
            ApiError::Invalid("enter an API key first".into())
        }
        _ => error.into(),
    }
}

/// `DELETE /api/settings/ai`: forgets the key, which turns AI off; global
/// administrators; audited as `settings.ai`.
pub async fn remove_key(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<AiView>, ApiError> {
    require_global_admin(&user)?;
    if state.settings.ai.config.enabled {
        return Err(ApiError::Conflict(LOCKED.into()));
    }
    let mut tx = state.pool.begin().await?;
    let old = stored(&mut *tx, true).await?;
    sqlx::query("UPDATE server_settings SET ai_enabled = false, ai_api_key = NULL")
        .execute(&mut *tx)
        .await?;
    let details = json!({
        "old": { "enabled": old.ai_enabled, "model": old.ai_model, "has_key": old.ai_api_key.is_some() },
        "new": { "enabled": false, "has_key": false },
    });
    record(&mut *tx, &user, details).await?;
    tx.commit().await?;
    tracing::info!(user = %user.id, "removed the AI API key");
    Ok(Json(current(&state).await?))
}

async fn record(
    db: impl PgExecutor<'_>,
    user: &CurrentUser,
    details: serde_json::Value,
) -> Result<(), ApiError> {
    let actor = user.id.to_string();
    audit::record(
        db,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: "settings.ai",
            target: Some(("settings", "ai")),
            details,
        },
    )
    .await?;
    Ok(())
}

/// Body of `POST /api/settings/ai/test`, what the form holds.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TestRequest {
    /// Absent or empty tests the stored key.
    pub api_key: Option<Secret>,
    /// Absent tests the stored model.
    pub model: Option<String>,
}

/// `POST /api/settings/ai/test`: global administrators; saves nothing.
/// Tests the form's key and model, or under the configuration what it
/// sets, and answers in plain words.
pub async fn test(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<TestRequest>, JsonRejection>,
) -> Result<Json<TestResult>, ApiError> {
    require_global_admin(&user)?;
    let settings = &state.settings.ai;
    let request = body(request)?;
    let (key, model) = if settings.config.enabled {
        let key = claude::read_config_key(&settings.config)
            .await
            .map_err(|e| e.to_string());
        (key.map(Some), settings.config.model.clone())
    } else {
        let before = stored(&state.pool, false).await?;
        let model = request.model.unwrap_or_else(|| before.model().to_owned());
        known_model(&model)?;
        (Ok(pasted(request.api_key)?.or_else(|| before.key())), model)
    };
    let result = match key {
        Ok(Some(key)) => settings.check(key, &model).await,
        Ok(None) => Err("Enter an API key first.".to_owned()),
        Err(error) => Err(error),
    };
    tracing::info!(user = %user.id, %model, ok = result.is_ok(), "tested the AI settings");
    Ok(Json(match result {
        Ok(()) => TestResult {
            ok: true,
            error: None,
        },
        Err(error) => TestResult {
            ok: false,
            error: Some(error),
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_configuration_takes_precedence_over_stored_settings() {
        let config = AiConfig {
            enabled: true,
            model: "claude-custom".into(),
            ..AiConfig::default()
        };
        let stored = Stored {
            ai_enabled: false,
            ai_api_key: None,
            ai_model: Some("claude-sonnet-5-5".into()),
        };
        let shown = view(&config, &stored);
        assert!(shown.enabled && shown.from_config && shown.has_key);
        assert_eq!(shown.model, "claude-custom");
    }

    #[test]
    fn a_model_no_longer_offered_falls_back_to_the_default() {
        let stored = Stored {
            ai_model: Some("claude-old".into()),
            ..Stored::default()
        };
        let shown = view(&AiConfig::default(), &stored);
        assert_eq!(shown.model, MODELS[0].id);
        assert!(!shown.enabled && !shown.from_config && !shown.has_key);
    }

    #[test]
    fn a_pasted_key_is_trimmed_and_an_empty_one_keeps_the_stored_key() {
        let key = pasted(Some(Secret::new(" sk-ant-1\n"))).unwrap().unwrap();
        assert_eq!(key.expose(), "sk-ant-1");
        assert!(pasted(Some(Secret::new("  "))).unwrap().is_none());
        assert!(pasted(None).unwrap().is_none());
        assert!(pasted(Some(Secret::new("k".repeat(MAX_KEY + 1)))).is_err());
    }
}
