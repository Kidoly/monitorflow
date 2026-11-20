# Load test

How the server behaves with 100, 500 and 1,000 agents, then with 100,000 sensors on 2,000 agents and 800 devices, what broke on the way, and how to run the test again. The decisions are in [ADR-0090](adr/0090-load-test.md).

## Running it

```
scripts/load-test.sh                                  # 1,000 agents, every phase, about 20 minutes
scripts/load-test.sh --agents 100 --steady 60         # a quick look
scripts/load-test.sh --agents 1000 --hold 900         # then look at the web interface for 15 minutes
scripts/load-test.sh --agents 2000 --sensors-per-agent 48 --devices 800   # 100,000 sensors, about 30 minutes
```

It needs Docker, the Rust toolchain and about 2 GB of memory. The script first issues the stores' certificate and password with `monitorflow-server storage-certs`, as the production compose's `storage-certs` service does (ADR-0087), then starts PostgreSQL 17, and VictoriaMetrics and VictoriaLogs over TLS with basic auth, at the production versions, in a compose project of their own (`monitorflow-loadtest`, loopback ports 25432, 28428 and 29428, set with `LOADTEST_POSTGRES_PORT`, `LOADTEST_VICTORIAMETRICS_PORT` and `LOADTEST_VICTORIALOGS_PORT`), builds the release server, relay and `monitorflow-loadtest`, runs them, prints the report, and removes the containers and their data. The report is also in `target/loadtest/report.md`, next to the server's and relays' logs, the agents' identities, and PostgreSQL's log in `target/load-test-postgres.log`. `monitorflow-loadtest --help` lists the options: number of agents, share behind relays, number of relays, ramp rate, length of the steady state, phases, the checks to add (below), and `--strict`, which fails the run when a goal is missed. With `CARGO_TARGET_DIR` set, the script runs the binaries from there.

CI runs 100 agents with a 30-second steady state, a command to each and a server restart, on debug builds, with `--strict` (the `load-test` job, about ten minutes, its report kept as an artifact).

## What it does

`monitorflow-loadtest` starts the real server with a configuration file, adds an administrator with `monitorflow-server user add`, logs in to the REST API, creates a site for direct agents and one per relay, starts the relays with single-use tokens, and creates one single-use token per agent. Then it starts the agents, 50 a second by default, in its own process.

Each simulated agent enrolls with its token, keeps its key and certificate in its own directory, and opens its own mTLS connection, to the server or to its relay. It runs the agent library's code for the stream (Hello, heartbeats every 30 s, the upload buffer flushed every 10 s, reconnects with backoff and full jitter), the command dispatcher (signature check against the CA, journal) and `Renew`. It sends a package report of 400 packages when it connects. Its sensors follow the configuration the server pushes: the five built-in ones (`cpu`, `memory`, `disk`, `diskio`, `net`) every 60 s, with the metrics, instances and labels of the real sensors, for hosts of 2 to 16 cores, one or two filesystems and disks, and one interface, and the `systemd` and `port` checks below. Commands succeed after 2 seconds without running anything. A quarter of the hosts say they are Rocky 9, the rest Debian 12.

Checks, when `--sensors-per-agent` is above 5 or `--devices` above 0: a sensor is one monitored thing, one row of the configuration a server or device runs (a built-in sensor counts once, whatever its disks or interfaces). Each agent runs the 5 built-in sensors plus checks added through the API as the interface adds them, each with its alert (critical below 1 for 60 s): two services (`systemd`, one unit each) for one port (`port`). `--own-sensors` of them (5 by default) are rows of each server, the rest rows of its site. `--devices` adds devices through `POST /api/devices`, each with its ping and `--device-ports` port checks (4 by default); the share behind relays goes to the relays' sites, so their relay runs them, the others are run by the central server. Their addresses are loopback addresses 127.1.x.y, on which the generator listens on the checked ports. The report counts the sensors the server lists (`GET /api/servers/{id}/sensors` of every server) and fails the phase when it differs from the plan.

Phases:

1. **Enrollment and first connection** of the whole fleet.
1. **Checks added through the API**, with checks to add: every site's, then every server's own, then the devices and theirs, 16 requests at a time; until every agent runs and acknowledged its configuration; then one site check changed, until every agent of the site acknowledged it.
2. **Steady state**, 5 minutes by default; at its end the API calls of the fleet overview (every page of `/api/servers` and the open incidents) and of the incident list (sites, servers, the first 50 incidents and the open ones) are timed 20 times each, and so are a server's page (the server, its sensors and their values, its thresholds and open incidents) for 20 servers across the fleet.
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

## 100,000 sensors

### Setup

- The same host, release builds of this repository's 0.9.1 with the fixes below, the same stores and pool. The host was shared with other work during the runs.
- 2,000 agents, 400 of them behind one relay, 50 new ones a second. 48 sensors each: the 5 built-in ones, 38 checks of their site (26 services and 12 ports) and 5 of their own (4 services, 1 port): 96,000 sensors.
- 800 devices, 160 of them run by the relay, each with a ping and 4 port checks: 4,000 sensors.
- 100,000 sensors in all, as the server lists them; every added check with its alert (27,352 API requests). Default phases. No host limit was reached: 2,000 mTLS streams, 400 more to the relay, and 3,200 listeners for the devices' ports fit in this host's open file limit and memory.

### Results

Means over the phase, maximum in brackets; CPU in percent of one core. First run: before the fixes; after: the same run on top of the four fixes.

| | First run | After the fixes |
| --- | --- | --- |
| All connected (50 new agents a second) | 40 s | 40 s |
| Checks added: 27,352 requests | 39 s, 695 a second, p50 16 ms, p95 32 ms | 40 s, 690 a second, p50 16 ms, p95 33 ms |
| Every agent runs and acknowledged its configuration | not after 600 s: 1,035 shown out of sync | at once after the last request |
| One site check changed: its 1,600 agents acknowledged | not after 300 s: 575 left out of sync | 6 s |
| `GET /api/servers/{id}/sensors`, each of 2,800 servers | p50 3 ms, p95 9 ms | p50 4 ms, p95 9 ms |
| Samples collected and sent | 7,283 a second | 7,283 a second |
| Rows into VictoriaMetrics, steady state | 7,422 a second (8,001) | 7,422 a second (8,001) |
| Server CPU, steady state | 19 (60) | 20 (97) |
| Server memory, steady state | 460 MiB | 458 MiB (463) |
| Relay CPU and memory, steady state | 2 (4), 99 MiB | 2 (4), 99 MiB |
| PostgreSQL active, steady state | 0.1 (1) | 0.1 (1) |
| Fleet overview API calls | 526 ms p50, 571 ms p95 | 478 ms, 511 ms |
| Incident list API calls | 524 ms, 546 ms | 479 ms, 494 ms |
| A server's page API calls | 38 ms, 142 ms | 36 ms, 137 ms |
| A command to every server: all results in | 13.3 s (p95 9.9 s per command) | 13.8 s (p95 8.8 s) |
| Restart: every stream open again | 2.5 s (p95 1.3 s) | 3.8 s (p95 1.2 s) |
| Restart: first batch of a direct agent | p50 10.7 s, max 12.7 s | p50 10.7 s, max 13.9 s |
| Alert storm: 2,000 incidents open after the breach | 184 s | 185 s |
| Opening notifications: outbox to sent | p50 74 s, p95 84 s, max 87 s | p50 31 s, p95 59 s, max 63 s |
| Resolutions: outbox to sent | p50 75 s, p95 94 s, max 99 s | p50 30 s, p95 59 s, max 63 s |
| Errors and warnings in the server's log | none but the incidents themselves | the same |

Reading the table:

- **Ingestion holds at 100,000 sensors.** 7,300 samples a second reached VictoriaMetrics with nothing dropped or rejected; the threshold engine evaluated every batch on its one task, with no drop, no write error and no batch out of order, and opened 2,000 incidents in the same 185 s as at 1,000 agents. The server used 20 % of a core and 460 MiB in the steady state, against 2 % and 136 MiB for 3,000 sensors: memory follows the series the engine and the stream buffers hold.
- **The configuration push** carried 100,000 sensors to 2,000 agents through the watcher's 10-second poll with no backlog: a site change reaches all its agents within one poll. The configuration a server loads at Hello and at each change took 1.6 ms in PostgreSQL with 43 rows.
- **The sensor pages and API** stay flat: the sensors of one server in 4 ms, a server's whole page in 36 ms at p50 (its sensor values are three VictoriaMetrics queries whatever the number of sensors). The fleet overview and the incident list fetch every server, 2,800 here, in six pages: half a second, linear in the fleet.
- **PostgreSQL** ran at most 15 statements at once, while 16 clients added checks; in the steady state 0.1. The heaviest steady statements were the server list (30 ms a page of 500) and the probes' read of their checks (40 ms, below).

### What it found, and what was fixed

1. **Acknowledgements of sensor configurations were dropped.** A site change reaches its agents in one poll, and they all acknowledge within a second; the watcher queued 1,024 acknowledgements and wrote them one statement each, so at 1,600 agents the rest were lost, and those servers showed out of sync until they reconnected. The queue now holds 16,384 and the watcher writes whatever gathered in one statement. Test: `a_whole_site_acknowledging_at_once_is_recorded` (`crates/server/tests/sensor_config.rs`).
2. **The notification dispatcher sent 100 events every 5 s.** When 2,000 incidents opened together, every event was due after the first claim (ADR-0080), yet the last waited 87 s and the last resolution 99 s. It now passes again at once while a pass claims a full batch, up to 50 in a row. What is left is the grouping window of ADR-0080: events written after a customer's claim wait for its next window, a minute. Test: `a_storm_of_due_events_goes_out_in_one_round` (`crates/server/tests/notifications.rs`).
3. **The command dispatcher's wake queue dropped wakes** (listed below since the 1,000-agent run): it held 1,024 and each command queued through the API wakes it twice. It now holds each waiting agent once, so it never grows past the fleet and drops nothing. The time to run 2,000 commands did not move, as the dispatcher's one task, about 250 commands a second, is now what sets it. Tests: `a_fleet_of_wakes_is_kept_once_each` and `a_wake_while_an_agent_is_looked_at_queues_it_again` (`crates/server/src/commands/dispatcher.rs`).
4. **A page of servers read the details of the whole site.** The list joined every server of the site to its open incidents, silences, acknowledgements and certificate, then sorted and kept the page, so each of the six pages of a 2,240-server site paid for all of them. The page's servers are now picked first. The statement went from 36 to 30 ms here; the gain grows with the size of a site. Test: `a_page_of_servers_reads_the_details_of_its_servers_only` (`crates/server/tests/api.rs`).

Checked and left as they are:

- **The threshold engine** on one task: no drop at 7,300 samples a second. Its reload of a server's thresholds scans every series it holds, of every server; 2,000 servers reloaded several times while their checks were added, and nothing queued behind it. It is the first thing to index by server beyond this size.
- **The fleet overview and the incident list** take half a second at 2,800 servers, as the interface fetches every server in pages of 500. Linear, and measured once in a browser at 1,000; a summary per state, already in `GET /api/servers/summary`, is the way for much larger fleets, which is a change of the page, not of the API.
- **The command dispatcher** sends about 250 commands a second, one task signing and writing each. 2,000 commands took 14 s; campaigns send in waves.
- **Device creation** takes the one lock on agent, relay and device IDs, so no two get the same ID: with 16 requests at once, each waited 160 ms on average for it, 800 devices in about 15 s. Adding devices one by one does not see it.

## Limits and follow-ups

Not fixed here, in order of interest:

- **The probes read all their checks every 5 s**: at 4,000 checks on 800 devices that statement took 40 ms, for the central server and each relay. A revision, as for agents' configurations, would read them only when they change.
- **The dependency facts** read every check name (`SELECT DISTINCT name FROM sensor_configs`, 5 ms at 13,000 rows) at each look: every 5 s in the threshold engine and with every page of servers.
- **The threshold engine writes each transition in its own transaction, on one task** (ADR-0017). At 1,000 incidents opening within a minute that was 2,000 transitions in short transactions, fine here; it is the first thing to batch beyond a few thousand servers.
- **The command dispatcher is one task**: about 250 commands a second. Its wake queue no longer drops anything (fixed with the 100,000-sensor run).
- **The background loops** (offline check, sensor configuration and threshold watchers, campaign runner, dispatchers) each poll on their own timer; since ADR-0089 the offline check, the notification dispatcher and automatic updates run on the leader only. Their statements took 1 to 3 ms a pass each at 1,000 servers. Two server instances were not load tested.
- **Each relay opens one upstream stream per agent** (ADR-0025): 200 agents behind one relay meant 200 streams from it, 49 MiB for the relay. Multiplexing them is the `relay-mux` TODO in `grpc/service.rs`.
- **`last_seen_at` is still one write a minute per agent**, now batched opportunistically; at 1,000 agents about 17 small statements a second.

Not tested: two server instances (ADR-0089) under load, Windows agents (same protocol, other sensors), chained relays, the AI remediation loop, update campaigns in waves, email and chat channels, mirrors, more than 2,000 agents, website (`http`) and SNMP checks from probes (they need targets on the host; ping and port checks were run), a server on its own host, a renewal wave, and the other pages of the web interface.
