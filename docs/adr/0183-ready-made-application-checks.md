# ADR-0183: Ready-made application checks

Date: 2026-11-24
Status: accepted; extends ADR-0148 (checks from a probe), ADR-0150 (sealed check passwords) and ADR-0146 (what the importer moves)

## Context

MonitorFlow watched databases and containers from the outside only: a service running, a port answering. A PostgreSQL that accepts connections but has run out of them, a replica three hours behind, a Redis evicting keys because it is full, a container restarting every minute: none of it showed without a script. Tools people move from have these as built-in sensors or templates (PRTG's database and Docker sensors, Zabbix's "PostgreSQL by Zabbix agent 2" and its siblings, Prometheus exporters), and the roadmap asks for PostgreSQL, MySQL and MariaDB, SQL Server, Redis, Java (JMX), Docker, Kubernetes and the main cloud providers as check templates instead of scripts.

That list is several projects. This decision delivers the databases people run most, Redis and Docker end to end, and records how the rest will follow.

What exists to build on: check kinds built by name on agents and probes (ADR-0012, ADR-0148), a password sealed per check row and given only to whoever runs it (`sensor_configs.secret`, ADR-0150, ADR-0125), the agent's `sensors.port_targets` limits (ADR-0111), and thresholds that apply to every instance of a metric (ADR-0017).

## Decision

### Four new kinds, in a crate of their own

`monitorflow-apps` (`crates/apps`) holds the kinds `postgresql`, `mysql` (MySQL and MariaDB), `redis` and `docker`. They speak the applications' own protocols, so they stay out of `monitorflow-sensors`, which has no transport code (ADR-0021). The PostgreSQL and MySQL clients are sqlx's, already in the workspace, over rustls; Redis is a bounded subset of its protocol (RESP2: `AUTH`, `INFO`) written here, about a hundred lines; Docker is HTTP/1.0 `GET` over the engine's Unix socket, with bodies bounded to 16 MiB. A collection connects, reads, and closes: no connection is kept between collections, so a restarted server is just the next collection.

Every kind writes `up` (1 when the application answered and the login worked; 0 with labels `reason`, one of `connect`, `timeout`, `tls`, `login`, `permission`, `protocol`, and `error`) and `response_seconds`, then its own metrics. A failure is a sample, not a sensor error, so a stopped database alerts like a failed website check. Counters are turned into what changed since the last collection (deadlocks, slow queries, evictions, restarts) or a rate per second, and hit ratios are computed over the interval, not since the server started, so a cache that just went cold shows.

| Kind | What it reads | Main metrics |
| --- | --- | --- |
| `postgresql` (10 or later) | `pg_stat_activity`, `pg_locks`, `pg_stat_database`, `pg_database_size`, `pg_stat_replication`, `pg_stat_wal_receiver` | connections against `max_connections`, active and idle in transaction, slow queries (running longer than `slow_after`, 30 s), longest query and transaction, waiting locks, deadlocks, cache hit ratio, transactions per second, size per database, replicas and their lag in bytes and seconds, and on a standby whether it streams and how old what it replayed is |
| `mysql` (MySQL 5.7, MariaDB 10.3 or later) | `SHOW GLOBAL STATUS`, `SHOW GLOBAL VARIABLES`, `SHOW REPLICA STATUS` (or `SLAVE`), `information_schema.tables` every 10 minutes | connections against `max_connections`, threads running, queries per second, slow queries, aborted connections, InnoDB buffer pool hit ratio, row lock waits, size per schema, replica I/O and SQL threads and `Seconds_Behind_Source` |
| `redis` (5 or later, Valkey, KeyDB) | `INFO` | clients, blocked clients, rejected connections, memory against `maxmemory`, fragmentation, evicted keys, keyspace hit ratio, keys, last snapshot and AOF write, replicas and their lag, and on a replica its link to the primary |
| `docker` (Docker or Podman) | `/version`, `/containers/json`, each container's inspection and one-shot statistics | per container: running, restarts (total and since the last collection), health check, processor share and memory against its limit; containers running and stopped |

The exact list, units and descriptions are in `monitorflow_apps::templates`, which the API serves (below), so the code, the API and the guides cannot drift apart.

### Where each runs

**Databases and Redis run where a port check runs.** By default on the server's agent, against `127.0.0.1`, which works with the out-of-the-box configuration of all three (they listen on the loopback only). An agent with `sensors.port_targets` connects only where that list allows, and a host name must be listed itself, since the clients resolve names on their own. From the form, or with `probe` in the API, the check runs instead from the central server or a relay of the site (ADR-0148), against the server's address, which is how a device or a managed database without an agent is watched. The central server never connects to a link-local address, as for website checks: an operator of one customer must not reach the instance metadata of the machine serving all of them.

**Docker runs on the server's agent only**, on Linux. Its API is a Unix socket, and a probe reaching a remote engine would need its TCP API opened, with root's power to whoever holds the certificate. The server never names a socket path: `engine` is `docker` (`/var/run/docker.sock`) or `podman` (`/run/podman/podman.sock`).

**Watched containers** are those that run, and stopped ones whose restart policy is `always` or `unless-stopped`: those meant to run. A one-off container that exited is not an incident. `containers` narrows them with a regular expression on names. At most 200 containers are looked at.

### Credentials

**The password is the check's own, sealed** in `sensor_configs.secret` like a website check's (ADR-0150), the column now documented for both. The API takes it as the parameter `password`, never stores it in `params`, never returns it (rows say `secret_set`), never writes it in the audit log (which records parameter names only), and gives it only to the agent or probe that runs the check, over their mTLS stream. An update without `password` keeps it, but only while the kind, `host` and `port` stay the same: a kept password never goes to a new address. Errors are scrubbed of it before they become the `error` label.

**A read-only account is recommended, with the least rights**, as the guides say step by step:

| Application | Account |
| --- | --- |
| PostgreSQL | `CREATE ROLE monitorflow LOGIN PASSWORD '...'; GRANT pg_monitor TO monitorflow;`. Without `pg_monitor`, PostgreSQL hides other roles' sessions and replication, and the numbers cover less |
| MySQL, MariaDB | `CREATE USER 'monitorflow'@'host' IDENTIFIED BY '...'; GRANT PROCESS, REPLICATION CLIENT ON *.* TO 'monitorflow'@'host';`. Schema sizes cover the schemas it can see. Without `REPLICATION CLIENT`, replication is left out rather than the check failing |
| Redis | `ACL SETUSER monitorflow on >... -@all +info +ping` (Redis 6), or the `requirepass` password without a user |
| Docker | No account: the agent's user joins the `docker` group (`usermod -aG docker monitorflow`, then restart the agent). That group equals root on the machine, so it is a deliberate step on the server, like Hyper-V's (ADR-0156), written in the guide and in the check's error. The check only reads |

Encryption: `tls` is `prefer` by default for PostgreSQL and MySQL (encrypted when the server offers it, its certificate not verified, what their own clients do by default), `verify` (certificate and name verified against the machine's roots) or `off`. PostgreSQL's SCRAM and MySQL's `caching_sha2_password` never send the password in clear over `prefer`. MySQL 8 without encryption needs an RSA exchange for `caching_sha2_password`, which is not built in (its RSA crate has an open timing advisory); the guide says to keep encryption on. Encrypted Redis is not checked yet: `tls` takes `off` only.

### Templates: the check and its alerts

A template is the check and the alerts that come with it. `GET /api/check-templates` lists, per kind, who runs it, its parameters with their defaults, its metrics and its default alerts. `POST .../sensor_configs` takes `"default_alerts": true` on a create to add them, in the same transaction, as ordinary thresholds named after the check (`monitorflow_<check>_<suffix>`), on the server or site of the row, where none exists on that metric for every instance. They are then changed, silenced or removed like any threshold, and every richer rule of ADR-0153 applies to these metrics. The web form's switch "Add the recommended alerts" sends it.

| Kind | Default alerts |
| --- | --- |
| `postgresql` | not answering (`up < 1`, 2 min, critical); connections over 80% warning, 95% critical (5 min); a transaction open for more than an hour (warning, 5 min); more than 10 locks waited for (warning, 5 min); a deadlock (warning); cache hit ratio under 90% (warning, 30 min); a replica more than 1 GiB behind (warning, 10 min); a standby not streaming (critical, 5 min); a standby more than 5 minutes behind (warning) or 30 (critical) |
| `mysql` | not answering (critical, 2 min); connections over 80% or 95%; more than 10 slow queries a collection (warning, 5 min); buffer pool hit ratio under 95% (warning, 30 min); more than 10 lock waits (warning, 5 min); replica I/O or SQL thread stopped (critical, 2 min); replica behind by 5 or 30 minutes |
| `redis` | not answering (critical, 2 min); memory over 80% or 95% of `maxmemory` (5 min); keys evicted for 15 minutes (warning); a connection rejected (warning); a failed snapshot or AOF write (critical); a replica's link to its primary down (critical, 2 min) |
| `docker` | the engine not answering (critical, 2 min); a container not running (critical, 2 min); a container restarted (warning); a container unhealthy (critical, 2 min); a container over 90% of its memory limit (warning, 5 min) |

Alerts on metrics a server never writes (replication on a lone server) never fire. One threshold covers every instance (every database, replica, container), so one restarting container is one incident naming it.

### Moving from another tool

The importer (ADR-0146) maps the equivalent sensors to these kinds where it is simple, PRTG first: its PostgreSQL, MySQL and Docker container sensors, the Zabbix templates "PostgreSQL by Zabbix agent 2", "MySQL by Zabbix agent", "Redis by Zabbix agent 2", "Docker by Zabbix agent 2" and their siblings, and Prometheus jobs scraping `postgres_exporter`, `mysqld_exporter` and `redis_exporter`. Passwords are never exported by these tools, so a database check is approximated: "enter the password on the check". A PRTG database sensor's own SQL query is not run: its connection and response time are, with the template's metrics.

### What comes next, each in its own task

- **SQL Server**: a `mssql` kind over TDS (the `tiberius` client, with rustls), from the agent on Windows or a probe: connections, batch requests, page life expectancy, buffer cache hit ratio, blocked processes, database sizes and log use, Always On replica state and lag, through `sys.dm_os_performance_counters`, `sys.dm_exec_requests` and `sys.dm_hadr_database_replica_states`, with `VIEW SERVER STATE` as its only right (`VIEW SERVER PERFORMANCE STATE` on 2022). Left out here to keep this change reviewable, and because its test server is a 1.5 GB image needing 2 GB of memory.
- **Java (JMX)**: JMX is Java RMI, which only a JVM speaks well. The plan is to read Jolokia's HTTP JSON bridge where it is installed (an agent JAR, as Zabbix's Java gateway is a separate process), from the agent or a probe: heap and non-heap use against their maximum, GC count and time per collector, threads, and Tomcat's or Kafka's MBeans as optional groups.
- **Kubernetes**: a cluster is a device checked from a probe through its API with a read-only service account token (sealed like a password) and its CA pinned: nodes ready and their pressure conditions, pods not running or restarting by namespace, deployments and stateful sets with fewer ready replicas than desired, and persistent volume claims pending. The inventory follows the VMware model (ADR-0156): one check, objects as instances, `_info` samples to describe them.
- **Cloud providers**: realistic first step, the state and basic metrics of virtual machines of AWS (EC2 `DescribeInstances`, `DescribeInstanceStatus` and CloudWatch `GetMetricData` for CPU, network and status checks), Azure (Resource Graph and Azure Monitor metrics) and Google Cloud (Compute Engine and Cloud Monitoring), each with read-only credentials sealed per account, run from the central server, and matched to monitored servers by instance ID, as virtual machines are (ADR-0156). Managed databases (RDS, Azure SQL, Cloud SQL) then reuse the database kinds above for what is inside, and the cloud check for what the provider sees.
- **Encrypted Redis** and an **agent-local Unix socket** for PostgreSQL and MySQL (peer authentication without a password).

## Alternatives rejected

- **Scripts or plugins shipped as templates.** A check plugin (ADR-0103) runs in a WebAssembly sandbox whose one capability is HTTP, so it cannot speak a database's protocol; scripts need a runtime and a client on every server, and parse text. Built-in kinds are checked by the server before saving, run the same on agents and probes, and are tested against real servers.
- **Keeping a connection open between collections.** It saves a login a minute, but a pooled connection hides a server that refuses new ones (the most common failure: `max_connections` reached) and holds a slot that people count against their limits.
- **Credentials per device, as SNMP and vCenter have** (ADR-0149, ADR-0156). A server often runs two PostgreSQL clusters, or PostgreSQL and Redis with different accounts; a password per check fits, and the column already exists.
- **Docker from a probe over the engine's TCP API.** It needs the API opened on the network with a client certificate equal to root on every Docker host; the agent is already there.
- **Alerts written by the web interface after the check**, as for services. They would not exist for API clients without each one copying the list; a template served by the server, applied in the check's transaction, is the same for everyone.
- **sqlx's RSA support for MySQL 8 without TLS.** Its `rsa` crate has an unfixed timing advisory; MySQL 8 enables TLS by default, which this path uses.

## Consequences

- Databases, Redis and containers are watched from inside with a few fields filled in, and alerts that make sense from the first minute.
- Each check opens one connection per collection, the check's own session counted among the connections it reports.
- An agent with `sensors.port_targets` must list `127.0.0.1` (or the database's address) for these checks to run there; the check's error says so.
- Watching Docker gives the agent's account root's power on that host, which a person grants knowingly; the agent's code only reads.
- The real tests (`crates/apps/tests/real.rs`, with `scripts/apps-test.sh`) need Docker and are not run by CI, which has no database containers; parsing, limits and failures are unit tested everywhere.
- SQL Server, JMX, Kubernetes and the cloud providers stay on the roadmap as their own tasks.
