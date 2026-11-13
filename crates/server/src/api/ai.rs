//! AI setup in the web interface (ADR-0064, ADR-0096): which AI, its key,
//! address and model, test and enable, without a file or a restart. The
//! key is written, never read back: views say `has_key`. `[ai] enabled =
//! true` in the configuration wins and locks all of it.

use std::time::Duration;

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::Json;
use monitorflow_ai::claude::{Model, DEFAULT_BASE_URL, MODELS};
use monitorflow_ai::openai::{self, DEFAULT_OPENAI_MODEL, OPENAI_BASE_URL};
use monitorflow_ai::Connection;
use monitorflow_common::config::{AiConfig, AiProviderKind, ServerConfig};
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
use crate::secrets::Column;

/// Longest accepted API key.
const MAX_KEY: usize = 1024;
/// Longest accepted model name or address.
const MAX_TEXT: usize = 512;

/// A stored key is not sent to another address than the one it was
/// entered for.
const KEY_OF_ANOTHER_ADDRESS: &str =
    "enter the API key again: the stored one was entered for another address";

const LOCKED: &str = "AI is set by the server's configuration ([ai] in server.toml)";

/// The configuration, and where the APIs are.
#[derive(Debug, Clone)]
pub struct AiSettings {
    pub config: AiConfig,
    /// Anthropic's API; tests point this at a local server.
    pub base_url: String,
    /// OpenAI's API; tests point this at a local server.
    pub openai_base_url: String,
}

impl AiSettings {
    pub fn from_config(config: &ServerConfig) -> Self {
        Self {
            config: config.ai.clone(),
            base_url: DEFAULT_BASE_URL.into(),
            openai_base_url: OPENAI_BASE_URL.into(),
        }
    }

    /// What reaches `kind` with this key, address and model, under the
    /// configuration's limits. Anthropic and OpenAI have their own
    /// address.
    fn connection(
        &self,
        kind: AiProviderKind,
        api_key: Option<Secret>,
        base_url: Option<String>,
        model: String,
    ) -> Connection {
        let base_url = match kind {
            AiProviderKind::Anthropic => Some(self.base_url.clone()),
            AiProviderKind::OpenAi => Some(self.openai_base_url.clone()),
            AiProviderKind::OpenAiCompatible => base_url,
        };
        Connection {
            kind,
            api_key,
            base_url,
            model,
            max_tokens: self.config.max_tokens,
            request_timeout: Duration::from_secs(self.config.request_timeout),
        }
    }
}

/// The AI columns of `server_settings`.
#[derive(Default, sqlx::FromRow)]
struct Stored {
    ai_enabled: bool,
    ai_provider: String,
    ai_api_key: Option<String>,
    ai_base_url: Option<String>,
    ai_model: Option<String>,
}

impl Stored {
    fn kind(&self) -> AiProviderKind {
        self.ai_provider.parse().unwrap_or_default()
    }

    /// The stored key, opened (ADR-0110); one that does not open must be
    /// entered again.
    fn key(&self) -> Result<Option<Secret>, ApiError> {
        crate::secrets::open_stored(Column::AiApiKey, self.ai_api_key.clone())
            .transpose()
            .map_err(|reason| ApiError::Invalid(format!("{reason}. Enter the API key again.")))
    }

    /// The stored key, for whom it was entered: the stored provider and,
    /// for an OpenAI-compatible server, the stored address. Anywhere else
    /// it would be sent to an address of the caller's choice.
    fn key_for(
        &self,
        kind: AiProviderKind,
        base_url: Option<&str>,
    ) -> Result<Option<Secret>, ApiError> {
        if kind != self.kind() {
            return Ok(None);
        }
        let moved =
            kind == AiProviderKind::OpenAiCompatible && self.ai_base_url.as_deref() != base_url;
        if moved && self.ai_api_key.is_some() {
            return Err(ApiError::Invalid(KEY_OF_ANOTHER_ADDRESS.into()));
        }
        self.key()
    }

    /// The stored model, while Anthropic still offers it.
    fn model(&self) -> String {
        let stored = self.ai_model.as_deref().filter(|m| !m.is_empty());
        match self.kind() {
            AiProviderKind::Anthropic => stored.filter(|m| offered(m)).unwrap_or(MODELS[0].id),
            AiProviderKind::OpenAi => stored.unwrap_or(DEFAULT_OPENAI_MODEL),
            AiProviderKind::OpenAiCompatible => stored.unwrap_or_default(),
        }
        .to_owned()
    }

    fn audit(&self) -> serde_json::Value {
        json!({
            "enabled": self.ai_enabled,
            "provider": self.kind(),
            "model": self.ai_model,
            "base_url": self.ai_base_url,
            "has_key": self.ai_api_key.is_some(),
        })
    }
}

fn offered(model: &str) -> bool {
    MODELS.iter().any(|m| m.id == model)
}

/// Body of `GET`, `PUT` and `DELETE /api/settings/ai`. Readable by every
/// user, since approvals say when AI is off; holds no key, and the address
/// only for global administrators.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AiView {
    pub enabled: bool,
    /// `[ai] enabled = true` in the configuration, which takes precedence
    /// and locks this page.
    pub from_config: bool,
    pub provider: AiProviderKind,
    /// Whether an API key is stored, or configured.
    pub has_key: bool,
    /// The address of an OpenAI-compatible server, often inside the
    /// operator's network: null for all but global administrators.
    pub base_url: Option<String>,
    pub model: String,
    /// Anthropic's short list, the default first.
    pub models: &'static [Model],
}

impl AiView {
    /// What everyone else reads: whether AI is on, and with which model.
    fn without_address(mut self) -> Self {
        self.base_url = None;
        self
    }
}

fn view(config: &AiConfig, stored: &Stored) -> AiView {
    if config.enabled {
        return AiView {
            enabled: true,
            from_config: true,
            provider: config.provider,
            has_key: config.api_key_file.is_some(),
            base_url: config.base_url.clone(),
            model: config.model.clone(),
            models: MODELS,
        };
    }
    AiView {
        enabled: stored.ai_enabled,
        from_config: false,
        provider: stored.kind(),
        has_key: stored.ai_api_key.is_some(),
        base_url: stored.ai_base_url.clone(),
        model: stored.model(),
        models: MODELS,
    }
}

async fn stored(db: impl PgExecutor<'_>, lock: bool) -> Result<Stored, ApiError> {
    let query = if lock {
        "SELECT ai_enabled, ai_provider, ai_api_key, ai_base_url, ai_model
         FROM server_settings FOR UPDATE"
    } else {
        "SELECT ai_enabled, ai_provider, ai_api_key, ai_base_url, ai_model FROM server_settings"
    };
    Ok(sqlx::query_as(query)
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

/// A typed model name or address: trimmed, `None` when empty.
fn typed(text: Option<String>, what: &str) -> Result<Option<String>, ApiError> {
    let Some(text) = text else { return Ok(None) };
    let text = text.trim();
    if text.len() > MAX_TEXT || text.chars().any(char::is_control) {
        return Err(ApiError::Invalid(format!("the {what} is not valid")));
    }
    Ok((!text.is_empty()).then(|| text.to_owned()))
}

fn known_model(kind: AiProviderKind, model: &str) -> Result<(), ApiError> {
    match kind {
        AiProviderKind::Anthropic if !offered(model) => {
            Err(ApiError::Invalid(format!("unknown model `{model}`")))
        }
        _ if model.is_empty() => Err(ApiError::Invalid("enter a model".into())),
        _ => Ok(()),
    }
}

/// The address an OpenAI-compatible server needs, checked as a URL; the
/// others have none.
fn address(kind: AiProviderKind, base_url: Option<String>) -> Result<Option<String>, ApiError> {
    if kind != AiProviderKind::OpenAiCompatible {
        return Ok(None);
    }
    let base_url = typed(base_url, "address")?
        .ok_or_else(|| ApiError::Invalid("enter the address of the model server".into()))?;
    openai::parse_base_url(&base_url).map_err(|e| ApiError::Invalid(plain(&e.to_string())))?;
    Ok(Some(base_url.trim_end_matches('/').to_owned()))
}

/// `AI address: reason` as the reason alone.
fn plain(error: &str) -> String {
    error.trim_start_matches("AI address: ").to_owned()
}

/// `GET /api/settings/ai`: every user; the address of the model server
/// only to global administrators, who set it.
pub async fn get(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<AiView>, ApiError> {
    let view = current(&state).await?;
    Ok(Json(if require_global_admin(&user).is_ok() {
        view
    } else {
        view.without_address()
    }))
}

/// Body of `PUT /api/settings/ai`: replaces everything but the key, which
/// is absent, `null` or empty to keep the stored one. The stored key
/// belongs to the stored provider: another provider forgets it. For an
/// OpenAI-compatible server it also belongs to the stored address: another
/// address needs the key again, 400 otherwise.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetRequest {
    /// Absent keeps the stored provider.
    #[serde(default)]
    pub provider: Option<AiProviderKind>,
    #[serde(default)]
    pub api_key: Option<Secret>,
    /// Required for `openai_compatible`, ignored otherwise.
    #[serde(default)]
    pub base_url: Option<String>,
    pub model: String,
    pub enabled: bool,
}

/// `PUT /api/settings/ai`: global administrators; audited as `settings.ai`,
/// without the key. 409 while the configuration sets AI. Turning AI on, or
/// changing what it uses while on, tests it first and answers 400 in plain
/// words when it does not work.
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
    let before = stored(&state.pool, false).await?;
    let kind = request.provider.unwrap_or_else(|| before.kind());
    let model = typed(Some(request.model), "model name")?.unwrap_or_default();
    known_model(kind, &model)?;
    let base_url = address(kind, request.base_url)?;
    let key = pasted(request.api_key)?;
    let same_provider = kind == before.kind();
    // Whether enabled or not: saving another address never keeps the key.
    let usable = match key.clone() {
        Some(key) => Some(key),
        None => before.key_for(kind, base_url.as_deref())?,
    };
    if request.enabled {
        if usable.is_none() && kind != AiProviderKind::OpenAiCompatible {
            return Err(ApiError::Invalid("enter an API key first".into()));
        }
        let same = before.ai_enabled
            && same_provider
            && key.is_none()
            && before.model() == model
            && before.ai_base_url == base_url;
        if !same {
            settings
                .connection(kind, usable, base_url.clone(), model.clone())
                .check()
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
    // The old key stays only with the provider it was entered for, and
    // for an OpenAI-compatible server with its address, also when a
    // concurrent change moved either since the check above.
    sqlx::query(
        "UPDATE server_settings
         SET ai_enabled = $1,
             ai_api_key = COALESCE($2, CASE
                 WHEN ai_provider = $4
                      AND ($4 <> 'openai_compatible' OR ai_base_url IS NOT DISTINCT FROM $5)
                 THEN ai_api_key END),
             ai_model = $3, ai_provider = $4, ai_base_url = $5",
    )
    .bind(request.enabled)
    .bind(
        key.as_ref()
            .map(|k| crate::secrets::seal(Column::AiApiKey, k.expose()))
            .transpose()
            .map_err(|e| ApiError::Internal(e.to_string()))?,
    )
    .bind(&model)
    .bind(kind.as_str())
    .bind(&base_url)
    .execute(&mut *tx)
    .await
    .map_err(not_enough_left)?;
    let details = json!({
        "old": old.audit(),
        "new": {
            "enabled": request.enabled, "provider": kind, "model": model,
            "base_url": base_url, "key_changed": key.is_some(),
        },
    });
    record(&mut *tx, &user, details).await?;
    tx.commit().await?;
    tracing::info!(user = %user.id, enabled = request.enabled, provider = kind.as_str(), %model, "changed the AI settings");
    Ok(Json(current(&state).await?))
}

/// A concurrent change took the key between the check and the write.
fn not_enough_left(error: sqlx::Error) -> ApiError {
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
        "old": old.audit(),
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

/// Body of `POST /api/settings/ai/test` and `/models`, what the form
/// holds. What it leaves out is taken from the stored settings, the key
/// and address only for the same provider, and the key of an
/// OpenAI-compatible server only for the stored address.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TestRequest {
    pub provider: Option<AiProviderKind>,
    /// Absent or empty uses the stored key.
    pub api_key: Option<Secret>,
    pub base_url: Option<String>,
    /// Absent tests the stored model.
    pub model: Option<String>,
}

/// What a test or a model list works on: the configuration's settings
/// while it sets AI, else the form's over the stored ones.
async fn resolve(state: &ApiState, request: TestRequest) -> Result<Connection, String> {
    let settings = &state.settings.ai;
    let config = &settings.config;
    if config.enabled {
        let key = monitorflow_ai::read_config_key(config)
            .await
            .map_err(|e| e.to_string())?;
        let mut connection = settings.connection(
            config.provider,
            key,
            config.base_url.clone(),
            config.model.clone(),
        );
        if config.provider != AiProviderKind::OpenAiCompatible {
            // An address in the configuration replaces the provider's own.
            connection.base_url = config.base_url.clone().or(connection.base_url);
        }
        return Ok(connection);
    }
    let invalid = |e: ApiError| match e {
        ApiError::Invalid(reason) => capitalized(&reason),
        other => other.to_string(),
    };
    let before = stored(&state.pool, false)
        .await
        .map_err(|e| e.to_string())?;
    let kind = request.provider.unwrap_or_else(|| before.kind());
    let same_provider = kind == before.kind();
    let model = match typed(request.model, "model name").map_err(invalid)? {
        Some(model) => model,
        None if same_provider => before.model(),
        None => String::new(),
    };
    if kind == AiProviderKind::Anthropic {
        known_model(kind, &model).map_err(invalid)?;
    }
    let base_url = typed(request.base_url, "address")
        .map_err(invalid)?
        .or_else(|| before.ai_base_url.clone().filter(|_| same_provider));
    let base_url = address(kind, base_url).map_err(invalid)?;
    let key = match pasted(request.api_key).map_err(invalid)? {
        Some(key) => Some(key),
        None => before.key_for(kind, base_url.as_deref()).map_err(invalid)?,
    };
    if key.is_none() && kind != AiProviderKind::OpenAiCompatible {
        return Err("Enter an API key first.".into());
    }
    Ok(settings.connection(kind, key, base_url, model))
}

fn capitalized(reason: &str) -> String {
    let mut chars = reason.chars();
    match chars.next() {
        Some(first) => format!("{}{}.", first.to_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

/// `POST /api/settings/ai/test`: global administrators; saves nothing.
/// Tests the form's settings, or under the configuration what it sets,
/// and answers in plain words.
pub async fn test(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<TestRequest>, JsonRejection>,
) -> Result<Json<TestResult>, ApiError> {
    require_global_admin(&user)?;
    let request = body(request)?;
    let connection = resolve(&state, request).await;
    let result = match &connection {
        Ok(connection) if connection.model.is_empty() => Err("Enter a model first.".to_owned()),
        Ok(connection) => connection.check().await,
        Err(error) => Err(error.clone()),
    };
    let provider = connection
        .as_ref()
        .map(|c| c.kind.as_str())
        .unwrap_or_default();
    let model = connection
        .as_ref()
        .map(|c| c.model.as_str())
        .unwrap_or_default();
    tracing::info!(user = %user.id, provider, model, ok = result.is_ok(), "tested the AI settings");
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

/// Body of `POST /api/settings/ai/models`.
#[derive(Debug, Serialize)]
pub struct ModelList {
    /// Sorted; empty when the server could not say.
    pub models: Vec<String>,
    /// Why there is no list, in plain words.
    pub error: Option<String>,
}

/// `POST /api/settings/ai/models`: global administrators; the models the
/// form's provider offers, read from its server.
pub async fn models(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<TestRequest>, JsonRejection>,
) -> Result<Json<ModelList>, ApiError> {
    require_global_admin(&user)?;
    let mut request = body(request)?;
    // Listing needs no model; Anthropic's default keeps the checks quiet.
    request.model = Some(MODELS[0].id.into());
    let result = match resolve(&state, request).await {
        Ok(connection) => connection.models().await,
        Err(error) => Err(error),
    };
    Ok(Json(match result {
        Ok(models) => ModelList {
            models,
            error: None,
        },
        Err(error) => ModelList {
            models: Vec::new(),
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
            provider: AiProviderKind::OpenAiCompatible,
            base_url: Some("http://ollama.lan:11434/v1".into()),
            model: "qwen3:8b".into(),
            ..AiConfig::default()
        };
        let stored = Stored {
            ai_model: Some("claude-sonnet-5-5".into()),
            ..Stored::default()
        };
        let shown = view(&config, &stored);
        assert!(shown.enabled && shown.from_config && !shown.has_key);
        assert_eq!(shown.provider, AiProviderKind::OpenAiCompatible);
        assert_eq!(shown.model, "qwen3:8b");
        assert_eq!(
            shown.base_url.as_deref(),
            Some("http://ollama.lan:11434/v1")
        );
    }

    #[test]
    fn a_model_no_longer_offered_falls_back_to_the_default() {
        let stored = Stored {
            ai_model: Some("claude-old".into()),
            ..Stored::default()
        };
        let shown = view(&AiConfig::default(), &stored);
        assert_eq!(shown.provider, AiProviderKind::Anthropic);
        assert_eq!(shown.model, MODELS[0].id);
        assert!(!shown.enabled && !shown.from_config && !shown.has_key);

        // Other providers keep any model; OpenAI starts at its default.
        let openai = Stored {
            ai_provider: "openai".into(),
            ..Stored::default()
        };
        assert_eq!(openai.model(), DEFAULT_OPENAI_MODEL);
        let local = Stored {
            ai_provider: "openai_compatible".into(),
            ai_model: Some("llama3.1:8b".into()),
            ..Stored::default()
        };
        assert_eq!(local.model(), "llama3.1:8b");
    }

    #[test]
    fn the_address_is_shown_to_global_administrators_only() {
        let stored = Stored {
            ai_enabled: true,
            ai_provider: "openai_compatible".into(),
            ai_base_url: Some("http://ollama.lan:11434/v1".into()),
            ai_model: Some("qwen3:8b".into()),
            ..Stored::default()
        };
        let full = view(&AiConfig::default(), &stored);
        assert_eq!(full.base_url.as_deref(), Some("http://ollama.lan:11434/v1"));
        let shown = full.clone().without_address();
        assert_eq!(shown.base_url, None);
        assert!(shown.enabled && shown.model == "qwen3:8b");
        assert_eq!(shown.provider, AiProviderKind::OpenAiCompatible);
    }

    fn sealed(key: &str) -> Option<String> {
        crate::secrets::install_for_tests();
        Some(crate::secrets::seal(Column::AiApiKey, key).unwrap())
    }

    #[test]
    fn the_stored_key_stays_with_its_provider_and_address() {
        let compatible = AiProviderKind::OpenAiCompatible;
        let stored = Stored {
            ai_provider: "openai_compatible".into(),
            ai_api_key: sealed("sk-local"),
            ai_base_url: Some("http://ollama.lan:11434/v1".into()),
            ..Stored::default()
        };
        let key = stored.key_for(compatible, Some("http://ollama.lan:11434/v1"));
        assert_eq!(key.unwrap().unwrap().expose(), "sk-local");
        // Another address, or none: the key must be pasted again.
        for other in [Some("http://evil.example/v1"), None] {
            match stored.key_for(compatible, other) {
                Err(ApiError::Invalid(reason)) => assert_eq!(reason, KEY_OF_ANOTHER_ADDRESS),
                other => panic!("{other:?}"),
            }
        }
        // Another provider forgets it, without an error.
        assert!(stored
            .key_for(AiProviderKind::OpenAi, None)
            .unwrap()
            .is_none());

        // Without a stored key a new address needs none: local servers
        // often have no key.
        let keyless = Stored {
            ai_api_key: None,
            ..stored
        };
        let key = keyless.key_for(compatible, Some("http://other.lan/v1"));
        assert!(key.unwrap().is_none());

        // Anthropic and OpenAI have one address each, never the caller's.
        let anthropic = Stored {
            ai_api_key: sealed("sk-ant-1"),
            ..Stored::default()
        };
        let key = anthropic.key_for(AiProviderKind::Anthropic, None);
        assert_eq!(key.unwrap().unwrap().expose(), "sk-ant-1");

        // One that does not open is asked for again, never sent.
        let lost = Stored {
            ai_api_key: Some("mfs1:00000000:AAAA".into()),
            ..Stored::default()
        };
        match lost.key_for(AiProviderKind::Anthropic, None) {
            Err(ApiError::Invalid(reason)) => assert!(reason.contains("again"), "{reason}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_pasted_key_is_trimmed_and_an_empty_one_keeps_the_stored_key() {
        let key = pasted(Some(Secret::new(" sk-ant-1\n"))).unwrap().unwrap();
        assert_eq!(key.expose(), "sk-ant-1");
        assert!(pasted(Some(Secret::new("  "))).unwrap().is_none());
        assert!(pasted(None).unwrap().is_none());
        assert!(pasted(Some(Secret::new("k".repeat(MAX_KEY + 1)))).is_err());
    }

    #[test]
    fn models_and_addresses_are_checked_per_provider() {
        assert!(known_model(AiProviderKind::Anthropic, "gpt-5.1").is_err());
        assert!(known_model(AiProviderKind::OpenAi, "gpt-5.1").is_ok());
        assert!(known_model(AiProviderKind::OpenAiCompatible, "").is_err());
        assert!(typed(Some("qwen3\n".into()), "model name").unwrap() == Some("qwen3".into()));
        assert!(typed(Some("a\u{0}b".into()), "model name").is_err());

        let compatible = AiProviderKind::OpenAiCompatible;
        assert_eq!(
            address(compatible, Some(" http://ollama.lan:11434/v1/ ".into())).unwrap(),
            Some("http://ollama.lan:11434/v1".into())
        );
        assert!(address(compatible, None).is_err());
        match address(compatible, Some("ollama.lan:11434".into())) {
            Err(ApiError::Invalid(reason)) => {
                assert!(reason.starts_with("the address must"), "{reason}")
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            address(AiProviderKind::OpenAi, Some("http://x".into())).unwrap(),
            None
        );
        assert_eq!(capitalized("enter a model"), "Enter a model.");
    }
}
