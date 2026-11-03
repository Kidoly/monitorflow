# Load test

How the server behaves with 100, 500 and 1,000 agents, what broke on the way, and how to run the test again. The decisions are in [ADR-0090](adr/0090-load-test.md).

## Running it

```
scripts/load-test.sh                                  # 1,000 agents, every phase, about 20 minutes
scripts/load-test.sh --agents 100 --steady 60         # a quick look
scripts/load-test.sh --agents 1000 --hold 900         # then look at the web interface for 15 minutes
```

It needs Docker, the Rust toolchain and about 2 GB of memory. The script first issues the stores' certificate and password with `monitorflow-server storage-certs`, as the production compose's `storage-certs` service does (ADR-0087), then starts PostgreSQL 17, and VictoriaMetrics and VictoriaLogs over TLS with basic auth, at the production versions, in a compose project of their own (`monitorflow-loadtest`, loopback ports 25432, 28428 and 29428, set with `LOADTEST_POSTGRES_PORT`, `LOADTEST_VICTORIAMETRICS_PORT` and `LOADTEST_VICTORIALOGS_PORT`), builds the release server, relay and `monitorflow-loadtest`, runs them, prints the report, and removes the containers and their data. The report is also in `target/loadtest/report.md`, next to the server's and relays' logs, the agents' identities, and PostgreSQL's log in `target/load-test-postgres.log`. `monitorflow-loadtest --help` lists the options: number of agents, share behind relays, number of relays, ramp rate, length of the steady state, phases, and `--strict`, which fails the run when a goal is missed.

CI runs 100 agents with a 30-second steady state, a command to each and a server restart, on debug builds, with `--strict` (the `load-test` job, about ten minutes, its report kept as an artifact).

## What it does

`monitorflow-loadtest` starts the real server with a configuration file, adds an administrator with `monitorflow-server user add`, logs in to the REST API, creates a site for direct agents and one per relay, starts the relays with single-use tokens, and creates one single-use token per agent. Then it starts the agents, 50 a second by default, in its own process.

Each simulated agent enrolls with its token, keeps its key and certificate in its own directory, and opens its own mTLS connection, to the server or to its relay. It runs the agent library's code for the stream (Hello, heartbeats every 30 s, the upload buffer flushed every 10 s, reconnects with backoff and full jitter), the command dispatcher (signature check against the CA, journal) and `Renew`. It sends a package report of 400 packages when it connects. Its sensors follow the configuration the server pushes: the built-in `cpu`, `memory` and `disk` every 60 s, with the metrics, instances and labels of the real sensors, for hosts of 2 to 16 cores and one or two filesystems, 26 samples a minute on average. Commands succeed after 2 seconds without running anything. A quarter of the hosts say they are Rocky 9, the rest Debian 12.

Phases:

1. **Enrollment and first connection** of the whole fleet.
2. **Steady state**, 5 minutes by default; at its end the API calls of the fleet overview (every page of `/api/servers` and the open incidents) and of the incident list (sites, servers, the first 50 incidents and the open ones) are timed 20 times each.
3. **A command to every server**: `apply_updates` through `POST /api/servers/{id}/commands`, until every result is recorded.
4. **Certificate renewal of every agent** through `Renew`.
5. **Server restart**: SIGTERM, start again at once; until the server shows every agent on a new stream (agents behind a relay keep their stream to it), and every direct agent sent a batch again.
6. **Alert storm**: a webhook channel to a receiver in the generator, a critical threshold on `monitorflow_cpu_usage_ratio{instance="total"}` above 0.9 for 60 s on every site, then every host at 97 % CPU at once; until every incident is open and its webhook arrived; then back to normal until every incident resolves and its webhook arrived. The page timings are taken again at the end.

Every 5 seconds it samples the CPU and resident memory of the server, the relays and itself from `/proc`, PostgreSQL's connections and active ones, VictoriaMetrics' accepted rows a second, and the established connections to the gRPC port. Each phase lists its 12 statements of most total time from `pg_stat_statements`.

## Setup of the runs below

- Intel Core i5-14600K (14 cores, 20 threads), 63 GB of memory, NVMe SSD (WD_BLACK SN850X) under btrfs on LUKS; Arch Linux, kernel 7.2, Docker 29.
- Server, relay, stores and generator on that one host, which other work shared during the runs. PostgreSQL in Docker managed about 1,000 `fdatasync` a second (`pg_test_fsync`).
- Release builds of commit `725d0f4` for the first three columns, and of this branch rebased on high availability and store TLS for the last one; PostgreSQL 17, VictoriaMetrics v1.155.0, VictoriaLogs v1.53.0, all with their default settings; the server with its 16-connection pool.
- One relay; 20 % of the agents behind it.

## Results

With the fixes below. Means over the phase, maximum in brackets; CPU in percent of one core. The first three columns ran before high availability (ADR-0089) and TLS to the stores (ADR-0087) were merged, with the stores over plain HTTP; the last one ran the same 1,000-agent test on top of both, one server instance, the stores over TLS with basic auth as in production.

| | 100 agents | 500 agents | 1,000 agents | 1,000, after HA and store TLS |
| --- | --- | --- | --- | --- |
| All connected (50 new agents a second) | 3 s | 10 s | 20 s | 20 s |
| Server CPU during enrollment | under 5 s of samples | 39 (41) | 40 (41) | 23 (28) |
| Server CPU, steady state | 1 (17) | 1 (8) | 2 (9) | 2 (3) |
| Server memory, steady state | 43 MiB | 84 MiB | 136 MiB (142) | 138 MiB (143) |
| Relay CPU and memory, steady state | 0, 18 MiB | 0, 32 MiB | 0 (2), 49 MiB | 0 (2), 48 MiB |
| Samples into VictoriaMetrics | 42 a second | 216 a second | 433 a second (502) | 433 a second (485), over TLS |
| PostgreSQL connections, of which active | 17, 0 | 17, 0 | 17, 0 (1) | 17, 0.1 (1) |
| Fleet overview API calls | 15 ms p50, 20 ms p95 | 42 ms, 54 ms | 83 ms, 90 ms | 20 ms, 40 ms; 89 ms, 97 ms after the storm |
| Incident list API calls | 15 ms, 17 ms | 42 ms, 48 ms | 85 ms, 95 ms | 20 ms, 32 ms; 86 ms, 95 ms after the storm |
| A command to every server: all results in | 3.1 s | 4.3 s | 5.6 s (p95 4.6 s per command) | 11.4 s (p95 9.8 s), see below |
| Server CPU while commands run | | 35 | 50 | 23 (42) |
| Restart: every stream open again at the server | 1.9 s (p95 1.0 s) | 1.6 s (1.0 s) | 2.0 s (1.0 s) | 1.9 s (1.0 s) |
| Restart: first batch of a direct agent | p50 20 s, max 51 s | p50 11 s, max 51 s | p50 11 s, max 61 s | p50 11 s, max 61 s |
| Server CPU during the reconnection storm | 0 (1) | 1 (2) | 4 (32) | 1 (2) |
| Alert storm: every incident open after the breach | 185 s | 184 s | 186 s | 185 s |
| Opening notifications: outbox to sent | p95 60 s | p95 65 s | p50 10 s, p95 15 s, max 60 s | p50 10 s, p95 14 s, max 14 s |
| Resolutions: outbox to sent | p95 60 s | p95 51 s | p50 54 s, p95 59 s | p50 45 s, p95 50 s |
| Every webhook delivered after the breach | 209 s | 229 s | 203 s | 194 s |
| Errors and warnings in the server's log | none | none | none | 8 slow single-row writes (1 to 6 s), no timeout |

Reading the table:

- **The server is far from its limits at 1,000 agents.** Steady state costs 2 % of a core and 136 MiB. CPU peaks are TLS handshakes: 40 % of a core while 50 agents a second enroll and connect, 32 % for one 5-second sample while 1,000 reconnect, 50 % while signing 1,000 commands in 6 seconds. PostgreSQL never ran more than 6 statements at once.
- **Ingestion is light.** 433 samples a second is what 1,000 hosts of this shape send at the default 60 s interval; VictoriaMetrics took it without effort. Ten times as many sensors, or a 6 s interval, would still be within what a single VictoriaMetrics accepts, but was not measured.
- **The API grows linearly with the fleet**, as the pages fetch every server: 85 ms for the fleet overview's calls at 1,000. The servers list itself takes 5 ms in PostgreSQL for a page of 500.
- **Reconnection storm**: the agents' backoff (full jitter from 1 s, ADR-0009) spreads 1,000 reconnections over 2 seconds, and no stream was refused. The first batch after the restart waits for the next sensor collection (every 60 s) and the next flush (10 s): no sample is lost meanwhile, the agents keep them. Agents behind the relay kept their stream to it; the relay reopened their upstream streams and replayed what it buffered.
- **Alert storm**: 186 s is the threshold's design, not a delay: up to 60 s until the next collection sees 97 %, 60 s for the threshold's duration, up to 60 s until the next collection confirms it. The threshold engine evaluated all of it on one task without falling behind. Notifications follow the grouping of ADR-0080: a customer's events go together after 10 s of quiet, then at most once a minute, so the opening of 1,000 incidents at once reached the webhook within a minute, most within 15 s. Each webhook gets one request per event (1,000 requests); chat and email would get grouped messages.
- **After high availability and store TLS** (last column), the server's work is the same: its 5-second instance heartbeat and `peer_streams` writes (only the streams that changed) are a few statements a minute, and per-agent threshold state loaded when a stream arrives did not show in the reconnection storm. HTTPS to VictoriaMetrics cost nothing visible. One number moved: a command to every server took 11.4 s instead of 5.6 s. 852 of the 1,000 commands went out within 2 s; the other 148 waited for the dispatcher's 10-second sweep. Each queued command now wakes the dispatcher twice, from the API and from its NOTIFY (ADR-0089), and 2,000 wakes overflow its queue of 1,024, so the excess waits for the sweep; listed below. Eight single-row writes took 1 to 6 s during the steady state, this shared host's disk stalling; with the single `last_seen_at` writer nothing piled up and no request waited for the pool.
- **Renewal**: the server issues a peer a certificate at most once an hour (ADR-0010), so right after enrollment every `Renew` was refused, 1,000 in 0.3 s. A real renewal wave happens in the last third of the certificates' 7 days, spread by each agent's hourly check with 5 minutes of jitter; a run long enough to renew was not done.

### The web interface at 1,000 servers

Measured once in headless Chromium against the production build (`npm run build`), served with the API behind a local proxy as Caddy does, on a run held with `--hold` after an alert storm (1,000 servers online, 1,000 resolved incidents), from navigation to the data on screen, five loads after a first one:

- Fleet overview: 160 to 200 ms median, 300 ms at most. It lists the servers with problems first and folds the healthy ones; showing all 1,000 (50 rows a page) takes 80 ms more.
- Incident list: 80 to 110 ms median, 280 ms at most, 50 rows.

The test script is not in the repository: it needs a browser and Node.js next to Rust, for one measure that the API timings of every run follow closely.

## What the test found, and what was fixed

1. **Every API request wrote its session row** (`UPDATE sessions SET last_seen_at`). The web interface sends several requests at once, so they queued on that row's lock, each holding a pool connection. During the first 1,000-agent alert storm, the incident list's calls took 4.9 s at p95 (5.8 s max), the statement 188 ms on average and 5.7 s at worst, and all 16 pool connections were busy. Fixed: the idle timer is written when it is a minute old (or a tenth of a shorter idle timeout); other requests only read. Same storm afterwards: 99 ms at p95, at most 2 statements active.
2. **Each stream spawned its own task to write `last_seen_at`**, once a minute. In that same storm, commits stalled for 1 to 20 seconds (on this shared host; the rerun did not stall), the pool was exhausted, and those tasks piled up: 342 writes timed out after 5 s, while Hellos, the command dispatcher and the notification dispatcher waited for connections. Fixed: one task writes them, in one statement per role for whatever gathered during the previous write, so a slow database holds one connection and the backlog collapses into one statement.
3. **The command dispatcher's 10-second sweep ran one query per connected agent**: 100 queries a second at 1,000 agents, nearly all finding nothing. Fixed: one query names the agents with a command in flight or queued.

Each fix has a test: `requests_refresh_the_idle_timer_once_a_minute` (`crates/server/tests/api.rs`), `peers_queued_during_a_write_go_in_the_next_one` (`crates/server/src/grpc/seen.rs`) and `the_sweep_finds_the_agents_next_would_answer_for` (`crates/server/tests/commands.rs`).

Checked and left as they are:

- **The 16-connection pool** is enough: at most 6 statements ran at once after the fixes, during the reconnection storm. More connections would only have hidden the session queue.
- **Reconnect jitter and backoff** already existed and work, see above.
- **Channels**: the queues between streams, ingest and the threshold engine are bounded and drop with a counter when full; none dropped anything (the engine's and writers' counters in the server's log show `dropped: 0`).
- **Indexes**: every frequent statement used an index; nothing in the reports needed a new one, so there is no migration.

## Limits and follow-ups

Not fixed here, in order of interest:

- **The notification dispatcher claims at most 100 events a pass**, every 5 s. 1,000 events take ten passes; with the one-minute window of ADR-0080 this stayed under a minute, but a fleet of several thousand opening at once would see the last notifications minutes late. Belongs with the notification work, not a constant to raise blindly.
- **The threshold engine writes each transition in its own transaction, on one task** (ADR-0017). At 1,000 incidents opening within a minute that was 2,000 transitions in short transactions, fine here; it is the first thing to batch beyond a few thousand servers.
- **The command dispatcher is one task** with a wake queue of 1,024 agents. Since ADR-0089 a queued command wakes it twice (the API, then its NOTIFY), so 1,000 commands queued at once overflow the queue and about 15 % wait for the 10-second sweep. Skipping the API's wake when the NOTIFY listener runs, or a larger queue of distinct agents, would fix it; it is ADR-0089's design, so it is listed here rather than changed.
- **The background loops** (offline check, sensor configuration and threshold watchers, campaign runner, dispatchers) each poll on their own timer; since ADR-0089 the offline check, the notification dispatcher and automatic updates run on the leader only. Their statements took 1 to 3 ms a pass each at 1,000 servers. Two server instances were not load tested.
- **Each relay opens one upstream stream per agent** (ADR-0025): 200 agents behind one relay meant 200 streams from it, 49 MiB for the relay. Multiplexing them is the `relay-mux` TODO in `grpc/service.rs`.
- **`last_seen_at` is still one write a minute per agent**, now batched opportunistically; at 1,000 agents about 17 small statements a second.

Not tested: two server instances (ADR-0089) under load, Windows agents (same protocol, other sensors), chained relays, the AI remediation loop, update campaigns in waves, email and chat channels, mirrors, several thousand agents, a server on its own host, a renewal wave, and the other pages of the web interface.
