# ADR-0017: Threshold engine and incidents

Date: 2026-10-19
Status: accepted

## Context

Architecture §5 puts the threshold engine on the central server, so rules change without touching the fleet, and asks that its state survive a restart without losing or duplicating incidents. Samples arrive on the agent streams (ADR-0013), already validated and labelled with the agent's identity; the stream loop must never wait for the engine, as it never waits for storage. Operators configure sensors per site with per-server overrides (ADR-0016), and thresholds should work the same way. Agents measure with their own clock and may send backlogs after a reconnect, up to 7 days old. Notifications are not in the first version (§12).

## Decision

**Thresholds.** Table `thresholds`, owned by one site or one server like `sensor_configs`: a full metric name (`monitorflow_disk_used_ratio`), an optional instance (NULL matches every instance), a comparison (`>`, `>=`, `<`, `<=`), a warning and/or a critical value, a duration in seconds, a hysteresis and `enabled`. A threshold names a metric, not a `sensor_configs` row: one sensor emits several metrics (ADR-0015), and a threshold on a metric keeps working when its sensor is reconfigured or renamed back. CHECK constraints refuse what the engine could not use: no value, non-finite values, a critical value on the safe side of the warning value, negative hysteresis, a duration over a day.

**Precedence.** ADR-0016's three layers keyed by `(metric, instance)`: built-in defaults, then the site's rows, then the server's rows; a later layer replaces an earlier one and a disabled row mutes what it replaces. For a sample, the threshold of its exact instance wins over the one for every instance, even when the exact one is muted, so `/boot` can be silenced without silencing `/`.

**Defaults** live in code, like ADR-0016's: disk used ratio above 0.85 (warning) and 0.95 (critical) for 5 minutes on every mount; memory available ratio below 0.10 and 0.05 for 5 minutes; CPU usage ratio of instance `total` above 0.9 for 10 minutes, warning only. All have a hysteresis of 0.02, so a value sitting on a threshold does not open and resolve in turn.

**State machine.** Per series (server, metric, instance), each severity has a pending timer: the timestamp of the first sample of an unbroken run where its condition holds. A severity fires once its timer is `duration` old, and keeps firing until the value is past its threshold by the hysteresis in the safe direction (`value <= threshold - hysteresis` for `>`). The series' incident takes the highest firing severity: none to some opens it, warning to critical escalates (after critical's own duration), critical to warning de-escalates (as soon as critical recovers), some to none resolves. Time is the samples' timestamps, not the server clock: backlogs are judged on when values were measured, and the machine is a pure function tested without clocks. A sample not newer than the last one of its series is ignored, so resent and reordered samples cannot move timers back.

**No data.** A silent series is not a recovered one: its open incident stays open, with `last_value_at` showing how old the last value is. Pending timers need an unbroken run, so a gap of more than `max_gap` (10 minutes by default, above the default 60 s interval) between two samples restarts them at the next sample. Marking incidents stale and resolving on agent offline were rejected: they would close real problems on a network blip. A silent agent will be covered by a "host down" rule on the registry, a follow-up, not by thresholds.

**Incidents.** Table `incidents`: server, metric, instance, severity, state (`open`, `resolved`), opened and resolved times (sample times), last value and its time, peak value (the worst seen while open) and a JSON snapshot of the threshold when it opened. A partial unique index allows one open incident per series; opening uses `ON CONFLICT DO NOTHING` and adopts the open row if one exists, so a restart or a race never duplicates one. Every change also appends a row to `incident_events` (`opened`, `escalated`, `deescalated`, `resolved`, with the severity after the change and the value), in the same transaction. Events are rejected on UPDATE and TRUNCATE by trigger and only go away with their server row; they are not in `audit_log`, which records actions of actors, while these are system observations at sample rate.

**Alert state.** Table `alert_states` holds the series that are not OK: severity of the open incident, the two pending timers and the last sample time. A series back to OK has no row. The engine writes it on every change of a timer or severity, not on every sample, so the cost follows transitions, not traffic.

**Restart.** At start the engine loads `alert_states` and the open incidents. Timers resume from their stored values, which is conservative because of the gap rule: the stored last sample time is the one of the last change, so a series whose first sample after the restart comes more than `max_gap` after it restarts its timers. A timer can therefore resume across a short restart, and never fires on a run the engine did not see for longer than `max_gap`. An open incident is resumed as it is, and the next samples resolve or escalate it.

**Pipeline.** The stream loop hands the valid series of each batch to the engine through a bounded channel with `try_send`; when the channel is full the batch is dropped and counted, as ADR-0013 does for storage. One engine task evaluates batches in order and writes changes to PostgreSQL. A database error is logged and the change kept in memory; the next change of that series rewrites its full state. Thresholds are cached per server with their revision, `sites.threshold_revision + servers.threshold_revision`, bumped by trigger as in ADR-0016, and the engine polls the revisions of cached servers every 10 s and reloads those that moved. When a threshold disappears or is muted, the open incidents it governed are resolved at the reload.

**Notifications.** Each transition is a `tracing` event and is passed to a `Notifier` trait, with no implementation besides logging yet.

## Consequences

- A threshold on a metric no sensor emits does nothing, silently; the API should warn about it.
- Durations count sample time. An agent whose clock is wrong opens incidents at the wrong wall time, but with the right duration.
- A dropped batch only delays a transition until the next sample of the series, unless it breaks a pending run longer than `max_gap`.
- An incident on a server whose agent stopped stays open until the host-down rule exists or the threshold is removed.
- Sensors with an interval above `max_gap` can only fire with a duration of 0, until `max_gap` follows the sensor interval.
- The API task can read thresholds, incidents, events and alert states as they are and writes only `thresholds`; the engine owns the other three tables.
