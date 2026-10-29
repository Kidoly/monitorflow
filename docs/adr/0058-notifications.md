# ADR-0058: Email and webhook notifications

Date: 2026-10-29
Status: accepted

## Context

`docs/product.md` listed alert notification channels (email, webhook, chat) as a non-goal of the first version, and the threshold engine kept a `Notifier` trait whose only implementation logs. An incident therefore only shows while someone has the web interface open (simplicity audit, B4). For a monitoring product, being told is the expectation a novice takes for granted, and the audit's walk shows it is the first thing missing once the fleet is installed.

Incidents are not the only thing worth a message: a command the AI proposed waits for approval until its deadline, and a failed update or a halted rollout stops progress without anyone looking. Incidents are also opened from more than one place: the threshold engine today, offline peers next (#176), maybe more later.

## Decision

This revises the product non-goal: **email (SMTP) and webhook notifications are part of the first version.** Chat services are reached through webhooks; there is no dedicated chat integration.

**Channels** live in PostgreSQL (`notification_channels`) and are set up by global administrators under Administration › Notifications. Each is an email channel (recipients, mail server and port, optional username, password and sender) or a webhook (a URL, an optional signing secret), and is either for every customer (the default) or for one. A site scope was left out: customers are the tenancy boundary (ADR-0026) and a site-level choice would be one more option to explain.

- **Secrets stay on the server.** The SMTP password or webhook secret is written by the API and never returned: views carry `has_secret`, an edit with an empty secret keeps it, and the audit records only whether one was set. The audit keeps a webhook's host, not its URL, since chat webhook URLs embed a token. Secrets are in the database, like the other credentials the server holds for the interface, so database backups include them.
- **Few settings.** There are no per-event or per-severity filters, no enable switch and no TLS mode: port 465 is TLS from the first byte; any other port uses STARTTLS, required when a password is set and opportunistic otherwise, so a local relay on port 25 works. The sender defaults to the username when it is an address.
- **Send test.** Saving a channel sends a test at once, and every channel has a "Send test" button. The answer is the outcome in plain words ("smtp.example.com:587 refused the username or password (535 …)", "Could not connect to hooks.example: connection refused"), which also becomes the channel's status until the next delivery.

**Events** come from an outbox, `notification_outbox`, which database triggers fill:

| Event | When |
| --- | --- |
| `incident.opened`, `incident.escalated`, `incident.resolved` | a row in `incident_events` of that kind; deescalations are not sent |
| `approval.waiting` | a row in `remediation_pending_commands` |
| `update.failed` | an `apply_updates` or `self_update` command outside campaigns, or a relay's `self_update`, ends `failed` or `expired`; a campaign ends `failed`; an automatic rollout halts |

Triggers rather than calls from each code path: anything that opens an incident through `incidents` and `incident_events` is covered without knowing about notifications, offline incidents included. The incident trigger reads a `relay_id` column when the row has one. A campaign reports once, when it fails, not once per server.

A **dispatcher** task on the server polls the outbox every 5 seconds, claims pending rows by setting `sent_at` (`FOR UPDATE SKIP LOCKED`), renders them, and sends each to every channel whose customer matches, in order, up to three tries per delivery (2 and 10 seconds apart); once a delivery failed every try, the rest of that pass get one try each. Delivery is at most once: a claimed row is not sent again after a restart. Rows older than a day when claimed are dropped instead of sent, and delivered rows are deleted after a week. The outbox is not the audit log; deleting from it is allowed.

**Webhook requests** are a `POST` with a JSON body, `Content-Type: application/json`, a 15-second timeout and no redirects followed. Headers:

- `X-MonitorFlow-Event`: the event, like `incident.opened`, or `test`.
- `X-MonitorFlow-Delivery`: the event's ID, the same for every channel; `0` for tests.
- `X-MonitorFlow-Timestamp`: Unix seconds when sent.
- `X-MonitorFlow-Signature`: with a signing secret only, `sha256=` and the hex HMAC-SHA256 of `<timestamp>.<body>` under the secret. Receivers recompute it over the raw body, compare in constant time, and reject old timestamps.

The body, version 1 (fields are only ever added):

```json
{
  "version": 1,
  "id": "1042",
  "event": "incident.opened",
  "occurred_at": "2026-10-29T10:15:00Z",
  "text": "Critical: cpu_usage_percent is 97.5 on web-1 (paris)",
  "url": "https://mf.example/incidents/view/?id=7c0e…",
  "customer": { "id": "…", "name": "default" },
  "site": { "id": "…", "name": "paris" },
  "server": { "id": "…", "name": "web-1" },
  "relay": null,
  "incident": {
    "id": "7c0e…", "metric": "monitorflow_cpu_usage_percent", "instance": "",
    "severity": "critical", "value": 97.5, "peak_value": 97.5,
    "opened_at": "2026-10-29T10:15:00Z", "resolved_at": null
  }
}
```

`text` is the event in one line, which Slack and Mattermost incoming webhooks show as is. `url` opens the page that shows the event, under the web interface's URL (`web_url`, ADR-0057). Each event adds one object:

- `incident`: as above; `severity` is the new one, or the last one when resolved.
- `approval`: `id`, `remediation_incident_id`, `incident_id`, `metric`, `command`, `tier` (`low_impact` or `high_impact`). The command is the AI's proposal: untrusted text.
- `update`: `kind` (`apply_updates`, `self_update`, `campaign` or `automatic_rollout`), `state`, `error`, and per kind `command_id`, `version`, `campaign_id` and `name`, or `peers` (`agent` or `relay`).

`customer`, `site`, `server` and `relay` are `null` when they do not apply. Emails carry the same `text` as subject (after `[MonitorFlow]`), then the link and the place, as plain text.

Rejected:

- **Notifying from each code path.** The engine's `Notifier` runs on the engine task and would need a twin in every place that opens incidents, approvals or updates; offline incidents would need their own call.
- **Delivering at least once with retries across restarts.** It needs per-channel delivery state and deduplication on the receiver; a lost message during a restart is rarer than a stuck queue that floods later.
- **Customer administrators managing their own channels.** Useful for MSPs, but it adds a permission model to this page; a global administrator can scope a channel to one customer meanwhile.
- **Dedicated Slack, Teams or chat integrations.** Webhooks with a `text` field cover Slack and Mattermost; others can follow if asked for.

## Consequences

- An installed server tells its administrators about incidents, waiting approvals and failed updates within seconds, after one form and a test.
- Paths that later open incidents in `incidents` notify without changes; paths that fail updates elsewhere need a trigger here.
- Every incident event writes an outbox row, kept a week, whether or not a channel exists.
- The update-available notice (Phase 8, task 11) can add an event to the outbox's list.
- Webhook URLs are reachable from the server, so an administrator could point one at an internal address. Only global administrators set them, and they already control the server.
