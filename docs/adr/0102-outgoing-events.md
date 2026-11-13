# ADR-0102: Outgoing events for integrations

Date: 2026-11-07
Status: accepted

## Context

Phase 16 asks MonitorFlow to fit into the tools people already use: a PSA such as HaloPSA that opens a ticket per incident, an automation that adds a server to an inventory, a script that closes a change when a campaign succeeds. These are machines. They need every event of the kinds they asked for, once each even when sent twice, in an order that makes sense, and a way to catch up after their own outage.

Notification channels (ADR-0060, ADR-0077, ADR-0078) are for people. They deliver at most once and drop what is a day old, group a customer's alerts into one message (ADR-0080), hold events back during a silence (ADR-0079), and send only what is worth waking someone for: no acknowledgements, no server added, no update that succeeded. Their generic webhook is signed, but a receiver cannot ask for a missed event again, and a lost message during a restart is accepted. Those choices are right for people and wrong for a ticketing system, so integrations cannot simply be another channel kind.

## Decision

### Integrations are their own outbox, fed from the same places

**Event sources are shared, delivery is not.** Integration events are written by database triggers on the tables where things happen, the same tables the notification triggers read (`incident_events`, `remediation_pending_commands`, `commands`, `relay_commands`, `campaigns`), plus `servers`. They reuse the notification helpers (`notification_place`, `notification_ref`) and the object shapes of the notification webhook (`customer`, `site`, `server`, `relay`, `incident`, `approval`, `update`), so the place of an event and its objects are computed once, by one function, and a receiver that reads one payload reads the other.

They go to a second table, `integration_events`, not to `notification_outbox`. The notification outbox is claimed once, held by silences, grouped, copied per channel and person, and pruned after a week; integration delivery needs per-endpoint state, retries over a day, and replay over a month. One table serving both would make every notification rule an exception for integrations, or the reverse.

Rejected:

- **A trigger on `notification_outbox` copying its rows.** It would inherit what that outbox leaves out (acknowledgements, successes, deescalation rules), and its follow-up and per-person copies would need filtering.
- **Integrations as a sixth channel kind.** Channels deliver at most once and group; endpoints need at least once and one request per event.
- **Calling an emitter from each code path.** Rejected by ADR-0060 for the same reason: triggers cover every writer, including future ones.

### Events, version 1

| Event | Written when |
| --- | --- |
| `incident.opened`, `incident.escalated`, `incident.acknowledged`, `incident.resolved` | a row of that kind in `incident_events`; deescalations are not sent |
| `server.added` | a server enrolls, or a removed one comes back (ADR-0062), with `"restored": true` |
| `server.removed` | a server is removed (`revoked_at` set) |
| `server.offline`, `server.online` | a server's offline incident (ADR-0072) opens, or resolves because it is back online |
| `update.started`, `update.succeeded`, `update.failed` | an `apply_updates` or `self_update` command outside campaigns, or a relay's `self_update`, starts running, succeeds, or ends any other way |
| `campaign.started`, `campaign.succeeded`, `campaign.failed` | a campaign starts running, completes, or ends any other way |
| `approval.waiting`, `approval.decided` | a command the AI proposed waits for approval, or is approved, rejected, expired or cancelled |

`failed` means "did not succeed": the object's `state` says how (`failed`, `expired`, `cancelled`, `stopped`), so a receiver always sees an end after a start. An offline incident is sent both as `incident.*` and as `server.*`: the first for a receiver that tracks incidents, the second for an asset inventory. Relays appear in incident events only; relay inventory events can be added later.

The body:

```json
{
  "version": 1,
  "id": "0b5f7c4e-…",
  "event": "incident.opened",
  "occurred_at": "2026-11-07T10:15:00Z",
  "url": "https://mf.example/incidents/view/?id=7c0e…",
  "customer": { "id": "…", "name": "acme" },
  "site": { "id": "…", "name": "paris" },
  "server": { "id": "…", "name": "web-1" },
  "relay": null,
  "incident": { "id": "…", "metric": "monitorflow_cpu_usage_percent", "severity": "critical", "…": "…" }
}
```

`id` is a UUID, the same for every endpoint, every retry and every replay: receivers de-duplicate on it. `version` changes only if a field changes meaning; fields are only ever added, as in ADR-0060. Each event adds one object (`incident`, `server`, `update`, `campaign`, `approval`); the JSON Schema is `docs/events/v1.schema.json`, a plain file the public API's OpenAPI document (ADR-0101) can reference under `webhooks` without depending on this code. The AI's command in `approval` is untrusted text, as everywhere.

### Endpoints

An **endpoint** is a URL, a name, the events it wants, and what it covers: everything, one customer or one site, with `notify::Coverage` and ADR-0078's rule unchanged: an administrator manages the endpoints of what they administer, and others do not exist for them (404). They live under Administration › Integrations.

- **The signing secret is made by the server**, 32 random bytes shown once when the endpoint is created and when it is rotated, never again. People are bad at choosing secrets and a receiver needs one anyway. Rotating replaces it at once; there is no overlap period, which would be one more thing to explain. Saving a new URL rotates it too, whoever saves it: the secret stays with the receiver it was given to, as a notification channel's does (ADR-0078), or a customer administrator could point an MSP's endpoint at their own server and collect events signed with the MSP's secret.
- **Event types** are chosen per endpoint, all of them by default.
- **One customer, no customers.** As everywhere (ADR-0104), the page names customers only to someone who sees several: otherwise the choices are Everything, All sites and each site.
- **Private addresses are refused** unless a global administrator ticks "Allow private addresses" for that endpoint. This is stricter than channels (ADR-0078), which trust a global administrator's choice silently: an integration is often set up from a PSA's documentation by someone copying a URL, and the central server sits next to VictoriaMetrics, which deletes data on a `POST`. A save by anyone else that changes the URL clears the flag. The check uses ADR-0078's resolver, on the addresses actually connected to, and refuses addresses written in the URL.
- **Pause and resume.** An endpoint is `on`, `paused` by an administrator, or `disabled` after failing (below). Only `on` endpoints get new events.

### Delivery

**At least once.** A trigger on `integration_events` writes one `integration_deliveries` row per `on` endpoint that wants the event and covers its place, in the transaction that wrote the event: an event and its deliveries exist together or not at all.

**Several instances (ADR-0089).** Every instance runs the deliverer. A delivery is claimed with `FOR UPDATE SKIP LOCKED` by moving its `next_at` 60 seconds ahead and counting the attempt, in its own transaction; the request is then sent and the outcome written. Two instances never claim one delivery at the same time. An instance that dies after claiming leaves the delivery to be claimed again when the 60 seconds pass, so a receiver may see an event twice, never zero times; the 15-second timeout of ADR-0060 keeps a live request well within the lease. No leader is needed, so a crash of the leader does not pause integrations.

**Order.** A delivery waits while an earlier one to the same endpoint about the same subject is still pending: an incident's `opened` is never sent after its `resolved`, a server's `removed` after its `added` again, or an update's `succeeded` before its `started`. The subject is the incident, server, command, campaign or approval. Different subjects do not wait for each other, so one failing event cannot block an endpoint for a day, and a receiver that wants strict global order sorts on `occurred_at`.

**Retries.** 2xx is success; anything else, no answer within 15 seconds, or a redirect (never followed) is a failure. Retries wait 30 s, then double up to an hour, and stop once the delivery is 24 hours old: about 30 attempts. Then the delivery is `failed` and stays in the log.

**Automatic disabling.** An endpoint that has not had a single success for 24 hours of failures is `disabled`: it gets no new events, its pending ones wait, and the administrators who see notifications for its coverage get `integration.disabled` through their channels (a new notification event). Turning it back on resumes what is pending; "Replay" sends what it missed.

**Request.** `POST`, `Content-Type: application/json`, and the headers of ADR-0060, so one verification routine serves both:

- `X-MonitorFlow-Event`: the event, or `test`.
- `X-MonitorFlow-Delivery`: the event's `id`, the de-duplication key.
- `X-MonitorFlow-Timestamp`: Unix seconds when this attempt was signed.
- `X-MonitorFlow-Signature`: `sha256=` and the hex HMAC-SHA256 of `{timestamp}.{raw body}` under the secret.
- `X-MonitorFlow-Replay: 1` on replays.

Receivers compare the signature in constant time and reject timestamps more than 5 minutes from their clock, which bounds what a captured request can replay; the event `id` handles the rest. `docs/integrations.md` has the snippet.

**Delivery log.** Each delivery keeps its attempts, last HTTP status, last error in plain words (without the URL, as ADR-0077 does), and when it ended. The page lists an endpoint's recent deliveries with the event, status and code.

**Replay.** An administrator replays one event, or every event of a time range that the endpoint wants and covers, to one endpoint. A replay is a new delivery of the same event, same `id`, flagged as a replay, with its own day of retries. Events are kept 30 days, so that is how far back a replay reaches.

**Send test.** Sends a `test` event to the endpoint now, signed like the others, and answers in plain words with the status code. It is not stored, so it neither enters the log nor touches the failure count.

### Rejected

- **Strict per-endpoint order.** A single failing event would hold every later one for a day.
- **A global sequence number receivers must follow.** Simple for us, hard for them; `occurred_at` and per-subject order cover what a PSA needs.
- **Sending through the notification dispatcher, on the leader.** It would tie integrations to the leader's 5-second pass and its grouping waits, and a leader crash would stall them.
- **Secrets the administrator types.** ADR-0078 shows how much care a typed secret needs; a generated one is shown once and needs none.
- **Per-endpoint retry settings.** One schedule, documented, is enough.

## Consequences

- A PSA or a script gets incidents, acknowledgements, servers, updates, campaigns and approvals within seconds, signed, at least once, and can ask again for a month.
- Every event writes a row in `integration_events`, kept 30 days, whether or not an endpoint exists; the volume is that of the notification outbox plus successes and acknowledgements.
- New tables that open incidents, start commands or remove servers through the existing tables are covered without changes. Updates of Windows servers go through `commands` and are covered.
- Receivers must de-duplicate on `id`; the guide says so first.
- The `integration.disabled` notice goes through existing channels; an install without channels sees the state on the page only.
- Relay inventory events, per-attempt history, and an overlap period for rotated secrets are left for later.
