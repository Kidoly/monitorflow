//! Provider-agnostic AI layer of the remediation loop (ADR-0003, ADR-0039).
//!
//! An [`AiProvider`] gets a system prompt, the incident context as data
//! blocks and the exchanges so far, and returns the next [`Step`]: a
//! diagnosis plus either one shell command to run or a decision to stop.
//! The answer is untrusted text: it carries no risk opinion, and the
//! server classifies every command by its own rules (architecture §8).
//!
//! [`claude::ClaudeProvider`] calls the Anthropic Messages API; the
//! `testing` feature adds a scripted [`testing::MockProvider`].

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use monitorflow_common::config::{AiConfig, AiProviderKind};
use serde::Serialize;

pub mod claude;
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

/// Builds the configured provider, or `None` when AI is disabled.
pub async fn from_config(config: &AiConfig) -> Result<Option<Arc<dyn AiProvider>>, AiError> {
    if !config.enabled {
        return Ok(None);
    }
    match config.provider {
        AiProviderKind::Claude => Ok(Some(Arc::new(
            claude::ClaudeProvider::from_config(config).await?,
        ))),
    }
}
