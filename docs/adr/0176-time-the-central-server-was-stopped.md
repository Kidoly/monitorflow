# ADR-0176: Time the central server was stopped is not counted

Date: 2026-11-22
Status: accepted; amends what counts in ADR-0155 and ADR-0172

## Context

Availability (ADR-0155) counts a thing down while one of its incidents is critical, from the incident's events. An incident only changes when the central server evaluates new samples. When the central server itself is stopped (its host shut down, an update that goes wrong, a power cut), nothing is evaluated: an incident that was critical stays critical until the server comes back and resolves it with the first sample. The whole stop then counts as down time of that server, although nobody watched anything then.

It happened on a real installation: the host of the central server was off for about 16 hours while a server's memory check was critical, and the report showed one outage of almost 4 days where about 3 days and 6 hours were seen. Offline incidents do not help: the central server gives peers 5 minutes after its own start before opening any (ADR-0072), which is right, since it cannot know what happened while it was off.

Nothing kept the time the central server was not running: `server_instances` (ADR-0089) loses an instance's row when it stops cleanly.

## Decision

**Record it.** A one-row table `central_server_heartbeat` holds the last heartbeat of any instance, moved forward on each cluster tick (every 5 seconds). When an instance finds it older than the delay after which an instance is taken for dead (30 seconds), it writes the time in between to `central_server_gaps`, in the same statement that moves the heartbeat, with the row locked. With several instances, any one still running keeps the heartbeat fresh, so only a stop of all of them is a gap. A database that cannot be reached for a while is a gap too: nothing could be written then either.

**Leave it out.** Over a report or an uptime view, the gaps are neither up nor down: they leave the time counted like planned maintenance does, and an incident's critical time within them is not down time. Planned maintenance that overlaps a gap stays maintenance, counted once. An outage that runs into a gap ends where the gap starts. A day or a window with nothing but gaps is "Not measured". `AvailabilityNumbers` gains `unobserved_seconds`; the rule printed on the page and in the PDF says so.

Gaps before this change were never recorded and still count as they did. An administrator who knows one can add it by hand to `central_server_gaps`.

## Consequences

- A stop of the central server no longer turns into an outage of every server that had a critical incident open at that moment.
- A stop of the central server also hides a real outage that happened while it was off. That is the honest answer: it was not seen, and the offline incidents opened after the restart still count from then on.
- One write per instance every 5 seconds on a one-row table, and one row per stop.

## Alternatives considered

- **Resolving incidents when the central server stops**: it would lose the incident's history and reopen a new one at the restart, which the incident pages and notifications would show as two problems.
- **Reading the gaps from the metrics store** (no samples from anyone): metrics have a retention shorter than the 13 months a report may cover, and ADR-0155 already keeps availability out of the metrics.
