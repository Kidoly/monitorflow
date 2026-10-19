# ADR-0019: Threshold, incident and site endpoints

Date: 2026-10-19
Status: accepted

## Context

The threshold engine (ADR-0017) stores thresholds per site and per server and owns incidents, their events and alert states. The web interface needs to edit thresholds, show what applies to a server, list incidents for the fleet and a server page, and pick a site for site-level configuration. The API already has conventions for this: sensor configuration endpoints (ADR-0016) with role and site scoping, keyset pages on `/api/servers`, errors as `{"error": "..."}`. The web thread consumes these endpoints in parallel, so the shapes must be fixed here.

## Decision

**Thresholds** follow the sensor configuration endpoints exactly, so the interface can share code between the two screens:

| Method | Path | Role | Body / response |
| --- | --- | --- | --- |
| GET | `/api/servers/{id}/thresholds/effective` | read | `{"server_id", "site_id", "revision", "thresholds": [EffectiveThreshold]}` |
| GET | `/api/sites/{id}/thresholds`, `/api/servers/{id}/thresholds` | read | `{"entries": [Entry]}`, sorted by metric, then instance with `null` first |
| POST | same two paths | modify | `EntryRequest`; 201 `Entry` |
| PUT | `/api/thresholds/{id}` | modify | `EntryRequest`, replaces every field; 200 `Entry` |
| DELETE | `/api/thresholds/{id}` | modify | 204 |

- `EntryRequest`: `{"metric", "instance"?, "comparison", "warning"?, "critical"?, "duration_s"?, "hysteresis"?, "enabled"?}`. `instance` `null` or absent covers every instance, and `""` is the series without an instance. `duration_s` and `hysteresis` default to 0, `enabled` to true. Unknown fields are refused.
- `Entry`: the request's fields plus `id`, `site_id`, `server_id` (exactly one set), `created_at`, `updated_at`.
- `EffectiveThreshold`: `{"origin": "default" | "site" | "server", "threshold_id", "metric", "instance", "enabled", "comparison", "warning", "critical", "duration_s", "hysteresis"}`. It is built with the engine's `rules::resolve`, so the view cannot drift from what the engine applies. **Muted keys are listed** with `enabled: false` and the row that mutes them: a disabled built-in default stays visible and can be turned back on by deleting that row. `threshold_id` is `null` for built-in defaults.
- Validation repeats the schema's CHECKs with messages a user can act on: the metric pattern of ADR-0018 (`monitorflow_` and a sensor metric name), ADR-0012's instance rules, one of the four comparisons, a warning or critical value unless disabled, critical not on the safe side of warning, `duration_s` 0 to 86,400, `hysteresis` finite and 0 or more. Failures are 400 and duplicates (same owner, metric and instance) 409.
- Every change writes `threshold.create`, `.update` (with `before` and `after`) or `.delete` to `audit_log` in the same transaction. The schema's trigger bumps the revision, and the engine reloads within its 10 s poll. `revision` in the effective view is the value it compares.

**Incidents are read-only.** `GET /api/incidents` and `GET /api/incidents/{id}`. The engine is the only writer of `incidents`, `incident_events` and `alert_states` (ADR-0017). An API write would race the engine's in-memory state, which assumes its own rows; for example, an incident resolved by hand would be reopened, or worse, adopted with a stale peak. Acknowledging and silencing will be their own tables when notifications arrive, not edits to these.

- Item: `{"id", "server_id", "hostname", "site_id", "site_name", "metric", "instance", "severity", "state", "opened_at", "resolved_at", "last_value", "last_value_at", "peak_value", "threshold"}`. Times are sample times, and `threshold` is the snapshot taken at opening.
- Filters, all optional: `state` (`open`, `resolved`), `severity` (current severity), `server_id`, `site_id`, `from`, `to`. The time range selects incidents **active at some point of it** (`opened_at <= to` and not resolved before `from`), which is what a "what happened during this window" view needs. Filtering only on `opened_at` would hide a long incident that started before the window. Unknown parameters, bad values and `from > to` are 400.
- Order: open incidents first, then `opened_at` newest first, then `id`. Pages are keyset-paginated on that key like `/api/servers`: `limit` 1 to 500 (default 100, since rows are larger than servers), `next_cursor` opaque, `null` on the last page. An incident opened or resolved between two pages does not make the others repeat. It may itself be missed or shown twice when it moves between the open and resolved groups, which a refresh fixes.
- Detail: the item plus `"events": [{"id", "kind", "severity", "value", "occurred_at", "recorded_at", "details"}]` in insertion order (`id`), which is the order the engine made the changes. `occurred_at` can go backwards across a restart or a removed threshold (server clock), so it is not the sort key.
- Scope: a server or site filter outside the user's sites matches nothing, which gives an empty list rather than an error, so the filter does not reveal which IDs exist. An incident outside them is 404.

**Fleet overview.** `/api/servers` items gain `open_incidents`, the count of open incidents per server. It is one probe of the partial unique index on open incidents per row of a page of at most 1,000. The field is added and none are changed, which the `ServerView` contract (add fields, never rename or remove them) allows.

**Sites.** `GET /api/sites` returns `{"sites": [{"id", "name", "server_count"}]}` for the sites the user reads, sorted by name. It is not paginated: an organisation has tens of sites, not thousands. A user without any role gets 403, like `/api/servers`.

## Consequences

- The interface can show every layer of a threshold and re-enable a muted default without special cases.
- Listing every incident of a large fleet has no index on `opened_at` alone. The per-server index covers server pages, and a fleet-wide index can be added when volumes justify it.
- A threshold on a metric no sensor emits is accepted silently (ADR-0017). A warning in the response needs the server's metric discovery and is a follow-up.
- Incident acknowledgement, comments and silences need new tables and endpoints. This ADR keeps `incidents` engine-only.
