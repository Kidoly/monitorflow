---
title: "How to monitor MongoDB"
description: "Get alerted when MongoDB stops or stops accepting connections on a Linux server, with the mongod service and port 27017 checks of MonitorFlow."
slug: monitor-mongodb
lang: en
translation: surveiller-mongodb
---

# How to monitor MongoDB

When `mongod` stops, the applications that depend on it fail with timeouts and connection errors, often far from the database itself. MonitorFlow watches the MongoDB service and its port and tells you within a minute or two.

## Why it matters

MongoDB stops for the same reasons as any database: a data volume that fills up, the kernel killing it for lack of memory, an upgrade that changed the configuration format, a reboot with the service not enabled. Its memory use grows with the data it caches, so a server that ran fine for months can tip over as the data grows.

## What MonitorFlow checks

| Catalogue entry | What it checks | Alert by default |
| --- | --- | --- |
| **"MongoDB running"** | The systemd service `mongod` is active. Metric `service_mongod_active`: 1 active, 0 not | Critical when it stops, for 1 minute |
| **"MongoDB answers"** | Port 27017 accepts a TCP connection on 127.0.0.1. Metric `port_27017_up` | Critical when it fails, for 1 minute |

`mongod` is the unit name of the official packages. A service that is not installed counts as stopped, so a wrong name raises an alert. With several instances on one server, add a service check and a port check per instance, with their own units and ports.

A port check proves that MongoDB accepts TCP connections, not that it answers queries or that a replica set has a primary. For that, a script of your own can query the server and report OK, warning or critical with its exit code. See [How to monitor anything with your own script](monitor-with-your-own-script.md).

Memory and disk space are checked on every server without setup: a disk warning at 85% full and critical at 95%, a memory warning under 10% available and critical under 5%.

## Set it up

1. Open **Fleet**, click the server, and stay on its **Sensors** tab.
2. Click **"Add a check"** and type `MongoDB` in **"Find a check"**.
3. Pick **"MongoDB running"**. Keep `mongod` in **"Service"** unless your unit has another name.
4. Leave **"Raise critical when it fails"** ticked. Under **"Add to"**, choose **"This server only"** or **"Every server of"** the site, then click **"Add check"**.
5. Do the same with **"MongoDB answers"**. If MongoDB listens only on another address (`net.bindIp` in `/etc/mongod.conf`), enter it in **"Address"**.

The checks run every 60 seconds by default. For a whole site, open **Settings › Sites**, click **"Alerts and checks"** under the site and use **"Add a check"** there, but only if every server of the site runs MongoDB: on the others, the missing service raises an alert.

## Change the alert

On the **Sensors** tab, open the check's menu and choose **"Edit"**. **"Alert"** sets critical or warning (a replica set member others can replace may deserve a warning), **"For at least"** the minutes the problem must last (0 alerts at once), and **"Apply to"** this server or every server of the site. On a site's **"Alerts and checks"** page, the same settings are under **"When to alert"**, with **"Raise"** for the severity.

## Where the alert goes

Alerts show at once in **Monitoring › Incidents**. To be told, add a channel under **Settings › Notifications**: email, a webhook, or Slack, Teams or Discord, for everything, one customer or one site. A critical alert nobody acknowledges within 15 minutes also goes to the nearest administrators who set how they are reached on the **On call** page.

## When the alert fires

The message names the check, its value, and the server with its site:

```
Critical: service_mongod_active mongod.service is 0 on mongo-1 (paris)
Critical: port_27017_up is 0 on mongo-1 (paris)
```

An email has that line as subject, after `[MonitorFlow]`, and a link to the alert. A message starting with `Resolved:` follows when MongoDB is back.

Click **"Acknowledge"** on the alert's page, then check:

1. **Why it stopped:** `systemctl status mongod` and `journalctl -u mongod -n 50`, then MongoDB's own log, `/var/log/mongodb/mongod.log` with the official packages.
2. **Disk space:** `df -h` on the volume of `storage.dbPath`.
3. **Memory:** `journalctl -k | grep -i oom` shows whether the kernel killed it.
4. **The service runs but the port fails:** check `net.bindIp` and `net.port` in `/etc/mongod.conf`, and the local firewall.

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then open **Settings › Add servers** in the web interface and paste the command it shows on each server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md).
