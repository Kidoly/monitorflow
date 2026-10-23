# ADR-0042: Remediation incidents and their audit trail

Date: 2026-10-26
Status: accepted

## Context

Architecture §8 requires every AI remediation to keep, per incident, the context sent to the AI, every proposal, who approved it, the exact command, its output and exit code. The incident loop (a later task) needs a model to write to: an incident with a state, the limits that make it escalate, and an ordered history that cannot be rewritten without anyone noticing. Two things already exist:

- `audit_log` (initial schema): one global append-only table, `actor_kind`/`actor_id`/`action`/`target`/`details`, protected by triggers, written by the API for configuration changes.
- `incidents` and `incident_events` (ADR-0017, ADR-0019): the threshold engine's alerts, owned by the engine only, served by `GET /api/incidents`.

## Decision

**Separate tables, not `audit_log` rows.** Migration `20261026090000_remediation_incidents` adds `remediation_incidents` and `remediation_incident_events`. `audit_log` stays the account of who changed configuration. Remediation history is a different thing, for these reasons:

- It needs a dense per-incident sequence and a hash chain. In `audit_log` that means nullable columns that existing rows lack, and a sequence per target inside a global table.
- Chaining needs a lock per chain. On `audit_log` the natural lock is global and would serialize every audited request. Here it is the incident's row.
- Payloads are large (redacted context, up to 64 KiB of output per stream) and are read per incident in order. `audit_log` holds short details read by time.
- The interface reads one incident's events in full. With a separate table that is a primary-key range scan, with no filtering of unrelated rows.

The two can be joined by the incident ID if the loop also writes summary rows (`remediation.approve`, ...) to `audit_log`. That is the loop's choice.

**Not the threshold engine's tables either.** `incidents`/`incident_events` are the engine's (ADR-0019 keeps them engine-only; its in-memory state assumes its own rows), cascade away with their server, and their states are `open`/`resolved`. A remediation incident points to the alert that opened it (`alert_incident_id`, with `metric` and `instance` copied so they survive) and has its own life. At most one active remediation per alert is enforced by a partial unique index. For the same reason the API lives at `/api/remediation/incidents`, beside ADR-0041's `/api/remediation/rules`, since `/api/incidents` is taken.

**Incident row.** `customer_id` (checked by trigger to be the server's customer, so one incident never mixes customers, §8), `server_id`, `alert_incident_id`, `metric`, `instance`, `state` (`open`, `running`, `awaiting_approval`, `resolved`, `escalated`, `failed`; `closed_at` set exactly for the last three), `opened_at`, `closed_at`, and the limits snapshot `max_commands`, `max_duration_s`, fixed at opening. Only `state`, `closed_at` and the chain head may change. A trigger refuses DELETE, TRUNCATE and updates of other columns. Foreign keys do not cascade, so history blocks the deletion of its server instead of disappearing with it.

**Events.** `(incident_id, seq)` is the primary key, `seq` from 1 without gaps. `kind` is one of `context_sent`, `ai_proposal`, `classification`, `approval`, `rejection`, `edit`, `command_queued`, `command_result`, `snapshot`, `escalation`, `state_change`, `note`. `actor_kind`/`actor_id` use `audit_log`'s kinds: a user ID, `system`, the AI's model, or a peer ID. Then `payload` (a JSON object, at most 1 MiB), `created_at`, `prev_hash` and `hash`. Triggers refuse UPDATE, DELETE and TRUNCATE, like `audit_log`.

**Hash chain.** `hash = SHA-256(prev_hash || canonical)`. `prev_hash` is 32 zero bytes for the first event. `canonical` is `remediation_event_canonical(...)`: the text of a `jsonb` array `[1, incident_id, seq, kind, actor_kind, actor_id, created_at, payload]`, where `1` is the format version and `created_at` is fixed-format UTC with microseconds. PostgreSQL prints a `jsonb` value the same way every time (sorted keys, normalized numbers), so the server does not have to reproduce PostgreSQL's numeric and key handling in Rust. The incident row holds `head_seq` and `head_hash`, so deleting the newest events is visible too.

**Who computes what.** A `BEFORE INSERT` trigger locks the incident row, refuses an event whose `seq` or `prev_hash` does not follow the head, computes `hash` (overwriting what the INSERT said) and moves the head. `remediation::audit::append` takes the same row lock first (`SELECT ... FOR UPDATE`) and assigns `seq = head_seq + 1` and `prev_hash = head_hash`. Concurrent appends therefore queue on the lock and get consecutive numbers, and a writer that skips the module still cannot break the chain. `append_in` does the same inside a caller's transaction, so an event commits with the state change or queued command it describes.

**Verification.** `remediation::audit::verify` reads the head and the rows in one repeatable-read snapshot, recomputes every hash in Rust (ring's SHA-256) from the canonical text, and reports the first broken `seq` with a reason: `gap`, `prev_hash`, `hash` or `head`. SQL `remediation_verify_chain(incident)` does the same for operators. `GET /api/remediation/incidents/{id}` runs it on every request and logs an error when it fails.

**Privileges.** The repository has no database role model. Like `audit_log`, production must grant the application role only INSERT and SELECT on `remediation_incident_events` (and INSERT, SELECT, UPDATE on `remediation_incidents`). Triggers stop the application. The chain shows what a table owner or superuser did with triggers off.

**API, read-only.** `GET /api/remediation/incidents` (filters `state`, `customer_id`, `site_id`, `server_id`, `alert_incident_id`; keyset pages newest first, `limit` 1 to 500) and `GET /api/remediation/incidents/{id}` (the item plus `events` and `chain`), scoped by readable sites as for `/api/incidents`: anything outside them is 404 or an empty list. There is no write route. The incident loop creates incidents with `remediation::audit::open`.

## Consequences

- Rows changed, removed or reordered in the middle of a chain, and a removed tail, are detected. Someone who can rewrite every row and the head consistently (a superuser recomputing the chain) is not. Anchoring head hashes outside the database (a periodic signed export, or head hashes copied to logs) is the follow-up that closes that gap.
- The canonical form depends on PostgreSQL's `jsonb` text output, which has been stable since 9.4. A change there would show up as mass verification failures after an upgrade, and the version element allows a format 2 then.
- Callers must redact payloads before appending (ADR-0040). Events are kept forever and shown in the interface.
- History pins its server and alert rows: deleting a server with remediation history fails. Servers are revoked, not deleted, so nothing does that today.
- Verifying on every detail request reads the whole chain twice. Fine for tens of events per incident. A cached result keyed by `head_seq` can come later.
