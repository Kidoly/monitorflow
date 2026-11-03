# ADR-0089: Several server instances

Date: 2026-11-03
Status: accepted

## Context

One central server runs everything. If its process or host dies, agents and relays queue their data (ADR-0027) and nobody is told: the threshold engine, the offline check and the notification dispatcher die with it. Phase 12 asks for high availability before real customers.

Much of the server was already written for more than one process. Campaigns advance under a row lock (ADR-0033), mirror jobs under advisory locks (ADR-0030, ADR-0074), the notification outbox is claimed with `SKIP LOCKED` (ADR-0060), and follow-ups keep their timing in the database (ADR-0082). The flaky-test work of PR #238 made the remediation leases transactional after it caught two servers driving one incident. Other parts kept state in one process's memory: the registry of live streams, which the offline check, automatic updates and the API's "online" read; the threshold engine's copy of every series' state, loaded once at start; the CA's revocation list on disk; and the command dispatcher, which only hears about commands queued in its own process.

## Decision

### What high availability means here

Two or more instances of the server share one PostgreSQL database, and agents, relays and browsers reach any of them under one name. When an instance's process or host dies, or stops for an update:

- its agents and relays reconnect to another instance within their backoff (at most 60 s, ADR-0009);
- the threshold engine, offline incidents, notifications, follow-ups, on call, campaigns, automatic updates and AI remediation continue on the other instances, within 30 s for the work only one instance does.

Not covered yet: PostgreSQL, VictoriaMetrics and VictoriaLogs stay single, on the first server's host in the compose. Without PostgreSQL nothing works; without VictoriaMetrics or VictoriaLogs ingest behaves as ADR-0013 says. Mirror repositories, backups and the browser certificate stay on the first host (below). Replicating the stores is a later decision.

A single server is one instance: it behaves as before, with nothing to set.

### Instances

Each process joins with a random ID in `server_instances` and, every 5 seconds, refreshes its row, removes instances silent for 30 seconds (and the streams they held), renews or takes the leader lease, writes the streams it holds and reads the others', and revokes what another instance revoked. A process stopping cleanly removes its row and gives up the lease, so another instance takes over at its next tick rather than after the lease expires. The table shows operators which instances run, on which host, at which version.

### Every background loop

Three kinds of loops, so each runs correctly with any number of instances, including two for a moment during an update or after a crash:

| Loop | Runs on | Why it is correct with several instances |
| --- | --- | --- |
| Command dispatcher, relay command sender (ADR-0031, ADR-0055) | every instance, for the streams it holds | Commands live in the database. `mark_sent` is conditional and the unique index keeps one command in flight per server. A trigger on `commands` and `relay_commands` sends `NOTIFY` with the peer's ID; the instance holding the stream sends at once, and the 10 s poll remains the fallback |
| Threshold engine (ADR-0017) | every instance, for the streams it holds | Below |
| Sensor configuration watcher, `last_seen_at`, live settings (ADR-0016, ADR-0069) | every instance | Each reads the database for its own streams or itself |
| CRL refresh (ADR-0007) | every instance | Each signs its own CRL from its copy of the CA; revocations are synced (below) |
| Campaign runner (ADR-0033) | every instance | Each campaign advances in one transaction holding its row (`FOR UPDATE SKIP LOCKED`); the commands it queues are in that transaction |
| AI remediation (ADR-0043) | every instance | A lease per incident, renewed inside every write of a pass (PR #238); an incident parked or closed drops its lease in the same transaction |
| Mirror scheduler, manual mirror jobs, retention (ADR-0030, ADR-0074) | every instance | One job per mirror under a session advisory lock; retention takes the all-mirrors lock alone. The lock goes with the connection when a process dies |
| Command expiry, session pruning, certificate renewal, enrollment | every instance | Idempotent `UPDATE`/`DELETE`, or a row or advisory lock per peer |
| Offline check (ADR-0072) | the leader | It needs every instance's streams, which it now reads; opening is `ON CONFLICT DO NOTHING` and resolving is conditional, so two leaders for a moment write one incident (tested) |
| Notification dispatcher, with silences, grouping, on call, follow-ups, failed backups and pruning (ADR-0060, ADR-0079 to ADR-0082) | the leader | Grouping a customer's events and pacing each chat channel need one sender. Claims are `SKIP LOCKED`, so two dispatchers never claim one event twice (tested) |
| Automatic updates (ADR-0055) | the leader | The allowance per site counts the whole fleet; the claim per server and version is `ON CONFLICT DO NOTHING`, so an overlap never queues an update twice |
| Release check (ADR-0061) | the leader | Once a day is enough; recording is idempotent |

**The leader** is whoever holds the row `leader` of `cluster_leases`. The holder renews it every 5 s for 30 s; anyone takes it once it has expired. The holder counts its own deadline from before it sent the renewal and stops its leader loops one heartbeat before the lease can expire, also when a tick is stuck on the database, so two leaders overlap only if clocks drift by more than 5 s in 30. Leadership is not what keeps these loops correct: each is also correct twice, as the table says. It keeps grouping and pacing whole and avoids doing the same work twice.

A lease row was preferred to a session advisory lock. An advisory lock lives on one pooled connection: if that connection drops while the process lives, the lock is gone and the process does not know until it checks, which is the same renewal loop with less visibility. The row shows who leads, works through a connection pooler, and has an expiry the holder can measure.

**A crash in the middle of a job**:

- A command sent by a dead instance stays `sent`. Its agent reconnects to another instance, which sends it again, and the agent answers from its journal (ADR-0031); the result is recorded once (tested).
- The threshold engine writes each change in one transaction; the next instance reads what was stored.
- A campaign step is one transaction. A remediation lease expires after 60 s and another instance resumes the incident from its events. A mirror job's lock goes with its connection, and the next job marks the run interrupted.
- Events a dying leader claimed but had not delivered are lost: the dispatcher is at most once (ADR-0060), with one server as with several. Making it at least once is a follow-up.

### The threshold engine

The engine loaded every series' state at start and kept it. With two instances, an agent moving from A to B and back to A was evaluated from A's old copy: an incident B had resolved stayed open in A's memory, so the next breach updated nothing and no incident opened (the test fails that way without the change). Now the engine reads an agent's stored states at its first batch, and forgets them when its stream opens or closes on that instance, through a queue it takes before any batch. Updating or resolving an incident that is no longer open records no event.

### Agents and relays

They reach "the server" through one DNS name with one address per instance. Nothing changes in their configuration, enrollment or install command: no list of servers, which would be a new setting to keep in sync on every host. Clients now resolve the name and try each address with its own timeout, from a random one; before, one timeout covered the whole connect, so a dead first address failed every attempt. A TCP load balancer or a virtual IP in front of the instances works as well, as long as it passes TLS through: mTLS ends at the server.

Each instance writes the streams it holds to `peer_streams`, changes only, and reads the others' into its registry. The API's online status, the relay pages, the offline check and automatic updates use this merged view; sending stays on the instance holding the stream. Streams of an instance silent for 30 s disappear with it.

### The CA and what lives on disk

- **CA key and certificate**, which also sign commands: the same copy on every instance, in `serverdata/ca`, copied when a server is added. They stay out of the database, so a database leak is not a CA compromise.
- **Revocations**: `certificates.revoked_at` in the database is the reference. Every 5 s each instance revokes in its own `revocations.json` what it lacks, publishes a new CRL and closes those peers' streams (tested). CRL numbers differ between instances; relays do not compare them.
- **Server certificate**: each instance has its own, for the shared names, from the shared CA.
- **Mirror repositories** stay on the first server. Relays keep serving their copies when it is down.
- **Backups and server updates** (ADR-0056, ADR-0058): each host's updater updates its own instance and backs up the shared database and its own `serverdata`.
- **Browser certificate** (ADR-0068): with the internal CA, every instance needs the same Caddy CA, copied with `serverdata`. ACME's HTTP-01 challenge may reach the instance that did not ask; that is not solved yet.

### Sessions and the web interface

Sessions, setup links, one-time links and settings are in PostgreSQL, so any instance serves any browser. The static interface is the same on every instance. In-memory limits, such as enrollment failures per address, count per instance.

### Installing and upgrading

A single server needs nothing: the migration adds three tables, an index and two triggers, and the server leads as soon as it starts. After a crash rather than a clean stop, the leader loops wait for the old lease to expire, at most 30 s.

During an update the old and new versions of two instances run side by side, so migrations must stay compatible with the previous release, as the protocol already must.

### Left for the next increments

1. Packaging a second server: `install-server.sh --join <first server>` copies `serverdata/ca` and the database credentials, starts only the server and web services, and reaches PostgreSQL, VictoriaMetrics and VictoriaLogs on the first host over TLS (which depends on the Phase 12 storage TLS task); the guide says which DNS records to add.
2. The browser certificate with ACME on several hosts.
3. Mirror jobs requested on an instance without the mirror tree: hand them to the instance that has it.
4. Notifications at least once across a leader crash.
5. Failed backups and update status of every host, not only the leader's.
6. Replicated PostgreSQL, VictoriaMetrics and VictoriaLogs.

## Consequences

- Two instances on one database are correct today, as `crates/server/tests/ha.rs` shows with two instances in one process: commands reach the agent on the other instance and survive its crash, one instance leads and another takes over, streams on another instance count as online until it dies, an agent moving between instances opens and resolves incidents once, revocations reach the other instance, and two dispatchers claim each event once.
- A second server cannot be installed with one command yet; that is the next increment.
- Each instance writes a heartbeat every 5 s and reads the other instances' streams, a few small queries, and holds one more database connection for `LISTEN`.
- The engine reads an agent's state from the database each time its stream opens, instead of once for the whole fleet at start.
