# ADR-0057: Offline incidents for servers and relays

Date: 2026-10-28
Status: accepted

## Context

ADR-0017 left "host down" as a follow-up: a server whose agent stops reporting keeps its threshold incidents open and opens none of its own. The simplicity audit (B4) stopped an agent and found "Online 0/1" next to "Critical incidents 0" and an empty Incidents page. A novice expects a monitoring product to say, unprompted, that a machine is gone. Relays are worse off: one silent relay hides every server behind it.

The registry (ADR-0013) knows which peers have a stream; `servers.last_seen_at` and `relays.last_seen_at` record the last message to within a minute. Agents and relays send a heartbeat every 30 s, and the server closes a stream after 90 s of silence.

## Decision

**Rule.** A built-in rule, with no configuration: a server or relay without a stream, whose last recorded message is more than 5 minutes old (ten missed heartbeats), opens a critical incident. That is longer than an agent restart, a relay restart or a server update, so routine work raises nothing. The incident is dated from the last message, so its duration is the real outage. The time since the server's own start counts as heard from: after the server itself was down, peers get 5 minutes to reconnect before anything opens. The incident resolves when the peer has a stream again, with the stream's start as its time.

**One incident per cause.** A server assigned to a relay that is offline opens nothing; the relay's incident covers it. An incident already open on such a server stays open. A muted or revoked relay no longer covers its servers.

**Model.** Offline incidents live in `incidents`, so the fleet tiles, the incidents list and every future notification see them without another code path. They use the reserved metric `offline` with instance `''`: sensor metrics all start with `monitorflow_`, so it cannot meet a threshold. A relay's incident has `relay_id` set and `server_id` NULL; `server_id` loses its NOT NULL, a CHECK keeps exactly one of the two set, and another partial unique index allows one open incident per relay. Values (`last_value`, `peak_value`) are 0 and meaningless; `threshold` holds `{"metric": "offline", "after_s": 300}`. Events are `opened` (reason `no_heartbeat`) and `resolved` (reason `back_online`, `muted` or `revoked`).

**Owner.** One task checks every 30 s, reading the servers, relays and open offline incidents from PostgreSQL and the live streams from the registry. It keeps no state, so a restart neither loses nor duplicates an incident, and opening uses `ON CONFLICT DO NOTHING`. It owns the offline rows of `incidents`; the threshold engine skips them when it loads its state, and the AI remediation loop never takes them, as no command reaches an offline host.

**Muting.** `offline_alerts`, on by default, on sites, servers and relays. An incident opens only when both the peer's and its site's are on; turning one off resolves an open incident at the next check. Operators of the site change them from the server and relay pages and from Administration › Sites (`PUT /api/{sites,servers,relays}/{id}/offline-alerts`, audited). There is no setting for the delay: one good default is simpler than a knob.

**API.** `IncidentView` gains `relay_id` and `customer_id`; `server_id` becomes nullable, the one type change, needed for relays' incidents. `hostname` carries the relay's name for those.

## Consequences

- A server that is down shows as a critical incident within about 5.5 minutes, and is counted in "Critical incidents" on the fleet.
- A relay outage is one incident, not one per server behind it. A server whose agent stopped while its relay is also down is reported once the relay is back.
- A host that is off on purpose for long needs to be muted, or revoked.
- `server_id` can be null in `GET /api/incidents`; clients that assumed a server must check `relay_id`.
- Notifications (Phase 8) will send these incidents like any other.
