//! What every adapter accepts as an answer: one call to `propose_command`
//! or `stop`, checked strictly whatever the provider (ADR-0039, ADR-0096).
//! The model's way of making the call differs; the checks do not.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::{Action, AiError, ProposedCommand, Step, StopDecision, StopOutcome};

pub const PROPOSE_TOOL: &str = "propose_command";
pub const STOP_TOOL: &str = "stop";
pub(crate) const MAX_COMMAND_LEN: usize = 4096;

/// One tool the model may call: name, description and JSON schema of its
/// input, which every field requires and nothing else may join.
pub(crate) struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: Value,
}

pub(crate) fn tools() -> [Tool; 2] {
    let text = |description: &str| json!({"type": "string", "description": description});
    [
        Tool {
            name: PROPOSE_TOOL,
            description: "Propose the next shell command to run on the affected server. \
                One command on one line. The server classifies its risk with its own \
                rules and may require a human approval before running it; its output \
                and exit code come back in the next request.",
            schema: json!({
                "type": "object",
                "properties": {
                    "diagnosis": text("Your current understanding of the incident."),
                    "command": text("The single shell command to run, on one line."),
                    "reasoning": text("Why this command, and what you expect it to show or fix. Shown to approvers."),
                },
                "required": ["diagnosis", "command", "reasoning"],
                "additionalProperties": false,
            }),
        },
        Tool {
            name: STOP_TOOL,
            description: "Finish the incident: `resolved` when the evidence shows the \
                problem is fixed, `escalate` when a human must take over, for example \
                when no safe command can make progress.",
            schema: json!({
                "type": "object",
                "properties": {
                    "diagnosis": text("Your final understanding of the incident."),
                    "outcome": {"type": "string", "enum": ["resolved", "escalate"]},
                    "reason": text("Why you stop, for the people reading the incident."),
                },
                "required": ["diagnosis", "outcome", "reason"],
                "additionalProperties": false,
            }),
        },
    ]
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposeInput {
    diagnosis: String,
    command: String,
    reasoning: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StopInput {
    diagnosis: String,
    outcome: OutcomeInput,
    reason: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum OutcomeInput {
    Resolved,
    Escalate,
}

/// The [`Step`] of a call to tool `name` with `input`, strictly: no
/// unknown or missing field, non-empty text, a one-line command.
pub(crate) fn step(name: &str, input: Value) -> Result<Step, AiError> {
    let invalid = |e: serde_json::Error| AiError::Malformed(format!("`{name}` input: {e}"));
    match name {
        PROPOSE_TOOL => {
            let input: ProposeInput = serde_json::from_value(input).map_err(invalid)?;
            Ok(Step {
                diagnosis: required("diagnosis", input.diagnosis)?,
                action: Action::Run(ProposedCommand {
                    command: command(input.command)?,
                    reasoning: required("reasoning", input.reasoning)?,
                }),
            })
        }
        STOP_TOOL => {
            let input: StopInput = serde_json::from_value(input).map_err(invalid)?;
            Ok(Step {
                diagnosis: required("diagnosis", input.diagnosis)?,
                action: Action::Stop(StopDecision {
                    outcome: match input.outcome {
                        OutcomeInput::Resolved => StopOutcome::Resolved,
                        OutcomeInput::Escalate => StopOutcome::Escalate,
                    },
                    reason: required("reason", input.reason)?,
                }),
            })
        }
        _ => Err(AiError::Malformed(format!("unknown tool `{name}`"))),
    }
}

fn required(field: &str, value: String) -> Result<String, AiError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(AiError::Malformed(format!("`{field}` is empty")));
    }
    Ok(value.to_owned())
}

/// One non-empty line without control characters.
fn command(value: String) -> Result<String, AiError> {
    let value = required("command", value)?;
    if value.len() > MAX_COMMAND_LEN {
        return Err(AiError::Malformed(format!(
            "command longer than {MAX_COMMAND_LEN} bytes"
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(AiError::Malformed(
            "command holds a line break or control character".into(),
        ));
    }
    Ok(value)
}
