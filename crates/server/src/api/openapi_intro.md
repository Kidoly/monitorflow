The public API of MonitorFlow, for scripts and integrations: a ticketing tool reading alerts, a chat bot acknowledging them, a deployment script silencing a server while it works. It is the same API the web interface uses, under `/api/v1`, with the same rules about who may see and change what.

## API keys

An administrator makes a key under **Admin › API keys**: a global administrator for anything, a customer's administrator for that customer, a site's administrator for that site. The key is shown once, when it is made: `mfk_` followed by 43 characters. MonitorFlow keeps only a hash of it, so a lost key cannot be shown again; make another one.

What a key may do is chosen when it is made and never changes. To change it, make another key and turn the first one off.

- **Access**: "Read only" (viewer, the default), "Read and change" (operator), "Approve commands" (approver) or "Administer" (administrator).
- **Reach**: everything, one customer or one site. Outside its reach, servers, sites and incidents do not exist for the key: they answer 404, and lists leave them out.
- **Areas**, all by default: `servers` (servers and relays, their metrics and logs), `alerts` (incidents, silences, on-call, follow-up, sensors, thresholds and check settings, offline alerts, notification channels), `updates` (commands, campaigns, packages, package sources, mirrors, rollback, update settings), `remediation` (AI remediation and AI commands), `dashboards`, and `administration` (everything else). A route outside the key's areas answers 403 `api_key_scope`. Any key may list servers, relays, sites and customers within its reach, to turn names into IDs. Each operation below says which area it belongs to.
- **Expiry**: 30 days, 90 days, 1 year (the default) or never.

A key acts as the person who made it, narrowed to its reach and access, checked again on every request. It never does more than its maker could at that moment: a maker who loses a role loses it for the key too, a maker who is disabled finds the key refused, and a maker who is deleted takes the key with them. The keys list shows when each key was last used, to the minute. Anyone who could have made a key can turn it off; it stops at its next request.

What a key changes is written to the audit log as its maker's action, with the key's ID, so the log tells a person from their script. The secret never appears in the audit log or the server's logs.

## Authentication

Send the key in the `Authorization` header of every request:

```
curl -H "Authorization: Bearer mfk_…" https://monitorflow.example.com/api/v1/servers
```

Keys work only under `/api/v1`; the web interface's own paths refuse them. A request with a key needs no CSRF header. A missing, malformed, unknown or expired key, or one whose maker is disabled, answers 401 `unauthenticated`.

Signing in and out, first-run setup, password and acknowledge links, display links and screens, API keys themselves, a person's own on-call contact, and inviting users or issuing their password links are not part of the public API: they answer 404 under `/api/v1`. They are for people in a browser, or they hand out a credential that would outlive the key.

## Versions and changes

`/api/v1` is a contract. Within v1, MonitorFlow only adds: new fields in answers, new optional parameters, new routes, new values of `code`. Fields are never renamed or removed and their types never change. Your client must ignore fields it does not know. When a change would break v1, MonitorFlow keeps the old shape in v1 and makes a v2 only when that becomes unreasonable.

This document is served by every central server at `/api/v1/openapi.json`, without a key.

## Conventions

- Bodies are JSON objects with `snake_case` field names. Send `Content-Type: application/json`. Bodies are at most 64 KiB; a larger one answers 413. Most routes refuse a body with a field they do not know, with 400 `invalid`.
- IDs are UUIDs. Times are RFC 3339 in UTC, like `2026-11-14T09:30:00Z`. Durations are in seconds (`_s`) or milliseconds (`_ms`), as the field name says.
- An ID that is not a UUID answers 404, like an ID that does not exist.

## Pagination

Lists that can grow without bound take `limit` and `cursor` and answer `next_cursor`: servers, relays, incidents, a server's commands and packages, campaigns, tokens, remediation incidents and events. Pass `next_cursor` as `cursor` to get the next page, with the same filters; it is `null` on the last page. Cursors are opaque: do not build or change them. A server or incident added between two pages neither repeats nor shifts the others. There are no page numbers or totals.

Lists that stay small (sites, customers, silences, approvals, dashboards) answer in full.

## Filters

Filters are query parameters, exact values ANDed together, like `GET /api/v1/incidents?state=open&severity=critical&site_id=…`. A bad value is 400 `invalid`, and so is an unknown parameter on the lists that say so.

## Errors

Every error answers with a status and a JSON body:

```json
{"error": "limit must be between 1 and 500", "code": "invalid"}
```

`error` is a sentence in plain words, safe to show to a person; it may change between versions. `code` is stable: branch on it. Codes are only ever added.

| Code | Status | Meaning |
| --- | --- | --- |
| `invalid_request` | 400 | The body could not be read as JSON (most routes answer `invalid` instead, with the parser's message) |
| `invalid` | 400 | A parameter or field is wrong; `error` says which and why |
| `unauthenticated` | 401 | No key, or one that is unknown, expired or turned off, or whose maker is disabled |
| `invalid_credentials` | 401 | Wrong username or password (signing in, not in v1) |
| `forbidden` | 403 | The key's access, or its maker's roles, do not allow this on something it can see |
| `api_key_scope` | 403 | The route is outside the key's areas |
| `cross_site` | 403 | A browser request without the CSRF header (sessions only) |
| `not_found` | 404 | It does not exist, or is outside the key's reach |
| `conflict` | 409 | The current state does not allow it, like a silence already over; `error` says why |
| `idempotency_in_progress` | 409 | The first request with this `Idempotency-Key` is still running |
| `gone` | 410 | A one-time link that was used or expired (not in v1) |
| `idempotency_key_reused` | 422 | This `Idempotency-Key` was used for another request |
| `rate_limited` | 429 | Over the key's rate limit |
| `internal` | 500 | Something failed on the central server; the details are in its log |
| `metrics_unavailable` | 502 | The metrics database is unreachable, failing or not configured |
| `logs_unavailable` | 502 | The logs database is unreachable, failing or not configured |
| `metrics_timeout` | 504 | The metrics database did not answer in time |
| `logs_timeout` | 504 | The logs database did not answer in time |

## Acting on many at once

The checkboxes of the web interface's incident and fleet lists act on many items with the routes of one item, called once per item, a few at a time. Each item then succeeds or fails on its own, and the summary says how many went through and why the others did not ("4 acknowledged, 1 refused: insufficient rights"). A script does the same, and a 403 or 404 on one item leaves the others done. The web interface applies these one item at a time:

| Action | Route, once per item |
| --- | --- |
| Acknowledge incidents | `POST /api/v1/incidents/acknowledge` with one ID in `incident_ids` |
| Silence the servers of incidents | `POST /api/v1/silences` with `scope` `server` (`relay` for a relay's offline incident), once per server however many incidents it has |
| Acknowledge the alerts of servers | `POST /api/v1/incidents/acknowledge` with the server's open incidents nobody acknowledged, once per server (they share its site) |
| Silence servers | `POST /api/v1/silences` with `scope` `server`, once per server |

`POST /api/v1/incidents/acknowledge` also takes up to 500 incidents in one request, but all or none: one incident on a site the key may only read refuses the whole request with 403, before anything is written. Send one at a time when some may be out of reach.

## Idempotency keys

A `POST`, `PUT` or `DELETE` made with a key may carry `Idempotency-Key`: 1 to 255 visible ASCII characters, a UUID usually. The first request with a value runs and its answer is kept for 24 hours. The same request again (same method, path, query and body) gets that answer back with `Idempotent-Replayed: true`, instead of running a second time. A script that lost its connection can then retry without opening two silences or queuing two updates.

- The same value with another request answers 422 `idempotency_key_reused`.
- While the first request is still running, a retry answers 409 `idempotency_in_progress`; try again a moment later.
- Answers of 500 and above are not kept, so a retry runs again. Answers over 1 MiB are not kept either.
- Answers that hand out a secret once (tokens, install keys, signing secrets) are not stored: a replay says it was done, without the secret.
- Values belong to a key: two keys never see each other's answers.

Without the header, writes behave as usual. Most are safe to repeat anyway, like acknowledging an incident or setting a value; the operations that are not say so.

## Rate limits

Each key may make 600 requests a minute, counted in fixed one-minute windows on each central server. Every answer to a key carries:

- `RateLimit-Limit`: requests allowed in a window, 600.
- `RateLimit-Remaining`: requests left in the current window.
- `RateLimit-Reset`: seconds until the window ends.

Over the limit, requests answer 429 `rate_limited` with `Retry-After`, in seconds. Requests made in a browser session are not limited.
