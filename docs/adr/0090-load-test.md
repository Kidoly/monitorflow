# ADR-0090: Load test on 1,000 agents

Date: 2026-11-03
Status: accepted

## Context

The first version targets about 1,000 servers (architecture §1), and Phase 12 asks for proof before real customers: a load test on about 1,000 agents, and fixes for what it finds. Nothing measured the server at that size before. The end-to-end test (`crates/e2e`) runs one or two real agents; a thousand agent processes, each with its own data directory, systemd unit and host, are not something a developer or CI can start.

## Decision

**A load generator of simulated agents, in the workspace.** `crates/loadtest` builds `monitorflow-loadtest`, a test-only binary that is never packaged. It runs the real `monitorflow-server` and `monitorflow-relay` binaries as child processes, configured from files and the REST API the way an operator does, and a fleet of agents inside its own process. Each simulated agent has its own enrolled identity and mTLS certificate on disk and its own connection, and runs the agent library's code for everything that crosses the wire: enrollment, the stream with its reconnect backoff and upload buffer, the command dispatcher with its signature checks and journal, `CertificateService::Renew`, package reports. Only the host is simulated: sensors produce samples with the metrics, instances and labels of the real `cpu`, `memory` and `disk` sensors at the intervals the server configures, and command handlers answer after a delay without running anything. The server side is never faked.

**Phases, in one run:** enrollment and first connection (paced like a rollout), a steady state, a command to every server through the API, a renewal of every agent, a server restart (reconnection storm), and an alert storm: every server crosses a CPU threshold at once, incidents open and notifications go to a webhook, then everything resolves. A share of the agents (20 % by default) goes through a relay.

**What it measures**, sampled every 5 seconds: CPU and memory of the server, relays and generator from `/proc`; PostgreSQL connections and active ones; VictoriaMetrics' accepted rows a second; established gRPC connections; per phase, the slowest statements from `pg_stat_statements` and the phase's own times (connection, reconnection and first batch per agent, command latency, notification latency from the outbox, the API calls of the fleet overview and the incident list). It writes a Markdown report.

**One command.** `scripts/load-test.sh` starts PostgreSQL (with `pg_stat_statements` and a log of checkpoints, lock waits and slow statements), VictoriaMetrics and VictoriaLogs at the production versions in a compose project of their own (`deploy/docker-compose.loadtest.yml`, loopback ports 25432, 28428 and 29428 by default), builds and runs the generator, keeps the PostgreSQL log, and removes the containers. Results and the hardware are in [docs/load-test.md](../load-test.md).

**A small run in CI.** The `load-test` job runs 100 agents for 30 seconds of steady state, a command to each and a server restart, on debug builds, with `--strict`: it fails when an agent does not connect or come back or a command does not finish. It costs one parallel job of about ten minutes and catches regressions in the paths that only break at scale, such as a stream refused while many reconnect. The alert storm stays out: it takes minutes of sample time by design (a sensor interval, then the threshold's duration).

**Fixes from the findings:**

- *Sessions*: every API request wrote its session's `last_seen_at`. The web interface sends several requests at once, which then queued on that row's lock while holding pool connections; during the 1,000-server alert storm the incident list's calls took 4.9 s at p95 and all 16 pool connections were busy. The idle timer is now written when it is a minute old, or a tenth of a shorter idle timeout, so a session may expire that much earlier than its exact idle time. Requests otherwise only read.
- *`last_seen_at` of streams*: each stream spawned a task for its write, once a minute. When commits stalled for seconds, those tasks piled up on the pool (342 timed out in one storm) and starved Hellos and the threshold engine. One task now writes them: a stream queues its ID without waiting, and the task writes what gathered in one statement per role, so a slow database holds one connection.
- *Command dispatcher sweep*: every 10 s it queried each connected agent's next command, one query per agent, 100 a second at 1,000 agents and nearly all empty. One query now names the agents with a command.

**What stays, measured.** The pool keeps its 16 connections: after the fixes, at most 2 ran at once. The reconnect backoff already has full jitter from 1 s doubling to 60 s (ADR-0009): 1,000 agents were back within 2 s of a restart, with no stream refused. The time from an incident to its notification is the grouping window of ADR-0080 (10 s for a quiet customer, then at most one claim a minute), not a queue: p95 under a minute for 1,000 incidents at once. No setting was added.

## Consequences

- The load test runs again with one command, on any Linux host with Docker, and its report is comparable between versions.
- Renewal is refused within an hour of the last certificate (ADR-0010); a run shorter than that measures refused `Renew` calls only, and says so.
- The simulated agents share the generator's host with the server and stores, so the CPU numbers are of one machine doing everything; a production server has the host to itself.
- Not covered: Windows agents (same protocol, other sensors), the AI remediation loop, campaigns in waves, email and chat channels (webhooks were used, one request per event), relays chained, several thousand agents, and a server on a separate host. The high availability work (Phase 12) changes the background loops and command dispatch; its leader election is the place for the follow-ups listed in docs/load-test.md.
