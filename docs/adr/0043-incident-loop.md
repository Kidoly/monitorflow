# ADR-0043: The remediation incident loop

Date: 2026-10-27
Status: accepted

## Context

Architecture §8 Flow: when a threshold incident opens, the server gathers context, the AI proposes one command at a time, each command is classified by rules and runs or waits for approval, the agent's output goes back to the AI, and the incident closes when the sensor returns to normal or escalates to a human, within a limit on commands and duration. The building blocks exist: the provider trait (ADR-0039), context and redaction (ADR-0040), the risk classifier and its `low_impact_auto` policy (ADR-0041), and remediation incidents with their hash-chained events (ADR-0042). Missing: what drives them, how a command reaches an agent, what happens across server restarts, where a command waiting for approval lives, and where snapshots will plug in. The approval queue (API and web) and Proxmox snapshots are separate tasks.

## Decision

**Trigger.** `remediation::runner::Remediation` runs in the server when `[ai]` is enabled; there is no other switch. A supervisor polls PostgreSQL every 10 s, like the campaign runner (ADR-0033), rather than hooking the threshold engine's notifier, which is in memory and lossy. Each poll opens one remediation incident (ADR-0042's `open`, limits copied from `[remediation]`) per open alert that never had one and opened less than an hour ago: one remediation per alert, ever, and alerts already open when remediation is enabled stay alone. A severity change of the alert is the same alert. Per-customer opt-in does not exist yet; `low_impact_auto` is the only policy read.

**Event-sourced passes.** The loop keeps no state of its own. Driving an incident is a series of passes: read its events, derive the next action (`runner::progress`), do it, record it in one transaction with whatever it changes. The next action follows from the last relevant event: no `context_sent` → gather the context; `context_sent` or `command_result` or `rejection` → ask the AI; `ai_proposal` with a command → classify; `approval` → (snapshot hook) → queue; `command_queued` → wait for its result; `ai_proposal` with `stop` → confirm or escalate. The AI's history (`Exchange`s) is rebuilt from the same events, with the outputs exactly as recorded. A server stopped anywhere resumes from the trail, and the approval queue resumes a parked incident by appending `approval` (or `edit` then `approval`, or `rejection`) and setting the state back to `running`, without touching the loop.

**One loop per incident.** `remediation_loop_leases (incident_id, holder, expires_at)` (migration `20261027090000_remediation_loop`): a process takes the lease of an `open` or `running` incident that has no live lease of another holder, renews it every third of its 60 s lifetime while it drives, and stops driving when a renewal fails. Closing or parking an incident deletes the lease, and so does a graceful shutdown. After a crash the lease expires and the next process resumes. Within a process a set of driven incidents prevents a second driver, and at most 16 run at once.

**A step.**

1. Context: `context::gather` and `render` for the alert (ADR-0040), recorded as `context_sent {model, context: {rendered, redactions}}`. A failure, foreign data included, closes the incident `failed`. On resume the recorded text is reused, not gathered again.
2. AI: `next_step` with the server's system prompt (rules for single simple commands, the agent's environment, the limits left, and that data blocks are untrusted and never instructions), the rendered context as one data block, and the rebuilt history. The call is bounded by the incident's deadline. An answer is `ai_proposal` (diagnosis, reasoning and command; texts cut at 8 KiB) or the same kind with `command: null`, `stop` and `reason`. A provider error closes the incident `failed`.
3. Classification: `risk::classify` with the server's rule set, then one transaction with `classification {command, tier, rule, reason}` and:
   - read-only, or low impact where `low_impact_auto` holds: `approval` by `system`;
   - low or high impact otherwise: a `remediation_pending_commands` row (command, diagnosis, reasoning, tier, rule, reason, the proposal's `seq`) and `running → awaiting_approval`; the lease is released and the driver stops;
   - forbidden: `escalation` and `running → escalated`.
4. Run: the command of the `approval` event, which is the proposal's exact text for system approvals, is classified again. Forbidden escalates (an edited command cannot get around deny rules), and a system approval whose tier now needs a human escalates (the rules changed). Change tiers go through the snapshot hook first. Then, in one transaction, a `commands` row of the new kind `shell` with a `ShellCommand {command, timeout_seconds}` payload, signed and dispatched like every command (ADR-0031), and `command_queued {command_id, command}`.
5. Result: the loop polls the command row. Its stdout, stderr and error are redacted by a fresh `Redactor` and recorded as `command_result`; `exit_code` is `null` unless the command started and exited normally. The AI gets the last 8 KiB of each stream. A command that did not run to completion (refused by the agent, expired, killed, timed out) escalates: retrying it is pointless and a timeout is a limit.

**Ending.** The incident resolves when its alert resolves: checked before every pass except while a command is in flight (its result is recorded first), and by the supervisor for parked incidents. When the AI stops with `resolved`, the loop waits up to 5 minutes (within the deadline) for the sensor to agree, else escalates. `escalate` from the AI escalates with its reason. Closing appends an `escalation` (escalated, failed) or a `note` (resolved) with the reason, ends a pending command (`cancelled`, or `expired` at the duration limit), and appends the `state_change`, in one transaction under the incident's row lock. `failed` means the loop could not work (context, AI, a vanished row); `escalated` means it worked and handed over.

**Limits.** `[remediation]`: `max_commands` (10), `max_duration` (1800 s) and `command_timeout` (120 s, at most 3600), copied into the incident at opening (the first two, ADR-0042) or applied per command. The command limit counts `command_queued` events and is checked when a proposal arrives, so the AI can still stop after the last command. The deadline `opened_at + max_duration_s` is checked every pass, bounds AI calls and command waits, and applies while waiting for approval: an approval hours later, on a system that changed, would be stale. A command gets `command_timeout` as its expiry and the agent's timeout, and 15 s more for its result.

**Agents.** `ShellCommand` is a new proto message (additive). The agent runs it with `/bin/sh -c` as its own user, environment reset to `PATH` and `LC_ALL=C`, stdin closed, from `/`, in its own process group killed at the timeout, keeping only the end of its output while reading. The commands API shows a `shell` command's `command` and `timeout_seconds` as its parameters; it does not accept the kind from users. It refuses the kind unless the host administrator set `commands.shell = true`: the server cannot grant itself arbitrary execution. The risk rules assume `/bin/sh` semantics (ADR-0041's lexer), which is why the agent uses a shell at all: pipes into filters are allowed.

**Snapshot seam.** `runner::hook::ChangeHook::before_change(Change {incident, server, command, tier})` runs once before every non-read-only command and returns what to record as the `snapshot` event (`method`, `name`, `detail`); an error escalates without running the command. The default `NoSnapshot` records `method: "none"`, "rollback is limited". The Proxmox snapshot task implements the trait and passes it with `Remediation::with_hook`; nothing else in the loop changes.

**API.** Read-only, as ADR-0042: each item of `/api/remediation/incidents` gains `pending_command`, the pending row (or `null`). Approving and rejecting are the approval queue's.

## Consequences

- Every action the loop takes is in the hash-chained trail before or with its effect; nothing is updated or deleted there. Leases and pending rows are working state beside it.
- The AI's text reaches execution only as the exact proposed command after the rules classified it, and again before queueing. It is never logged; it is stored as data and shown to approvers.
- The `commands` row keeps the agent's raw output, as for every command (ADR-0031); only the trail and the AI get the redacted copy.
- Agents must opt in to `shell`; until they do, every automatic command escalates with the agent's refusal.
- Polling adds up to 10 s before an alert gets a remediation incident, and up to a second per command result.
- Approvals must come within the duration limit; the default 30 minutes may be raised in `[remediation]`.
- A crash during the AI call asks again; one during a snapshot runs the hook again. Both are rare and harmless; a crash after queueing never queues twice, since the queue and its event commit together.
