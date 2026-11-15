---
title: "How to monitor PostgreSQL"
description: "Get alerted when PostgreSQL stops or stops accepting connections on a Linux server, with two checks from the MonitorFlow catalogue."
slug: monitor-postgresql
lang: en
translation: surveiller-postgresql
---

# How to monitor PostgreSQL

When PostgreSQL stops, every application on top of it stops too, and the first sign is often an error page someone else sees. MonitorFlow watches the PostgreSQL service and its port and tells you within a minute or two.

## Why it matters

PostgreSQL rarely crashes on its own. It stops because of what happens around it: a disk full of WAL files, the kernel killing it for lack of memory, an update that restarted it with a broken configuration, a cluster that did not start after a reboot. The application logs connection errors nobody reads, and you hear about it from a user.

## What MonitorFlow checks

| Catalogue entry | What it checks | Alert by default |
| --- | --- | --- |
| **"PostgreSQL running"** | The systemd service `postgresql` is active. Metric `service_postgresql_active`: 1 active, 0 not | Critical when it stops, for 1 minute |
| **"PostgreSQL answers"** | Port 5432 accepts a TCP connection on 127.0.0.1. Metric `port_5432_up` | Critical when it fails, for 1 minute |

A service that is not installed counts as stopped, so a typo in the unit name raises an alert instead of passing silently.

**On Debian and Ubuntu, check the cluster unit.** There, `postgresql.service` is an umbrella unit that stays active even when the database has stopped. The server runs as `postgresql@<version>-<cluster>`, like `postgresql@16-main` (`systemctl list-units 'postgresql@*'` shows yours): enter that name instead. On RHEL and Rocky, `postgresql` is right (`postgresql-16` with the community packages).

A port check proves that PostgreSQL accepts TCP connections, not that it answers queries. To go further, wrap `pg_isready` in a script: its exit codes (0 accepting, 1 rejecting, 2 no response) are the ones MonitorFlow reads as OK, warning and critical. See [How to monitor anything with your own script](monitor-with-your-own-script.md).

Disk space is checked on every server without setup: a warning at 85% full, critical at 95%.

## Set it up

1. Open **Fleet**, click the database server, and stay on its **Sensors** tab.
2. Click **"Add a check"** and type `PostgreSQL` in **"Find a check"**.
3. Pick **"PostgreSQL running"**. In **"Service"**, keep `postgresql` or enter your cluster unit.
4. Leave **"Raise critical when it fails"** ticked. Under **"Add to"**, choose **"This server only"** or **"Every server of"** the site, then click **"Add check"**.
5. Do the same with **"PostgreSQL answers"**. If PostgreSQL listens only on another address, enter it in **"Address"**.

The checks run every 60 seconds by default. For a whole site, open **Settings › Sites**, click **"Alerts and checks"** under the site and use **"Add a check"** there, but only if every server of the site runs PostgreSQL: on the others, the missing service raises an alert.

## Change the alert

On the **Sensors** tab, open the check's menu and choose **"Edit"**. **"Alert"** sets critical or warning, **"For at least"** the minutes the problem must last (0 alerts at once), and **"Apply to"** this server or every server of the site. On a site's **"Alerts and checks"** page, the same settings are under **"When to alert"**, with **"Raise"** for the severity.

## Where the alert goes

Alerts show at once in **Monitoring › Incidents**. To be told, add a channel under **Settings › Notifications**: email, a webhook, or Slack, Teams or Discord, for everything, one customer or one site. A critical alert nobody acknowledges within 15 minutes also goes to the nearest administrators who set how they are reached on the **On call** page.

## When the alert fires

The message names the check, its value, and the server with its site:

```
Critical: service_postgresql_active postgresql.service is 0 on db-1 (paris)
Critical: port_5432_up is 0 on db-1 (paris)
```

An email has that line as subject, after `[MonitorFlow]`, and a link to the alert. A message starting with `Resolved:` follows when PostgreSQL is back.

Click **"Acknowledge"** on the alert's page, then check:

1. **Why it stopped:** `systemctl status` and `journalctl -u` with your unit, like `postgresql@16-main`. The server's **Logs** tab shows the journal's warnings and errors too.
2. **Disk space:** a full data or WAL volume stops PostgreSQL. Run `df -h`.
3. **Memory:** `journalctl -k | grep -i oom` shows whether the kernel killed it.
4. **The service runs but the port fails:** check `listen_addresses` and `port` in `postgresql.conf`, and the local firewall.

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then open **Settings › Add servers** in the web interface and paste the command it shows on each server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md).
