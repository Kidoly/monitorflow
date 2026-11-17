# ADR-0146: Moving from another monitoring tool

Date: 2026-11-17
Status: accepted

## Context

Most people who try MonitorFlow already watch their servers with something else. Typing again hundreds of hosts, checks, thresholds, maintenance windows and notification addresses is the largest cost of switching, and doing it by hand loses details. The roadmap asks for an import that reads the other tool's configuration (an export file or its API), shows first what maps exactly, what is approximated and what cannot be moved, creates nothing until confirmed, and leaves the old tool running until the switch. Servers then only need the agent.

## Decision

**A neutral description in between.** Readers turn one tool's configuration into an `Inventory` (`crates/import`, `monitorflow-import`): hosts (name, addresses, system hint, groups), checks (what is watched, how often, the alert when it fails), thresholds (measure, above or below, warning and critical, how long, the recovery value), silences (what they cover, from, to, why), notification channels (email, webhook, Slack, Teams, Discord) and what cannot be moved. Each item keeps where it came from in the source (kind, name, the key or expression) and how faithfully MonitorFlow does the same: exact, or approximated with a note. What cannot be moved has a note saying why. Notes are codes with arguments (`crates/import/src/notes.rs`), shown in English and French from the web interface's catalog; API clients get the English text beside the code. Readers know nothing of the database; the server knows nothing of other tools' formats. A new tool is one module implementing `Source` and its notes.

**What maps to what.** Measures that every server has already (processor, memory, disks, disk activity, the agent's link) need no new check: they are listed as exact, with nothing to create. A threshold on them becomes a threshold row of the server. Services, TCP ports and log patterns become checks of the existing kinds, with the alert that comes with them. A web check becomes a TCP check of the URL's host and port, marked approximated. Maintenance windows become silences of the servers they cover; a recurring one gives its next window, and a window longer than 30 days is cut (ADR-0079), both marked. Channels keep their secrets sealed (ADR-0125) from the upload to the confirmation, and are never shown back.

**Preview first, then confirm.** `POST /api/imports` reads the input, plans, and stores the run with its items as a preview: nothing else is written. Each item says what it creates, in which list it falls (exact, approximated, cannot be moved), whether MonitorFlow has it already, and which items it needs (a check needs its server). `POST /api/imports/{id}/confirm` creates the ticked items in one transaction; unticking a server unticks what needs it. A preview expires after a day.

**Idempotent.** Each item has a natural key: a server's ID, a check's server and name, a threshold's server, metric and instance, a silence's server, period and reason, a channel's kind and destination. The planner marks what exists already, and the confirmation inserts with the same keys and skips what appeared since. Importing the same configuration twice creates nothing the second time; confirming the same preview twice is refused.

**Undo.** Every created row is recorded with its run. `POST /api/imports/{id}/undo` removes what the run created and still exists: checks, thresholds, silences, channels, and servers still waiting for their agent. A server an agent has taken over stays, with what was created on it, and the answer says so: removing it would revoke a working machine. Preview, confirmation and undo are audited (`import.preview`, `import.confirm`, `import.undo`) with counts, never secrets.

**Expected servers.** An imported host becomes a `servers` row with `awaiting_agent`: its ID comes from its name as install keys make one (ADR-0073), it has no certificate, and it belongs to the site chosen for the import. Its checks, thresholds and silences are ordinary rows on it, visible and editable like any other. The offline alert skips it, the fleet shows it as waiting for its agent with the install command, and integrations hear of it when an agent arrives, not before. An agent takes it over when it enrolls with that ID: a token made for it, or an install key of the same site whose host reports the same name, which the key then gives this ID instead of a new one. The first Hello finds the row, fills it and clears `awaiting_agent`; the audit log records `server.agent_arrived`. Nothing else changes in enrollment: the rules of ADR-0105 on IDs, roles and sites apply as before, so an expected server in one site keeps its ID from enrollments in another.

**Who may.** An administrator of the target site. Channels cover everything when a global administrator imports, the site otherwise, as the channels page does (ADR-0078).

**Connections.** Reading through another tool's API takes its URL and a token typed for this one request: it is never stored or logged. The server makes the requests, http or https, without following redirects, with a timeout and a cap on answers, and reads only.

**Everything through the public API.** Sources, preview (upload or connect), the run, confirm and undo are under `/api/v1/imports` and in the OpenAPI document; the web interface uses the same routes (ADR-0143).

## Consequences

- A person sees, before anything is created, what will be the same, what differs and why, and what stays behind.
- Readers are small and testable against fixtures; the server's planner and its tests do not change for a new tool.
- `servers` gains a state that is not an enrolled agent. Code that lists servers shows it; code that talks to agents never finds one connected, so it waits as it does for an offline server.
- The old tool keeps running: nothing is written to it, and silences or channels are copies, so both send until it is switched off.
