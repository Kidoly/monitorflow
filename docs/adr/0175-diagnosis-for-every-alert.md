# ADR-0175: A diagnosis for every alert, commands where allowed

Date: 2026-11-23
Status: accepted

## Context

AI is what sets MonitorFlow apart, and its owner could not get anything out of it. They set up a provider under Settings › AI, incidents opened, and as far as they could see the AI did nothing: no diagnosis, no command.

We followed a new user's path on a local stack, with a fake model server instead of a real key. A first administrator, AI on with a local model, one server added with the default command (no `MF_ALLOW_COMMANDS`, no `MF_AI_COMMANDS_FROM_WEB`), and a disk threshold crossed. Each dead end we met:

1. **The AI was asked, but to run commands.** The loop sent the server's context with the prompt of ADR-0043: "diagnose it and fix it, one shell command at a time". The model proposed `df -h /` with a first guess as its diagnosis. The rules classified it read-only, the server's report said AI commands were off (ADR-0076), and the incident escalated in the same second with `commands_off`. The AI never gave a diagnosis it meant as one, because it was waiting for that command.
2. **The incident page showed none of it.** "AI remediation · Escalated · 6 events · opened 23:55 · closed 23:55". The diagnosis was only in the audit log, as an event among six.
3. **What it said to do was out of date.** "Run the installer there with MF_ALLOW_COMMANDS=1", in the escalation, the audit log and next to "Run AI again". The server's page had offered a switch since ADR-0098; the sentence did not point there.
4. **"Run AI again" was disabled** on such a server, so a person could not even ask for a diagnosis.
5. **Add servers left "Allow AI commands to be turned on from the web interface" unticked.** Every server added that way can have AI commands turned on only by logging in to it and running the installer again.
6. **Devices and offline hosts never had a remediation**, and their incident page said "No agent can run commands for this alert" or nothing.
7. **An alert opened before AI was on, or while it was off, showed nothing at all**: the AI block appears only once a remediation exists or "Run AI" is possible.
8. **Settings › AI said "AI is on." and "No server accepts AI commands."**, which reads as "AI does nothing here". The approval queue said "The AI looks into each new alert on servers that accept AI commands". The audit log, empty, said "No remediation has run yet."

The owner decided: the AI diagnoses every incident from what the central server already holds, on every server, devices and offline hosts included, without running anything on them; where a server accepts AI commands, today's behaviour continues on top. The web consent is ticked by default on Add servers. The rest of ADR-0098's boundary stays: no shell without the host's own consent.

## Decision

### Where no command can run, the AI diagnoses

When the loop gathers an incident's context (ADR-0040), it decides once whether commands can run there:

| Machine | Commands | Code |
| --- | --- | --- |
| A server whose agent reports refusing AI commands | No | the refusal's: `commands_off`, `commands_off_windows`, `system_unsupported`, `agent_too_old` |
| A device (ADR-0148) | No | `device` |
| The `offline` alert of a server (ADR-0072) | No | `offline` |
| Any other server, including one whose agent never reported | Yes, as before | none |

The answer is recorded in the `context_sent` event as `diagnosis_only: {code, reason}`, so a resumed incident reads it from its events like everything else, and a report changing midway does not change the incident.

Where no command can run, the AI gets other instructions: no command can run there, and why; diagnose from the context alone (metric history, logs, recent changes and commands, packages, inventory); give the likely cause and the evidence, or say what is missing; stop with `escalate`, giving in the reason what a person should check or do next, commands to run by hand included. The data rules of ADR-0039 are the same text in both prompts. The tools offered do not change: a provider sees the same two, and nothing in `monitorflow-ai` changes.

Its answer closes the incident, escalated, with one event: the code of the table above, `diagnosis_only: true`, and the sentence "the AI diagnosed the incident without commands: ...". A command it proposes anyway is kept as its words in its `ai_proposal` event, and is neither classified nor queued: no rule runs on it, no snapshot is taken, nothing reaches an agent. A stop with `resolved` waits for the sensor as before.

**Keeping the refusal's code** is what lets ADR-0165 work unchanged: the remediation ends with `commands_off`, so when the server starts accepting AI commands, its alert gets its new attempt by itself, this time with commands.

**Devices and offline alerts** now get a remediation like any alert. A relay's own offline incident has no server and still gets none. ADR-0080's grouping does not limit AI calls: a site losing its link opens one diagnosis per server, each one AI call, bounded by the loop's concurrency (16 per process).

### What the API says

`GET /api/remediation/incidents` and `/{id}` add to each remediation incident: `diagnosis` and `diagnosed_at` (the AI's latest, from its latest `ai_proposal`), `ai_advice` (the reason it stopped with, or the reasoning of its command), `ai_command`, `diagnosis_only` (the code above), and, once closed, `close_code`, `close_reason`, `close_detail` and `close_limit` from the event that closed it. Texts from the AI stay untrusted.

`GET /api/remediation/alerts/{id}` no longer blocks a run for a device, an offline host or a server that refuses AI commands (`no_agent` and `commands_refused` are not returned any more): the attempt diagnoses. It adds `diagnosis_only` and `diagnosis_only_reason`, what an attempt started now would do, keeps `refusal` filled whenever the server refuses, and adds `no_attempt` with `no_attempt_reason` on an open alert that never had a remediation: `ai_off`, `too_old` (opened more than an hour before AI looked at it, ADR-0165) or `soon`. `POST .../run` starts an attempt in these cases.

All of it is in `/api/v1` and its OpenAPI document; nothing on screen is missing from it.

### On screen

**The incident page** shows the AI block right under the figures, before the chart, on every open alert it can read:

- the latest attempt's state, "Diagnosed" (French "Diagnostiqué") for one that only diagnosed;
- the AI's diagnosis, then "Next:" and its advice, and a command it proposed, labelled "Command to run by hand, not run by AI" where none could run; a note says it was written by the AI from what MonitorFlow already collected, with a link to the audit log;
- one sentence on why it did no more, with the link that changes it: "AI commands are off on web-01. Turn them on" (the server's Commands tab, which has the switch or the line to run, ADR-0098), "a device has no agent to run them", "the host stopped reporting", "The AI provider failed: ... Check Settings › AI", "A command the AI proposed waits for an approver. Review it", or, with no remediation yet, "AI is off. Turn on AI", "it opened more than an hour before AI was on" next to "Run AI", or "AI looks at this alert within seconds";
- "Run AI again", enabled where only a diagnosis is possible, with "The AI will diagnose only: no command can run there."

**The server's refusal** now reads "AI commands are off on this server; the Commands tab of its page turns them on, or gives the line to run there", in the API and both languages.

**Add servers** ticks "Allow AI commands to be turned on from the web interface" by default, adding `MF_AI_COMMANDS_FROM_WEB=1` to the Linux line and `AI_COMMANDS_FROM_WEB=1` to the Windows one. Its note says commands stay off until someone turns them on. Unticking removes both.

**Settings › AI** says, when AI is on, that it diagnoses every new alert from what MonitorFlow already collects without running anything, and that the diagnosis shows on the incident; the section on commands says what they add (read-only checks by AI, fixes for approval) and how to turn them on. **The approval queue** and **the audit log** say the same when empty. The audit log words a diagnosis-only ending "Diagnosis only, handed to a person: ...".

### What does not change

- **No shell without the host's consent.** The consent of ADR-0098 is what a ticked box adds to the install line, given by the person who runs it on the host. AI commands stay off on a new server until an administrator turns them on from its page, which ADR-0098 audits and notifies.
- The agent decides what it runs. Commands are signed, classified by the rules, approved as before.
- The context is redacted before the AI sees it (ADR-0040); the AI reads nothing more than before.

Rejected:

- **A third "diagnose" tool in the provider layer.** Every adapter and its strict schemas would change for what the instructions already say; a proposal made anyway is handled on the server.
- **Running read-only commands without consent.** That is the shell ADR-0098 keeps behind the host's consent, read-only or not.
- **A new state for a diagnosed incident.** `escalated` is what it is: handed to a person with the AI's view. The `diagnosis_only` field tells the two apart, and existing clients keep working.
- **A diagnosis-only code of its own.** It would hide the refusal and break the automatic attempt of ADR-0165.
- **Ticking `MF_ALLOW_COMMANDS=1` by default.** It turns AI commands on at install, without anyone choosing it on the server's page.

## Consequences

- After a provider is set under Settings › AI, every new alert of a server, a device or a host that stops reporting shows the AI's diagnosis on its page within seconds, with what to do next and why the AI did no more.
- Each alert costs at least one AI call, also where none was made before: devices and offline hosts. A site going offline at once makes one call per server.
- Servers added after this version can have AI commands turned on from their page; older ones still need the line their page gives.
- `RemediationRunStatus` no longer returns `no_agent` and `commands_refused`; clients that handled them keep working, as they only stop appearing.
- No migration: the new fields are read from existing events.
