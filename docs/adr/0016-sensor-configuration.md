# ADR-0016: Sensor configuration pushed from the server

Date: 2026-10-19
Status: accepted

## Context

Agents run sensors (ADR-0012) but had no way to know which ones. Operators manage a fleet of about 1,000 servers grouped in sites, so most sensors are the same for a whole site with a few per-server exceptions. The configuration lives in PostgreSQL with the rest of the inventory (ADR-0002), and the agent must keep monitoring when its upstream is unreachable, including across its own restarts. Thresholds will be configured per sensor later and must fit next to it.

## Decision

**Storage.** One table, `sensor_configs`. A row belongs to exactly one site (a default for its servers) or one server (an override or an addition): `kind` (the implementation), `name` (the metric name, ADR-0013), `instance`, `interval_ms`, `timeout_ms` (NULL for the agent's default), `params` and `enabled`. `(name, instance)` is unique per owner. CHECK constraints repeat ADR-0012's rules, so the database never stores what the agent would refuse. Thresholds will get their own table referencing a row's `id`; nothing here anticipates their columns.

**Precedence.** The effective configuration of a server is three layers keyed by `(name, instance)`: the built-in defaults, then its site's rows, then its own rows. A later layer replaces the sensor of an earlier one; a disabled row removes it, which is how a site or a server opts out of a default or of a site sensor.

**Defaults.** The built-in layer is `cpu`, `memory` and `disk` every 60 s, in server code, not seeded rows. A new site is monitored without any setup, a release can improve the defaults without a data migration, and turning one off is the same operation as overriding anything else. A CLI to insert the three rows was rejected: a forgotten step leaves sites silently unmonitored, and the rows would freeze the defaults of the day.

**Revisions.** `sites.sensor_revision` and `servers.sensor_revision` are counters bumped by a trigger on every insert, update and delete of a row of that owner. A server's revision is their sum. Each counter only grows, so the sum changes on every change, even when two transactions on the two layers commit in another order than they took their values, which a single global sequence compared with `GREATEST` would miss. Changes of the built-in defaults are not counted: they come with a server upgrade, which reconnects every agent, and every Hello sends the configuration anyway.

**Protocol.** Two additive envelope variants. `SensorConfig { revision, sensors }` goes down after every accepted agent Hello and whenever the revision changes; it replaces the whole configuration, so a lost or repeated message cannot leave a partial state. `SensorConfigAck { revision, errors }` goes up once the agent applied it, listing the specs it refused, which keep the others running. The server writes the acknowledged revision to `servers.sensor_revision_applied`, so drift (applied below current) can be shown later. Params are a `map<string, string>`, the type `sensors::SensorConfig.params` already has: each sensor parses its own values, and the agent needs no JSON parser for this. The database stores them as a flat JSON object of scalars, which is easier to write by hand and from the API; numbers and booleans are sent in their JSON text.

**Change detection.** The server polls: one task reads the revisions of all connected agents in one query every 10 s and resends to those whose revision differs from what their connection last got. LISTEN/NOTIFY was rejected for now: it needs a dedicated connection and a reconnect path, and notifications lost while that connection is down need a poll to recover anyway. At 1,000 agents the poll is one indexed query every 10 s, and 10 s of latency is fine for a configuration change.

**Failures.** At Hello, the configuration is loaded after the inventory row; a database error refuses the stream with `UNAVAILABLE`, as ADR-0008 does for the row. Later, a failed poll, load or acknowledgement write is logged and retried at the next poll. A configuration that does not fit in the stream's outbound queue within a second is retried at the next poll too.

**Agent.** The agent applies a configuration by building a new scheduler and replacing the running one (ADR-0012), reports refused specs in the acknowledgement and never exits over one, and stores the last configuration it received in its data directory, to run it at start-up before any upstream answers.

## Consequences

- Until the sensors API exists, operators edit `sensor_configs` with SQL; the schema rejects invalid rows.
- Every configuration change costs a full resend to the servers concerned; at a few dozen sensors per server this is small.
- A relay gets no configuration for itself. Agents behind a relay get theirs through it once relays route envelopes per agent.
- `sensor_configs` rows are deleted with their site or server. Moving a server between sites, when it exists, must bump its revision.
- Removing a built-in default from code drops that sensor on every server at its next connection, unless a site or server row keeps it.
