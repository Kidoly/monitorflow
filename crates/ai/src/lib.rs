//! Provider-agnostic AI layer of the remediation loop (ADR-0003, ADR-0039).
//!
//! An [`AiProvider`] gets a system prompt, the incident context as data
//! blocks and the exchanges so far, and returns the next [`Step`]: a
//! diagnosis plus either one shell command to run or a decision to stop.
//! The answer is untrusted text: it carries no risk opinion, and the
//! server classifies every command by its own rules (architecture §8).
//!
//! [`claude::ClaudeProvider`] calls the Anthropic Messages API,
//! [`openai::OpenAiProvider`] the Chat Completions API of OpenAI or of any
//! server speaking it, local models included (ADR-0096). The `testing`
//! feature adds a scripted [`testing::MockProvider`].

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use monitorflow_common::config::{AiConfig, AiProviderKind};
use monitorflow_common::redact::Secret;
use serde::Serialize;

mod answer;
pub mod claude;
mod http;
pub mod openai;
#[cfg(any(test, feature = "testing"))]
pub mod testing;

/// Asks an AI for the next step of an incident, one command at a time.
#[async_trait]
pub trait AiProvider: Send + Sync + std::fmt::Debug {
    /// Short name for logs and the audit trail, like `claude`.
    fn name(&self) -> &str;

    async fn next_step(&self, request: &StepRequest) -> Result<Step, AiError>;
}

/// Everything a provider sees for one step. `context` and the outcomes in
/// `history` come from monitored hosts and must already be redacted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepRequest {
    /// Instructions from the server, the only trusted text sent.
    pub system_prompt: String,
    pub context: Vec<DataBlock>,
    /// Earlier proposals with what became of them, oldest first.
    pub history: Vec<Exchange>,
}

/// A named piece of untrusted data, like recent logs or sensor history.
/// Providers send it apart from the instructions, never inlined in them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataBlock {
    pub title: String,
    pub content: String,
}

/// One earlier step that proposed a command, and its outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exchange {
    pub diagnosis: String,
    pub proposal: ProposedCommand,
    pub outcome: CommandOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CommandOutcome {
    /// `command` is what actually ran, which an approver may have edited.
    /// `exit_code` is `None` when the process was killed by a signal or
    /// timed out.
    Executed {
        command: String,
        exit_code: Option<i32>,
        output: String,
    },
    /// Rejected by an approver or blocked by policy.
    NotRun { reason: String },
}

/// What the provider wants next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub diagnosis: String,
    pub action: Action,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Run(ProposedCommand),
    Stop(StopDecision),
}

/// One shell command, on one line, with the AI's reasoning shown to
/// approvers. There is deliberately no risk field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposedCommand {
    pub command: String,
    pub reasoning: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopDecision {
    pub outcome: StopOutcome,
    pub reason: String,
}

impl Step {
    pub fn run(
        diagnosis: impl Into<String>,
        command: impl Into<String>,
        reasoning: impl Into<String>,
    ) -> Self {
        Self {
            diagnosis: diagnosis.into(),
            action: Action::Run(ProposedCommand {
                command: command.into(),
                reasoning: reasoning.into(),
            }),
        }
    }

    pub fn stop(
        diagnosis: impl Into<String>,
        outcome: StopOutcome,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            diagnosis: diagnosis.into(),
            action: Action::Stop(StopDecision {
                outcome,
                reason: reason.into(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    /// The AI believes the problem is fixed; the sensor decides.
    Resolved,
    /// A human must take over.
    Escalate,
}

/// Errors of a provider. Messages never hold the API key or request URL.
#[derive(Debug, thiserror::Error)]
pub enum AiError {
    /// `reason` never holds the key.
    #[error("AI API key file {}: {reason}", path.display())]
    ApiKeyFile { path: PathBuf, reason: String },
    #[error("invalid AI API key: {0}")]
    InvalidApiKey(&'static str),
    /// A base URL that is not one, or plain HTTP to a public address.
    #[error("AI address: {0}")]
    InvalidAddress(String),
    #[error("AI request failed: {0}")]
    Transport(String),
    #[error("AI API answered HTTP {status} ({}): {message}", error_type.as_deref().unwrap_or("unknown error"))]
    Api {
        status: u16,
        error_type: Option<String>,
        message: String,
        request_id: Option<String>,
    },
    #[error("AI declined to answer (category: {})", category.as_deref().unwrap_or("none"))]
    Refused { category: Option<String> },
    #[error("AI answer was cut off by max_tokens")]
    Truncated,
    #[error("malformed AI answer: {0}")]
    Malformed(String),
}

impl AiError {
    /// Whether the same request may succeed later. Providers already
    /// retried these before giving up.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Transport(_) => true,
            Self::Api { status, .. } => is_retryable_status(*status),
            _ => false,
        }
    }
}

/// HTTP statuses worth retrying: rate limits, server errors, overload.
pub(crate) fn is_retryable_status(status: u16) -> bool {
    matches!(status, 408 | 409 | 429 | 500..=599)
}

/// What a provider needs, from the configuration or the web interface.
/// `Debug` hides the key.
#[derive(Debug, Clone)]
pub struct Connection {
    pub kind: AiProviderKind,
    /// Required but for [`AiProviderKind::OpenAiCompatible`].
    pub api_key: Option<Secret>,
    /// Required for [`AiProviderKind::OpenAiCompatible`]; otherwise
    /// replaces the provider's own address, which only tests do.
    pub base_url: Option<String>,
    pub model: String,
    /// `None` takes the provider's default.
    pub max_tokens: Option<u32>,
    pub request_timeout: Duration,
}

/// A built provider, kept typed for what only the web interface asks:
/// tests and model lists.
enum Built {
    Claude(claude::ClaudeProvider),
    OpenAi(openai::OpenAiProvider),
}

impl Connection {
    /// The configured key, model and limits, without the key.
    pub fn from_config(config: &AiConfig) -> Self {
        Self {
            kind: config.provider,
            api_key: None,
            base_url: config.base_url.clone(),
            model: config.model.clone(),
            max_tokens: config.max_tokens,
            request_timeout: Duration::from_secs(config.request_timeout),
        }
    }

    /// The provider these settings describe.
    pub fn provider(&self) -> Result<Arc<dyn AiProvider>, AiError> {
        Ok(match self.build()? {
            Built::Claude(provider) => Arc::new(provider),
            Built::OpenAi(provider) => Arc::new(provider),
        })
    }

    fn build(&self) -> Result<Built, AiError> {
        let flavor = match self.kind {
            AiProviderKind::Anthropic => {
                let key = self
                    .api_key
                    .clone()
                    .ok_or(AiError::InvalidApiKey("Anthropic needs an API key"))?;
                let mut options = claude::ClaudeOptions::new(key);
                options.model.clone_from(&self.model);
                options.max_tokens = self.max_tokens.unwrap_or(claude::DEFAULT_MAX_TOKENS);
                options.request_timeout = self.request_timeout;
                if let Some(url) = &self.base_url {
                    options.base_url.clone_from(url);
                }
                return Ok(Built::Claude(claude::ClaudeProvider::new(options)?));
            }
            AiProviderKind::OpenAi => openai::Flavor::OpenAi,
            AiProviderKind::OpenAiCompatible => openai::Flavor::Compatible,
        };
        let base_url = match (&self.base_url, flavor) {
            (Some(url), _) => url.clone(),
            (None, openai::Flavor::OpenAi) => openai::OPENAI_BASE_URL.to_owned(),
            (None, openai::Flavor::Compatible) => {
                return Err(AiError::InvalidAddress(
                    "enter the address of the model server".into(),
                ))
            }
        };
        let mut options = openai::OpenAiOptions::new(flavor, base_url, &self.model);
        options.api_key.clone_from(&self.api_key);
        if let Some(max_tokens) = self.max_tokens {
            options.max_tokens = max_tokens;
        }
        options.request_timeout = self.request_timeout;
        Ok(Built::OpenAi(openai::OpenAiProvider::new(options)?))
    }

    /// Whether these settings work, with the smallest real request, or
    /// what is wrong in plain words.
    pub async fn check(&self) -> Result<(), String> {
        let result = match self.build() {
            Ok(Built::Claude(provider)) => provider.check().await,
            Ok(Built::OpenAi(provider)) => provider.check().await,
            Err(error) => Err(error),
        };
        result.map_err(|error| self.explain(&error))
    }

    /// The models the provider's server offers, or what is wrong in plain
    /// words. Anthropic's short list lives in [`claude::MODELS`].
    pub async fn models(&self) -> Result<Vec<String>, String> {
        let result = match self.build() {
            Ok(Built::Claude(_)) => Ok(claude::MODELS.iter().map(|m| m.id.to_owned()).collect()),
            Ok(Built::OpenAi(provider)) => provider.models().await,
            Err(error) => Err(error),
        };
        result.map_err(|error| self.explain(&error))
    }

    fn explain(&self, error: &AiError) -> String {
        match self.kind {
            AiProviderKind::Anthropic => claude::explain(error),
            AiProviderKind::OpenAi => {
                openai::explain(openai::Flavor::OpenAi, openai::OPENAI_BASE_URL, error)
            }
            AiProviderKind::OpenAiCompatible => openai::explain(
                openai::Flavor::Compatible,
                self.base_url.as_deref().unwrap_or_default(),
                error,
            ),
        }
    }
}

/// Builds the configured provider, or `None` when AI is disabled. The key
/// is read from `api_key_file`, which a model server without keys does
/// not need.
pub async fn from_config(config: &AiConfig) -> Result<Option<Arc<dyn AiProvider>>, AiError> {
    if !config.enabled {
        return Ok(None);
    }
    let mut connection = Connection::from_config(config);
    connection.api_key = read_config_key(config).await?;
    connection.provider().map(Some)
}

/// The key of `config`'s `api_key_file`, if any.
pub async fn read_config_key(config: &AiConfig) -> Result<Option<Secret>, AiError> {
    match config.api_key_file {
        Some(_) => claude::read_config_key(config).await.map(Some),
        None => Ok(None),
    }
}
