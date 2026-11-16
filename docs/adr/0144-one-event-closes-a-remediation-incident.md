# ADR-0144: One event closes a remediation incident

Date: 2026-11-17
Status: accepted

## Context

ADR-0043 closes a remediation incident with two events in one transaction: an `escalation` (escalated, failed) or a `note` (resolved) with the reason, then a `state_change`. The audit log showed both in the same second, "Escalation" then "State change: running → escalated", saying the same thing twice. When a command did not complete, the `command_result` said why, and the `escalation` right after quoted the same error again.

The reasons were English sentences built by the server, like "AI commands are off on this server; ...". The web interface showed them as they were, in English on a French page.

## Decision

**The closing event carries the state change.** Closing appends one event, still an `escalation` or a `note`, whose payload adds `from` and `to`. No `state_change` follows it. `state_change` events keep recording every other move (open to running, running to awaiting approval and back). The trail loses nothing: the closing event says what changed, why and when, in the same chain.

**A stable code next to the sentence.** The closing payload keeps `reason` (or `text` for a note) word for word, so existing clients and integrations read the same thing, and adds `code` (`sensor_normal`, `duration_limit`, `command_limit`, `ai_escalated`, `not_fixed`, `alert_gone`, `context_failed`, `ai_failed`, `forbidden_proposal`, `approved_forbidden`, `needs_approval`, `commands_off`, `commands_off_windows`, `system_unsupported`, `agent_too_old`, `snapshot_failed`, `command_gone`, `timed_out`, `incomplete`), with `detail` for words the client cannot translate (an error, a rule's reason, the AI's words, a command) and `limit` for the number in a limit. Codes are only ever added. For `incomplete`, `detail` is left out: the `command_result` just before has the error.

A `command_result` the server writes itself, when no result came in time, adds `error_code: "no_result"` and `limit` (the seconds waited) next to its English `error`.

**The web interface words them.** It translates the codes and a known agent refusal ("shell commands are disabled on this agent"), and falls back to the English sentence for events written before this change or codes it does not know yet. Text from the AI model stays in its own words.

## Consequences

- Incidents closed before this change keep their two events; the chain is append-only and is not rewritten.
- Filtering the audit log on `state_change` no longer shows closings; filter on `escalation` and `note`, or read `to` in their payload.
- Supersedes the "appends the `state_change`" part of ADR-0043's closing paragraph; the rest of ADR-0043 stands.
