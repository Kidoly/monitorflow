# ADR-0033: Update campaigns

Date: 2026-10-24
Status: accepted

## Context

ADR-0031 gave the server one-off `apply_updates` commands, one in flight per server, with results in `commands` and a `campaign_id` column waiting for campaigns. Architecture §7 asks for campaigns: an administrator picks a group of servers, the update runs in waves (a pilot first), only inside maintenance windows, and stops by itself when a wave fails. A web thread builds the campaign pages on the REST API next, so its JSON must be settled here.

Constraints:

- A campaign belongs to one customer and may only touch that customer's servers (ADR-0026); queuing updates is a customer administrator's decision, as for `POST /api/servers/{id}/commands` (ADR-0029, ADR-0031).
- The server may restart at any time; a campaign must neither lose its place nor queue a server twice.
- Commands already sent must be allowed to finish: stopping cannot interrupt a package manager.

## Decision

### Model

Migration `20261024090000_campaigns`, plus `20261024091000_commands_campaign_fk` for the foreign key from `commands.campaign_id`.

- `campaigns`: `customer_id`, `name`, the `site_id` named at creation if any, the `ApplyUpdates` spec (`packages`, `security_only`, `allow_reboot`), `command_ttl_s` (60 s to 7 days, default one hour), the failure threshold (`failure_threshold`, `failure_threshold_unit` `count` or `percent`), `state`, `current_wave`, `error`, `created_by`, `created_at`, `started_at`, `finished_at`. Campaigns are never deleted.
- `campaign_waves`: `(campaign_id, position)` from 0, `started_at`, `finished_at`.
- `campaign_targets`: one row per server with its `wave`, its current `command_id` and an `error` when it got no command (revoked since creation).
- `campaign_windows`: the maintenance windows, below.

**Targets are frozen at creation.** The request names servers and/or a site; the server resolves them to the customer's unrevoked servers, sorted by hostname, and stores which wave each is in. A server enrolled in the site later is not added. Progress per wave stays meaningful, the API can show exactly who is next, and the runner never re-plans after a restart. Waves are requested as an ordered list of `{"servers": [ids]}`, `{"size": n}` or `{"percent": p}` (of all targets, rounded up): explicit servers are placed first, size and percentage waves then take the next servers in hostname order, and any servers left over form one last wave; no waves at all is one wave. A size is enough for the usual "pilot of 1, then 10 %, then the rest"; explicit lists let an administrator pick the pilot. A wave that would be empty is refused.

**Failure threshold, per wave.** A wave fails when more of its servers failed than `value` (count) or than `value` percent of the wave. The default, count 0, stops at the first failure. A failed server is a command `failed`, `expired` or `cancelled`, or a target skipped with an `error`. The threshold is per wave, not cumulative, so a large final wave is not judged by the pilot's size.

### Maintenance windows

A campaign has zero or more windows; with none it may queue at any time, with several their union counts.

- **Weekly**: ISO `weekdays` (1 Monday to 7 Sunday), `start` and `end` as local `HH:MM` (stored as minutes past midnight, `24:00` allowed as an end) and an IANA `timezone`. An end before the start closes the next day: Saturday 22:00 to 04:00 is open until Sunday 04:00.
- **Once**: an explicit `[starts_at, ends_at)`.

Weekdays plus a time range is what maintenance windows are in practice and what a web form can edit; the one-off range covers "this Sunday night only" and campaigns that must not start before a date. Cron expressions and RRULEs were rejected: hard to validate and display, and they describe starts without an end.

The SQL function `campaign_window_end(campaign, at)` returns when the windows open at `at` close (the latest end), NULL when none is open and `infinity` without windows. It converts with `AT TIME ZONE`, so local times follow PostgreSQL's time zone database across daylight saving changes, and the server needs no time zone crate. Time zone names are checked against `pg_timezone_names` at creation.

### States and actions

`draft` → (start) `scheduled` → (first wave queued) `running` → `completed`; `failed` when a wave exceeds its threshold. `pause` from `scheduled` or `running` gives `paused`; `resume` returns to `running`, or `scheduled` if no wave was queued yet. `stop` from any non-final state gives `stopped`. `stopped`, `completed` and `failed` are final and set `finished_at`; `error` says why.

Pausing and stopping cancel the campaign's commands still `queued` (not sent); `sent` and `running` ones finish and are counted. After a pause, the cancelled commands' targets are detached and get a new command when the campaign runs again. Every action and change is audited: `campaign.create`, `campaign.start`, `.pause`, `.resume`, `.stop` (actor the user), and from the runner (actor `system` `campaign-runner`) `campaign.queue` (wave and command count), `campaign.wave_finish`, `campaign.complete`, `campaign.fail`. Command results stay audited as `command.finish` (ADR-0031).

### Runner

`server::campaign::Runner`, one task, every 10 s and when the API starts or resumes a campaign. For each `scheduled` or `running` campaign, oldest first, one transaction holding the campaign's row lock (`FOR UPDATE SKIP LOCKED`: a campaign locked by a user action or another server process is looked at next tick):

1. If a wave is in progress: when its failures exceed the threshold, the campaign fails, its queued commands are cancelled and the wave is closed. When every target of the wave has a final command or an error, the wave is finished and the next one becomes current; past the last wave the campaign is completed.
2. Otherwise, or for the new current wave, commands are queued only when a window is open for at least 60 more seconds (the shortest command TTL). Every target of the wave without a command and without an error gets one through `CommandStore::enqueue_in` in the same transaction, with `campaign_id` set and a TTL of `command_ttl_s` capped at the window's end, so an agent that connects late never starts outside the window. A target revoked since creation gets the error `server revoked` instead.
3. After commit, the dispatcher is woken for the queued agents.

The runner reads results by polling `commands`; nothing in the gRPC path changes. The "one in flight per server" rule stays the dispatcher's (ADR-0031): a campaign command queues behind any other command of its server. A command still queued when its window closes expires and counts as a failure: an agent offline for a whole window fails its wave, which an administrator sets the threshold for.

Restarts lose nothing: state lives in the tables, a target gets a command in the same transaction that records it, and a wave is finished in the transaction that checks it, so running a step twice queues nothing twice. The current time is a parameter of `Runner::tick`, so tests drive windows with any clock.

### REST API

Reading needs a role on the campaign's customer as a whole, global or by customer grant (a site grant does not show campaigns, which span sites); creating and acting need an administrator role on it, global or by customer grant, the rule of `POST /api/servers/{id}/commands`. A campaign of a customer the user does not see is 404, one it sees without that right 403. Targets outside the customer are 400 `unknown server <id>`, as for IDs that do not exist.

| Method | Path | Answer |
| --- | --- | --- |
| GET | `/api/campaigns` | `{"campaigns": [Campaign], "next_cursor"}`, newest first; `customer_id`, `state`, `limit` (1 to 500, default 100) and `cursor` |
| POST | `/api/campaigns` | body below; 201 CampaignDetail |
| GET | `/api/campaigns/{id}` | CampaignDetail |
| POST | `/api/campaigns/{id}/start`, `/pause`, `/resume`, `/stop` | 200 CampaignDetail; 409 `{"error": "cannot pause a campaign that is draft"}` |

Body of `POST /api/campaigns` (only `customer_id`, `name` and a target are required):

```json
{"customer_id": "uuid", "name": "October updates",
 "site_id": "uuid", "server_ids": ["uuid"],
 "waves": [{"servers": ["uuid"]}, {"size": 5}, {"percent": 25}],
 "packages": [], "security_only": false, "allow_reboot": false,
 "command_ttl_s": 3600,
 "failure_threshold": {"value": 0, "unit": "count"},
 "windows": [{"kind": "weekly", "weekdays": [6, 7], "start": "22:00", "end": "04:00",
              "timezone": "Europe/Paris"},
             {"kind": "once", "starts_at": "2026-11-01T20:00:00Z", "ends_at": "2026-11-02T04:00:00Z"}]}
```

Campaign:

```json
{"id": "uuid", "customer_id": "uuid", "customer_name": "acme", "name": "October updates",
 "site_id": "uuid", "state": "running", "error": null,
 "spec": {"packages": [], "security_only": false, "allow_reboot": false},
 "command_ttl_s": 3600, "failure_threshold": {"value": 0, "unit": "count"},
 "windows": [{"kind": "weekly", "weekdays": [6, 7], "start": "22:00", "end": "04:00",
              "timezone": "Europe/Paris"}],
 "wave_count": 3, "current_wave": 0,
 "progress": {"total": 12, "pending": 9, "in_progress": 2, "succeeded": 1, "failed": 0,
              "cancelled": 0},
 "created_by": "uuid", "created_at": "…", "started_at": "…", "finished_at": null}
```

CampaignDetail is a Campaign plus:

```json
{"waves": [{"index": 0, "state": "running", "started_at": "…", "finished_at": null,
            "progress": {"total": 1, "pending": 0, "in_progress": 1, "succeeded": 0,
                         "failed": 0, "cancelled": 0},
            "servers": [{"server_id": "uuid", "peer_id": "agent-1", "hostname": "web-1",
                         "site_id": "uuid", "status": "running", "command_id": "uuid",
                         "exit_code": null, "error": null, "finished_at": null}]}]}
```

`current_wave` is `null` until the first wave is queued. A wave's `state` is `pending`, `running` or `done`. A server's `status` is `pending` (no command yet), the state of its command (`queued`, `sent`, `running`, `succeeded`, `failed`, `expired`, `cancelled`), or `skipped` with an `error` when it got none. In `progress`, `in_progress` counts `queued`, `sent` and `running`; `failed` counts `failed`, `expired` and `skipped`. A command's output is at `GET /api/commands/{command_id}`. Fields may be added; none will be renamed or removed.

## Consequences

- Campaigns reuse signed commands, the agent journal and the dispatcher unchanged; anything ADR-0031 guarantees per command holds per campaign server.
- A server can be in several campaigns at once; their commands run one after the other.
- Window evaluation lives in a SQL function: changing its rules needs a new migration replacing the function.
- Pinning a repository snapshot (architecture §7) needs the agent's source configuration per campaign (ADR-0032); a later change can add a `configure_sources` step before each server's update. Done in ADR-0038: optional `sources` per campaign, configured before each server's update and restored after it.
- No CLI yet; campaigns are managed through the API.
