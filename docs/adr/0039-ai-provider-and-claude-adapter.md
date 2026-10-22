# ADR-0039: AI provider trait and Claude adapter

Date: 2026-10-23
Status: accepted

## Context

ADR-0003 settled on one provider-agnostic trait with one adapter per provider, Claude first. Architecture §8 adds constraints: one command at a time, context passed as clearly delimited data because logs and command output may carry prompt injection, and the AI's own opinion of a command's risk never used for classification. The incident loop, context gathering, risk classifier and audit log are separate tasks; they need a stable interface and a provider they can test against without network calls.

## Decision

**Crate.** `crates/ai` (`monitorflow-ai`) holds the trait, the Claude adapter and, behind the `testing` feature, a scripted `MockProvider`. It depends on `monitorflow-common` for the configuration and `Secret`, not on the server.

**Interface.** `AiProvider::next_step(&StepRequest) -> Result<Step, AiError>`. A `StepRequest` is the server's system prompt, the context as titled `DataBlock`s, and the earlier `Exchange`s: the diagnosis, the proposed command with its reasoning, and the outcome (`Executed` with the command that actually ran, which an approver may have edited, its exit code and output; or `NotRun` with a reason). A `Step` is a diagnosis plus `Action::Run(ProposedCommand { command, reasoning })` or `Action::Stop` with `Resolved` or `Escalate` and a reason. There is no risk field: classification is the server's alone. Context and outputs must be redacted before they reach the provider.

**Requests.** The Claude adapter POSTs to the Messages API (`/v1/messages`, `anthropic-version: 2023-06-01`) with reqwest on rustls. Every step is one stateless request: the system prompt is the server's prompt followed by fixed adapter rules; the single user turn holds the context as `document` blocks (plain text, one per `DataBlock`) and the earlier steps as one more document, a JSON array. Untrusted text therefore never sits in instructions, and a log line like `</document>` cannot close anything since documents are separate JSON values. Replaying earlier steps as assistant `tool_use` turns was rejected: current models think by default and expect their own thinking blocks back in such turns, which a provider-agnostic history cannot hold, and the outcome of an edited or rejected command fits poorly in a `tool_result`. The last context document carries `cache_control` so the stable prefix (tools, system, context) is cached across the steps of an incident.

**Answers.** Two strict tools, `propose_command {diagnosis, command, reasoning}` and `stop {diagnosis, outcome, reason}`, with `tool_choice: auto` and `disable_parallel_tool_use`. Forced tool choice (`any`/`tool`) is refused by `claude-opus-5-5`, the default model, so the rules ask for one tool call and the parser rejects anything else. Parsing is strict: exactly one call to a known tool, no unknown or missing field, non-empty trimmed text, the command one line without control characters and at most 4096 bytes. Thinking, text and fallback blocks are ignored. `stop_reason: refusal` is `AiError::Refused` with its category, `max_tokens` is `AiError::Truncated`, every other deviation is `AiError::Malformed`.

**Refusals.** For models known to support it (`claude-fable-5-1`, `claude-opus-5-5`, `claude-opus-5`, `claude-sonnet-5-5`) requests carry `fallbacks: "default"` with the `server-side-fallback-2026-07-01` beta, so a safety-classifier decline is retried by the API on another model. Remediation commands often look like security work, where false declines are likelier. Other models get neither, since they would answer 400.

**Retries.** Up to 3 retries on transport errors and HTTP 408, 409, 429 and 5xx (529 overloaded included), waiting `retry-after` seconds when given, else 1 s doubled per attempt, never more than 60 s. Other 4xx and malformed answers are not retried. Errors keep the API's error type, a truncated message and the `request-id`, never the key or URL.

**Configuration.** `[ai]` in the server config, disabled by default: `provider = "claude"`, `api_key_file`, `model` (default `claude-opus-5-5`), `max_tokens` (16000), `request_timeout` (300 seconds), each with a `MONITORFLOW_AI_*` override. The key is read from the file at start, trimmed, held as a `Secret` and sent as a sensitive header; it is never accepted inline. `api_key_file` is required only when enabled.

## Consequences

- The incident loop codes against `dyn AiProvider` and tests with `MockProvider`; adding a provider is one adapter and one `AiProviderKind` variant.
- Each step resends the whole context; prompt caching keeps that cheap when the context is large enough to be cached, and the per-incident command limit bounds the history.
- The AI's earlier diagnoses and reasoning are sent back as data too, so text an injection planted in them stays out of instructions.
- Multi-line commands (heredocs) are refused; the AI must use one-line forms. The risk classifier still sees a single line.
- Retry waits have no jitter; one server makes few concurrent AI requests.
- Effort, thinking display and streaming are left at API defaults; a 300-second timeout and 16000 tokens suit non-streaming answers. Streaming would be needed for much larger `max_tokens`.
