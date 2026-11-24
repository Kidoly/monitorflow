---
title: "How to monitor PostgreSQL"
description: "Watch PostgreSQL's connections, long transactions, locks, cache and replication, and get alerted when it stops answering, with MonitorFlow's ready-made PostgreSQL check."
slug: monitor-postgresql
lang: en
translation: surveiller-postgresql
---

# How to monitor PostgreSQL

When PostgreSQL stops, every application on top of it stops too, and the first sign is often an error page someone else sees. MonitorFlow logs in to PostgreSQL with a read-only account, reads how it is doing, and tells you within a minute or two when it stops answering or starts to struggle.

## Why it matters

PostgreSQL rarely crashes on its own. It stops because of what happens around it: a disk full of WAL files, the kernel killing it for lack of memory, an update that restarted it with a broken configuration, a cluster that did not start after a reboot. The application logs connection errors nobody reads, and you hear about it from a user.

Before it stops, it often slows down: connections run out, a transaction left open blocks the cleanup, queries wait on locks, a replica falls hours behind. These show in PostgreSQL's own statistics long before users complain.

## What MonitorFlow checks

### The PostgreSQL check

The **"PostgreSQL"** entry of the catalogue (an **"Application check"**) connects to PostgreSQL every minute, logs in, and reads:

- whether it answers and the login works, and how long that takes;
- connections against `max_connections`, those running a query, and those idle inside an open transaction;
- slow queries (running longer than 30 seconds by default), the longest query and the oldest open transaction;
- locks waited for, deadlocks, the cache hit ratio and transactions per second;
- the size of each database;
- on a primary, its replicas and how far behind each one is; on a replica, whether it streams from its primary and how old the last replayed transaction is.

Metrics are named after the check: `monitorflow_postgresql_up`, `monitorflow_postgresql_connections_ratio`, and so on (with another check name, that name replaces `postgresql`).

With **"Add the recommended alerts"** on, these alerts come with it:

| Metric | Warning | Critical | For |
| --- | --- | --- | --- |
| `monitorflow_postgresql_up` (it answers and the login works) | None | Under 1 | 2 minutes |
| `monitorflow_postgresql_connections_ratio` | Over 0.8 (80%) | Over 0.95 | 5 minutes |
| `monitorflow_postgresql_longest_transaction_seconds` | Over 3600 (1 hour) | None | 5 minutes |
| `monitorflow_postgresql_locks_waiting` | Over 10 | None | 5 minutes |
| `monitorflow_postgresql_deadlocks` | Over 0 | None | At once |
| `monitorflow_postgresql_cache_hit_ratio` | Under 0.9 | None | 30 minutes |
| `monitorflow_postgresql_replica_lag_bytes` (per replica) | Over 1 GiB | None | 10 minutes |
| `monitorflow_postgresql_wal_receiver_up` (on a replica) | None | Under 1 | 5 minutes |
| `monitorflow_postgresql_replication_delay_seconds` (on a replica) | Over 300 (5 minutes) | Over 1800 (30 minutes) | 5 minutes |

Replication alerts stay quiet on a server without replication: the metric is not there.

### Service and port checks

Two simpler checks still have their place, for example on a server where you do not want a database account:

| Catalogue entry | What it checks | Alert by default |
| --- | --- | --- |
| **"PostgreSQL running"** | The systemd service `postgresql` is active. Metric `service_postgresql_active`: 1 active, 0 not | Critical when it stops, for 1 minute |
| **"PostgreSQL answers"** | Port 5432 accepts a TCP connection on 127.0.0.1. Metric `port_5432_up` | Critical when it fails, for 1 minute |

A service that is not installed counts as stopped, so a typo in the unit name raises an alert instead of passing silently.

**On Debian and Ubuntu, check the cluster unit.** There, `postgresql.service` is an umbrella unit that stays active even when the database has stopped. The server runs as `postgresql@<version>-<cluster>`, like `postgresql@16-main` (`systemctl list-units 'postgresql@*'` shows yours): enter that name instead. On RHEL and Rocky, `postgresql` is right (`postgresql-16` with the community packages).

A port check proves that PostgreSQL accepts TCP connections, not that it answers queries: the PostgreSQL check does. For anything else, wrap `pg_isready` or your own query in a script. See [How to monitor anything with your own script](monitor-with-your-own-script.md).

Disk space is checked on every server without setup: a warning at 85% full, critical at 95%.

## Create a read-only account

The check needs PostgreSQL 10 or later and an account that can log in. Give it the `pg_monitor` role and nothing else:

```sql
CREATE ROLE monitorflow LOGIN PASSWORD 'choose-a-long-password';
GRANT pg_monitor TO monitorflow;
```

`pg_monitor` lets it see every session, the replication and the database sizes, without reading or changing your data. Without it, the check still works, but PostgreSQL hides what other roles run, so the numbers cover the check's own sessions only.

The account must also be allowed in `pg_hba.conf` from where the check runs. On the server itself, the usual `host all all 127.0.0.1/32 scram-sha-256` line is enough. Reload PostgreSQL after a change (`systemctl reload` with your unit).

## Set it up

1. Open **Fleet**, click the database server, and stay on its **Sensors** tab.
2. Click **"Add a check"** and type `PostgreSQL` in **"Find a check"**.
3. Pick **"PostgreSQL"**, the **"Application check"**.
4. Keep **"Address"** at `127.0.0.1` and **"Port"** at `5432` if PostgreSQL runs on this server. Keep `monitorflow` in **"User name"** and enter the account's password in **"Password"**.
5. Leave **"Add the recommended alerts"** on, then click **"Add check"**.

The password is kept sealed on the central server, encrypted like its other secrets. It is never shown again, never written to a log, and the API only says whether one is set. If you later change **"Address"** or **"Port"**, enter the password again.

**"More options"** holds the rest:

- **"Database"**: the database to connect to, `postgres` by default.
- **"Encryption"**: **"When the server offers it"** (the default: encrypted if PostgreSQL offers it, certificate not verified), **"Required, certificate verified"**, or **"None"**.
- **"Slow after (seconds)"**: a query running longer than this counts as slow, 30 by default.

The check runs every 60 seconds by default. To watch every database server of a site at once, choose **"Every server of"** the site under **"Add to"**, or add it from **Settings › Sites**, **"Alerts and checks"**, if every server of the site runs PostgreSQL with the same account.

### Where the check runs

**"Checked from"** chooses who connects:

- **"This server's agent"** (the default): the agent on the database server connects to `127.0.0.1`. If its administrator set port targets (`sensors.port_targets` in `agent.toml`), the address and port must be among them.
- **"The central server"**, or **"Relay"** followed by the name of a relay of the site: to check it from outside, the way applications reach it. Enter the server's address in **"Address"**, make PostgreSQL listen on it (`listen_addresses` in `postgresql.conf`), and allow the central server's or relay's address in `pg_hba.conf`, like `host all monitorflow 10.0.0.5/32 scram-sha-256`. The central server never connects to link-local addresses (`169.254.x.x`, `fe80::`).

A device, a machine without the agent, has the same check in its catalogue: it always runs from the central server or a relay, against the device's address.

### Through the API

The public API adds the same check: `POST /api/v1/servers/{id}/sensor_configs` with the kind, its parameters, and `default_alerts` to add the recommended alerts. `GET /api/v1/check-templates` lists every metric and alert of each kind.

```json
{
  "kind": "postgresql",
  "interval_ms": 60000,
  "params": { "host": "127.0.0.1", "port": 5432, "user": "monitorflow", "password": "choose-a-long-password" },
  "default_alerts": true
}
```

### Service and port checks

1. In **"Find a check"**, type `PostgreSQL` and pick **"PostgreSQL running"**. In **"Service"**, keep `postgresql` or enter your cluster unit.
2. Leave **"Raise critical when it fails"** ticked. Under **"Add to"**, choose **"This server only"** or **"Every server of"** the site, then click **"Add check"**.
3. Do the same with **"PostgreSQL answers"**. If PostgreSQL listens only on another address, enter it in **"Address"**.

Add them to a whole site only if every server of the site runs PostgreSQL: on the others, the missing service raises an alert.

## Change the alert

On the **Sensors** tab, the recommended alerts are ordinary alerts, one line each under **"When to alert"**. Open a line's menu and choose **"Edit"**: **"Alert"** sets critical or warning, **"For at least"** the minutes the problem must last (0 alerts at once), and **"Apply to"** this server or every server of the site. A busy server may need a higher connection threshold; a reporting database may hold long transactions on purpose.

The service and port checks work the same way: open the check's menu and choose **"Edit"**. On a site's **"Alerts and checks"** page, the same settings are under **"When to alert"**, with **"Raise"** for the severity.

## Where the alert goes

Alerts show at once in **Monitoring › Incidents**. To be told, add a channel under **Settings › Notifications**: email, a webhook, or Slack, Teams or Discord, for everything, one customer or one site. A critical alert nobody acknowledges within 15 minutes also goes to the nearest administrators who set how they are reached on the **On call** page.

## When the alert fires

The message names the check, its value, and the server with its site:

```
Critical: postgresql_up is 0 on db-1 (paris)
Warning: postgresql_connections_ratio is 0.86 on db-1 (paris)
Critical: service_postgresql_active postgresql.service is 0 on db-1 (paris)
Critical: port_5432_up is 0 on db-1 (paris)
```

An email has that line as subject, after `[MonitorFlow]`, and a link to the alert. A message starting with `Resolved:` follows when PostgreSQL is back.

When `postgresql_up` is 0, its `reason` label says why, and its `error` label gives PostgreSQL's own words:

| `reason` | What to check |
| --- | --- |
| `connect` | Nothing answers at that address and port: PostgreSQL stopped, listens elsewhere (`listen_addresses`, `port`), or a firewall refuses the connection |
| `timeout` | No answer in time: a firewall that drops packets, or a server too loaded to answer |
| `tls` | Encryption failed: with **"Required, certificate verified"**, the certificate must be valid for the name in **"Address"** |
| `login` | Wrong user name or password, or no `pg_hba.conf` line allows the account from where the check runs |
| `permission` | The account lacks a right, or the database in **"Database"** does not exist |
| `protocol` | Something else answered on that port, or PostgreSQL said something unexpected |

Click **"Acknowledge"** on the alert's page, then check:

1. **Why it stopped:** `systemctl status` and `journalctl -u` with your unit, like `postgresql@16-main`. The server's **Logs** tab shows the journal's warnings and errors too.
2. **Disk space:** a full data or WAL volume stops PostgreSQL. Run `df -h`.
3. **Memory:** `journalctl -k | grep -i oom` shows whether the kernel killed it.
4. **The service runs but the port fails:** check `listen_addresses` and `port` in `postgresql.conf`, and the local firewall.
5. **Connections or long transactions:** `SELECT pid, usename, state, xact_start, query FROM pg_stat_activity ORDER BY xact_start;` shows who holds them.

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then open **Settings › Add servers** in the web interface and paste the command it shows on each server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md).
