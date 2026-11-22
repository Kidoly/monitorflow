# ADR-0165: New AI remediation attempts on an alert still open

Date: 2026-11-22
Status: accepted

## Context

The incident loop opens one remediation per alert, ever, when the alert opens (ADR-0043). Once that remediation closes, nothing starts another while the alert stays open, and no API or screen can.

In production this left an alert with nothing to act on. A critical disk alert opened; its remediation sent `df -h /`, the agent answered that shell commands were disabled (`commands.shell`), and the remediation escalated ten seconds after it opened. The administrator then allowed `commands.from_web` on the host and turned AI commands on from the server's page (ADR-0098); the agent reported them accepted (ADR-0076). The alert was still open weeks later, its remediation escalated, the approval queue empty, and the incident page said "escalated" with no way forward. The owner's position: AI should not run only when a new alert opens.

Three causes leave an alert in that state: the remediation stopped because the server refused AI commands (the server's report said so before the command was queued, or the agent refused it); it stopped for another reason (a limit, the AI handing over, a failed context); or it never started (AI was off when the alert opened, and the alert is older than the hour after which the loop leaves alerts alone, ADR-0043 and ADR-0064).

## Decision

### One remediation per attempt

A new attempt is a new remediation incident of the same alert, with its own hash chain and the limits of `[remediation]` when it opens. The database already allows it: the unique index covers active remediations only (`open`, `running`, `awaiting_approval`), so an alert has at most one in progress and any number closed. No chain is reopened or extended, so ADR-0042 holds as it is.

The first event of an attempt is a `note` with `code` `attempt`, appended in the transaction that opens it:

```json
{"text": "attempt 2, started by a person, after the attempt before was escalated",
 "code": "attempt", "trigger": "person", "attempt": 2,
 "previous": {"id": "...", "state": "escalated", "code": "incomplete", "reason": "..."}}
```

`trigger` is `person` (the actor is the user) or `commands_accepted` (the actor is the system). `previous` is how the attempt before ended, from the event that closed it, or `null` for an alert's first. The loop skips notes when it reads events, so an attempt then starts like any remediation, with its context.

Opening an attempt locks the alert's row, reads its latest remediation again and refuses when it is in progress, when the alert resolved, or when the alert has no agent to run commands (a device, ADR-0148, or the `offline` alert, ADR-0072). The loop also passes the remediation it saw, and gives up when another started since. Two server processes, or a person and the loop, therefore never open two attempts after the same one; the unique index backs that up.

### By itself, once the server accepts AI commands

Each poll, the loop looks for open alerts whose latest remediation escalated because the server refused AI commands:

- its closing event has one of the refusal codes of the server's report (`commands_off`, `commands_off_windows`, `system_unsupported`, `agent_too_old`), or
- it closed as `incomplete` (or without a code, before codes existed) right after a `command_result` whose error is the agent's own refusal, which starts with "shell commands are disabled on this agent" on every system.

It opens an attempt when the server has accepted AI commands since that remediation ended. **`servers.shell_accepted_at`** says when: a trigger sets it when the report for `shell` becomes `accepted`, clears it on a reported refusal (`off`, `unsupported`), and leaves it alone while the report is unknown, so an agent moving behind an older relay and back starts nothing. Every path that stores the report (ADR-0076) moves it, including the reconnection that follows the web interface's switch (ADR-0098).

**Once per change.** The new attempt opens after `shell_accepted_at`, so when it stops on a refusal again its end is later, and nothing starts until the server goes from refusing to accepting once more. Each attempt keeps its own command and duration limits.

**AI turned on.** The loop only runs while AI is on, so a server that started accepting commands while AI was off gets its attempt when AI is turned on.

**After an upgrade.** The migration sets `shell_accepted_at` to its own time on servers that accept AI commands, so a remediation that stopped on a refusal before this version, on an alert still open, gets its one attempt; that is the production case above.

Other endings get no automatic attempt: a limit, the AI handing over or a forbidden command would most likely end the same way, and trying again by itself would spend AI calls and commands on a guess. A person decides, below.

### By hand: "Run AI again"

`GET /api/remediation/alerts/{id}` says where an alert's remediations stand and whether the caller may start one: `attempts`, the latest one and its state, `can_run`, and `blocked` with a stable code and an English `reason`. `POST /api/remediation/alerts/{id}/run` opens an attempt, 201 with its ID and number.

- **Who**: approvers and administrators of the alert's site, the roles that let commands run there (ADR-0045). Others get 403, or 404 for an alert they cannot see. Reading the status needs any role on the site. Both are in the `remediation` area of API keys.
- **Refused**, 409 with the reason: the alert is resolved, or on a device or an offline host; an attempt is running or waits for approval; AI is off; the server reports refusing AI commands. The last is said before running, with how to allow them, since the first command would only come back refused. An alert that never had a remediation can get its first this way.
- **Audit**: the `attempt` note in the new chain, with the user as actor, and `remediation.run` in `audit_log` with the alert, the attempt's number and the one before.

The incident page shows the latest attempt with its number when there are several, links each earlier one to its events in the audit log, and offers "Run AI again" (French "Relancer l’IA"), or "Run AI" for an alert that never had one, on an open alert. Where it cannot run for a reason a person can act on (AI off, the server refusing AI commands, no agent), the button is disabled and the reason is beside it; while an attempt runs, or for someone who may not start one, there is no button. The audit log words the note: "Attempt 2, started by hand. The one before: The command did not complete".

### The AI learns how earlier attempts went

An attempt's context gains a data block, `earlier_attempts`, after the others: up to three earlier attempts on the alert, newest first, each with its state and times, the AI's diagnoses, each command with its exit code or why it did not complete and the end of its output (1 KiB), rejections, and how it ended. It is read from their audit events, which were redacted when written, and recorded in `context_sent` like the rest of the context, so the trail shows what the AI was told. The context it was sent before is not repeated.

### Alerts that opened while AI was off

They stay as ADR-0043 and ADR-0064 decided: alerts opened within the last hour get a remediation when AI is turned on, older ones do not. Turning AI on over a fleet with many long-standing alerts would otherwise start that many AI conversations and read-only commands at once, on problems people may already handle. "Run AI" now covers any of them one by one.

Rejected:

- **Reopening the escalated remediation.** A closed chain would change state again, and its limits, already spent, would have to be raised or ignored.
- **Retrying every escalation after a delay.** Most endings are not transient; it would loop on alerts that need a person, which ADR-0043's limits exist to stop.
- **Detecting the change from the commands' history.** It needs a refused command first and cannot tell a change from a stale report; the report's own transition is what changed.
- **Starting attempts from the web switch's command.** It covers ADR-0098 only, not `MF_ALLOW_COMMANDS=1` or a Windows agent turned on locally; the report covers all of them.

## Consequences

- An alert whose remediation stopped because AI commands were off gets a new attempt by itself once the server accepts them, without a new alert. An operator can start one on any open alert whose remediation ended, or that never had one.
- An alert can have several remediation incidents. Clients that read `GET /api/remediation/incidents?alert_incident_id=` with `limit=1` still get the latest.
- `servers` gains `shell_accepted_at` and a trigger (migration `20261202140000`). The API's settings carry `[remediation]` for the limits of an attempt a person starts.
- The agent's refusal is matched by its English start. A future agent that words it differently loses the automatic attempt after an `incomplete`, not the manual one; the server's own refusal codes are unaffected.
