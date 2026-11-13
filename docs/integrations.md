# Integrations: receiving MonitorFlow events

MonitorFlow can send what happens to another tool, like a PSA or an automation: incidents, acknowledgements, servers, updates, campaigns and approvals. Set it up under **Administration › Integrations** (ADR-0102). For messages to people, use notification channels instead.

## What you receive

One `POST` per event, with a JSON body described by [`events/v1.schema.json`](events/v1.schema.json):

```json
{
  "version": 1,
  "id": "0b5f7c4e-2b8a-4a35-9d38-6a1f2f0c8e11",
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

| Event | Sent when |
| --- | --- |
| `incident.opened`, `incident.escalated`, `incident.acknowledged`, `incident.resolved` | an alert opens, becomes critical, is acknowledged, or ends |
| `server.added`, `server.removed` | a server is enrolled (or comes back, `"restored": true`), or removed |
| `server.offline`, `server.online` | a server stops reporting, or reports again |
| `update.started`, `update.succeeded`, `update.failed` | updates of one server or relay, outside campaigns |
| `campaign.started`, `campaign.succeeded`, `campaign.failed` | an update campaign |
| `approval.waiting`, `approval.decided` | a command the AI proposed waits for approval, or was decided |

`failed` means "did not succeed": the object's `state` says how (`failed`, `expired`, `cancelled`, `stopped`). New fields may appear at any time; ignore those you do not know.

## Rules your receiver must follow

1. **De-duplicate on `id`.** Delivery is at least once: after a timeout, a restart, or a replay, the same event comes again with the same `id`.
2. **Answer 2xx within 15 seconds.** Anything else is retried: after 30 seconds, then twice as long each time up to an hour, for 24 hours. Redirects are not followed. Do the work after answering.
3. **Order:** events about one subject (an incident, a server, an update, a campaign, an approval) arrive in order. Events about different subjects may not; sort on `occurred_at` if you need one order.
4. An integration that has not accepted anything for 24 hours is turned off, and administrators are told. Turn it back on from the page, then use **Send again** in its deliveries for what it missed. Events are kept 30 days.

## Verifying the signature

Every request carries:

| Header | Value |
| --- | --- |
| `X-MonitorFlow-Event` | the event, or `test` |
| `X-MonitorFlow-Delivery` | the event's `id` |
| `X-MonitorFlow-Timestamp` | Unix seconds when this attempt was signed |
| `X-MonitorFlow-Signature` | `sha256=` and the hex HMAC-SHA256 of `{timestamp}.{raw body}` under the integration's signing secret |
| `X-MonitorFlow-Replay` | `1` when an administrator sent the event again |

Compute the HMAC over the raw bytes of the body, before any JSON parsing, compare in constant time, and refuse timestamps more than 5 minutes away from your clock.

Python:

```python
import hashlib, hmac, time

def verify(secret: str, headers, body: bytes) -> bool:
    timestamp = headers["X-MonitorFlow-Timestamp"]
    if abs(time.time() - int(timestamp)) > 300:
        return False
    expected = "sha256=" + hmac.new(
        secret.encode(), timestamp.encode() + b"." + body, hashlib.sha256
    ).hexdigest()
    return hmac.compare_digest(expected, headers["X-MonitorFlow-Signature"])
```

Node.js:

```js
const crypto = require("node:crypto");

function verify(secret, headers, rawBody) {
  const timestamp = headers["x-monitorflow-timestamp"];
  if (Math.abs(Date.now() / 1000 - Number(timestamp)) > 300) return false;
  const expected = "sha256=" + crypto
    .createHmac("sha256", secret)
    .update(`${timestamp}.`)
    .update(rawBody)
    .digest("hex");
  const given = Buffer.from(headers["x-monitorflow-signature"] ?? "");
  return given.length === expected.length && crypto.timingSafeEqual(given, Buffer.from(expected));
}
```

The signing secret is shown once, when the integration is added, given a **New secret**, or saved with a new URL. A new URL always gets a new secret, so the old one never signs for another receiver. Keep it in your receiver's secret store.

## Addresses

The URL must be reachable from the central server. Private and local addresses (like `10.0.0.5`, `192.168.1.10` or `localhost`) are refused unless a global administrator ticks **Allow private addresses** for that integration.
