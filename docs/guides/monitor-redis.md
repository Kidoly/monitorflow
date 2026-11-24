---
title: "How to monitor Redis"
description: "Watch Redis memory, evictions, clients, snapshots and replication, and get alerted when it stops answering, with MonitorFlow's ready-made Redis check."
slug: monitor-redis
lang: en
translation: surveiller-redis
---

# How to monitor Redis

Redis often sits quietly behind an application as a cache, a session store or a job queue. When it stops, logins fail, queues stall and pages slow down, and nothing says why. MonitorFlow asks Redis how it is doing every minute and tells you within a minute or two when it stops answering, runs out of memory or cannot save.

## Why it matters

Redis keeps its data in memory, which makes it fast and fragile. It stops when the kernel kills it for lack of memory, when a snapshot cannot be written to a full disk, or after an update or reboot that did not bring it back. Applications often treat a missing cache as a slow path rather than an error, so the outage can go unnoticed for hours.

Before it stops, it often warns: memory reaches `maxmemory` and keys get evicted, clients are refused, the last snapshot failed, a replica lost its primary. Redis reports all of it in `INFO`.

## What MonitorFlow checks

### The Redis check

The **"Redis"** entry of the catalogue (an **"Application check"**) works with Redis 5 or later, and servers that speak its protocol like Valkey and KeyDB. Every minute, it runs `INFO` and reads:

- whether it answers and the login works, and how long that takes;
- connected clients, clients blocked on a command, and connections refused for `maxclients`;
- memory used against `maxmemory` (when set), fragmentation, and evicted keys;
- the keyspace hit ratio and the number of keys;
- whether the last snapshot (RDB) and the last append-only file write (AOF) worked;
- connected replicas and their lag; on a replica, whether it is linked to its primary.

Metrics are named after the check: `monitorflow_redis_up`, `monitorflow_redis_memory_usage_ratio`, and so on (with another check name, that name replaces `redis`).

With **"Add the recommended alerts"** on, these alerts come with it:

| Metric | Warning | Critical | For |
| --- | --- | --- | --- |
| `monitorflow_redis_up` (it answers and the login works) | None | Under 1 | 2 minutes |
| `monitorflow_redis_memory_usage_ratio` (with `maxmemory`) | Over 0.8 (80%) | Over 0.95 | 5 minutes |
| `monitorflow_redis_evicted_keys` (since the last check) | Over 0 | None | 15 minutes |
| `monitorflow_redis_rejected_connections` (since the last check) | Over 0 | None | At once |
| `monitorflow_redis_last_save_ok` | None | Under 1 | At once |
| `monitorflow_redis_aof_write_ok` (with AOF) | None | Under 1 | At once |
| `monitorflow_redis_primary_link_up` (on a replica) | None | Under 1 | 2 minutes |

An alert whose metric is not there stays quiet: without `maxmemory`, AOF or replication, those lines never fire.

### Service and port checks

Two simpler checks still have their place:

| Catalogue entry | What it checks | Alert by default |
| --- | --- | --- |
| **"Redis running"** | The systemd service `redis-server` is active. Metric `service_redis_server_active`: 1 active, 0 not | Critical when it stops, for 1 minute |
| **"Redis answers"** | Port 6379 accepts a TCP connection on 127.0.0.1. Metric `port_6379_up` | Critical when it fails, for 1 minute |

The unit is `redis-server` on Debian and Ubuntu and `redis` on RHEL and Rocky, where the metric is `service_redis_active`. A service that is not installed counts as stopped, so a wrong name raises an alert.

A port check proves that Redis accepts TCP connections, not that it answers commands: the Redis check does. For anything else, see [How to monitor anything with your own script](monitor-with-your-own-script.md).

Memory is checked on every server without setup: a warning under 10% available, critical under 5%, for 5 minutes. On a Redis server, it is often the first sign of trouble.

**Encrypted Redis is not checked yet.** If Redis accepts only TLS connections, keep the service and port checks for now.

## Give it an account

The check needs only `INFO` and `PING`. On Redis 6 or later, create an ACL user that can run those two commands and nothing else:

```
ACL SETUSER monitorflow on >choose-a-long-password -@all +info +ping
```

Save it with `ACL SAVE` if you use an ACL file, or `CONFIG REWRITE` otherwise, so it survives a restart.

On older Redis, or without ACL users, leave **"User name"** empty and enter the `requirepass` password. A Redis without a password needs neither.

## Set it up

1. Open **Fleet**, click the server, and stay on its **Sensors** tab.
2. Click **"Add a check"** and type `Redis` in **"Find a check"**.
3. Pick **"Redis"**, the **"Application check"**.
4. Keep **"Address"** at `127.0.0.1` and **"Port"** at `6379` if Redis runs on this server. Enter `monitorflow` in **"User name"** and its password in **"Password"**, or only the password, or nothing.
5. Leave **"Add the recommended alerts"** on, then click **"Add check"**.

The password is kept sealed on the central server, encrypted like its other secrets. It is never shown again, never written to a log, and the API only says whether one is set. If you later change **"Address"** or **"Port"**, enter the password again.

The check runs every 60 seconds by default. To watch every Redis server of a site at once, choose **"Every server of"** the site under **"Add to"**, or add it from **Settings › Sites**, **"Alerts and checks"**, if every server of the site runs Redis with the same account.

### Where the check runs

**"Checked from"** chooses who connects:

- **"This server's agent"** (the default): the agent on the server connects to `127.0.0.1`. If its administrator set port targets (`sensors.port_targets` in `agent.toml`), the address and port must be among them.
- **"The central server"**, or **"Relay"** followed by the name of a relay of the site: to check it from outside, the way applications reach it. Enter the server's address in **"Address"**, make Redis listen on it (`bind` in `redis.conf`), and give it a password: with none, Redis's `protected-mode` refuses connections from other machines. Allow the central server's or relay's address in the firewall. The central server never connects to link-local addresses (`169.254.x.x`, `fe80::`).

A device, a machine without the agent, has the same check in its catalogue: it always runs from the central server or a relay, against the device's address.

### Through the API

The public API adds the same check: `POST /api/v1/servers/{id}/sensor_configs` with the kind, its parameters, and `default_alerts` to add the recommended alerts. `GET /api/v1/check-templates` lists every metric and alert of each kind.

```json
{
  "kind": "redis",
  "interval_ms": 60000,
  "params": { "host": "127.0.0.1", "port": 6379, "user": "monitorflow", "password": "choose-a-long-password" },
  "default_alerts": true
}
```

### Service and port checks

1. In **"Find a check"**, type `Redis` and pick **"Redis running"**. In **"Service"**, keep `redis-server` or enter `redis`.
2. Leave **"Raise critical when it fails"** ticked. Under **"Add to"**, choose **"This server only"** or **"Every server of"** the site, then click **"Add check"**.
3. Do the same with **"Redis answers"**. If Redis listens only on another address (`bind` in `redis.conf`), enter it in **"Address"**.

Add them to a whole site only if every server of the site runs Redis: on the others, the missing service raises an alert.

## Change the alert

On the **Sensors** tab, the recommended alerts are ordinary alerts, one line each under **"When to alert"**. Open a line's menu and choose **"Edit"**: **"Alert"** sets critical or warning (a cache that only slows things down may deserve a warning), **"For at least"** the minutes the problem must last (0 alerts at once), and **"Apply to"** this server or every server of the site. A pure cache that evicts keys on purpose can do without the eviction alert: open its menu and turn it off.

The service and port checks work the same way: open the check's menu and choose **"Edit"**. On a site's **"Alerts and checks"** page, the same settings are under **"When to alert"**, with **"Raise"** for the severity.

## Where the alert goes

Alerts show at once in **Monitoring › Incidents**. To be told, add a channel under **Settings › Notifications**: email, a webhook, or Slack, Teams or Discord, for everything, one customer or one site. A critical alert nobody acknowledges within 15 minutes also goes to the nearest administrators who set how they are reached on the **On call** page.

## When the alert fires

The message names the check, its value, and the server with its site:

```
Critical: redis_up is 0 on cache-1 (paris)
Critical: redis_last_save_ok is 0 on cache-1 (paris)
Critical: service_redis_server_active redis-server.service is 0 on cache-1 (paris)
Critical: port_6379_up is 0 on cache-1 (paris)
```

An email has that line as subject, after `[MonitorFlow]`, and a link to the alert. A message starting with `Resolved:` follows when Redis is back.

When `redis_up` is 0, its `reason` label says why, and its `error` label gives Redis's own words:

| `reason` | What to check |
| --- | --- |
| `connect` | Nothing answers at that address and port: Redis stopped, listens elsewhere (`bind`, `port`), or a firewall refuses the connection |
| `timeout` | No answer in time: a firewall that drops packets, or a Redis too busy to answer |
| `login` | Wrong user name or password, or Redis asks for a password and none is set (`NOAUTH`) |
| `permission` | The ACL user cannot run `INFO` or `PING` (`NOPERM`) |
| `protocol` | Something else answered on that port, or Redis returned an error, like `protected-mode` refusing a connection from another machine |

Click **"Acknowledge"** on the alert's page, then check:

1. **Why it stopped:** `systemctl status redis-server` and `journalctl -u redis-server -n 50`. The server's **Logs** tab shows the journal's warnings and errors too.
2. **Memory:** `journalctl -k | grep -i oom` shows whether the kernel killed it. Check `maxmemory` and `maxmemory-policy` in `redis.conf`.
3. **Disk space:** `df -h`. A failed snapshot on a full disk can make Redis refuse writes; `redis-cli INFO persistence` shows the last error.
4. **The service runs but the port fails:** check `bind` and `port` in `redis.conf`, and the local firewall.

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then open **Settings › Add servers** in the web interface and paste the command it shows on each server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md).
