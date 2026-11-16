---
title: "How to monitor Redis"
description: "Get alerted when Redis stops or stops accepting connections on a Linux server, with the service and port checks of MonitorFlow."
slug: monitor-redis
lang: en
translation: surveiller-redis
---

# How to monitor Redis

Redis often sits quietly behind an application as a cache, a session store or a job queue. When it stops, logins fail, queues stall and pages slow down, and nothing says why. MonitorFlow watches the Redis service and its port and tells you within a minute or two.

## Why it matters

Redis keeps its data in memory, which makes it fast and fragile. It stops when the kernel kills it for lack of memory, when a snapshot cannot be written to a full disk, or after an update or reboot that did not bring it back. Applications often treat a missing cache as a slow path rather than an error, so the outage can go unnoticed for hours.

## What MonitorFlow checks

| Catalogue entry | What it checks | Alert by default |
| --- | --- | --- |
| **"Redis running"** | The systemd service `redis-server` is active. Metric `service_redis_server_active`: 1 active, 0 not | Critical when it stops, for 1 minute |
| **"Redis answers"** | Port 6379 accepts a TCP connection on 127.0.0.1. Metric `port_6379_up` | Critical when it fails, for 1 minute |

The unit is `redis-server` on Debian and Ubuntu and `redis` on RHEL and Rocky, where the metric is `service_redis_active`. A service that is not installed counts as stopped, so a wrong name raises an alert.

A port check proves that Redis accepts TCP connections, not that it answers commands. A short script can go further:

```sh
#!/bin/sh
if [ "$(redis-cli -h 127.0.0.1 ping 2>/dev/null)" = "PONG" ]; then
  echo "OK: Redis answers"; exit 0
fi
echo "CRITICAL: Redis does not answer PING"; exit 2
```

If Redis requires a password, `ping` gets an authentication error and the script reports a problem. See [How to monitor anything with your own script](monitor-with-your-own-script.md) for where to install it.

Memory is checked on every server without setup: a warning under 10% available, critical under 5%, for 5 minutes. On a Redis server, it is often the first sign of trouble.

## Set it up

1. Open **Fleet**, click the server, and stay on its **Sensors** tab.
2. Click **"Add a check"** and type `Redis` in **"Find a check"**.
3. Pick **"Redis running"**. In **"Service"**, keep `redis-server` or enter `redis`.
4. Leave **"Raise critical when it fails"** ticked. Under **"Add to"**, choose **"This server only"** or **"Every server of"** the site, then click **"Add check"**.
5. Do the same with **"Redis answers"**. If Redis listens only on another address (`bind` in `redis.conf`), enter it in **"Address"**.

The checks run every 60 seconds by default. For a whole site, open **Settings › Sites**, click **"Alerts and checks"** under the site and use **"Add a check"** there, but only if every server of the site runs Redis: on the others, the missing service raises an alert.

## Change the alert

On the **Sensors** tab, open the check's menu and choose **"Edit"**. **"Alert"** sets critical or warning (a cache that only slows things down may deserve a warning), **"For at least"** the minutes the problem must last (0 alerts at once), and **"Apply to"** this server or every server of the site. On a site's **"Alerts and checks"** page, the same settings are under **"When to alert"**, with **"Raise"** for the severity.

## Where the alert goes

Alerts show at once in **Monitoring › Incidents**. To be told, add a channel under **Settings › Notifications**: email, a webhook, or Slack, Teams or Discord, for everything, one customer or one site. A critical alert nobody acknowledges within 15 minutes also goes to the nearest administrators who set how they are reached on the **On call** page.

## When the alert fires

The message names the check, its value, and the server with its site:

```
Critical: service_redis_server_active redis-server.service is 0 on cache-1 (paris)
Critical: port_6379_up is 0 on cache-1 (paris)
```

An email has that line as subject, after `[MonitorFlow]`, and a link to the alert. A message starting with `Resolved:` follows when Redis is back.

Click **"Acknowledge"** on the alert's page, then check:

1. **Why it stopped:** `systemctl status redis-server` and `journalctl -u redis-server -n 50`. The server's **Logs** tab shows the journal's warnings and errors too.
2. **Memory:** `journalctl -k | grep -i oom` shows whether the kernel killed it. Check `maxmemory` in `redis.conf`.
3. **Disk space:** `df -h`. A failed snapshot on a full disk can make Redis refuse writes.
4. **The service runs but the port fails:** check `bind` and `port` in `redis.conf`, and the local firewall.

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then open **Settings › Add servers** in the web interface and paste the command it shows on each server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md).
