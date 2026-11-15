---
title: "How to monitor MySQL and MariaDB"
description: "Get alerted when MySQL or MariaDB stops or stops accepting connections, with the service and port checks of the MonitorFlow catalogue."
slug: monitor-mysql-mariadb
lang: en
translation: surveiller-mysql-mariadb
---

# How to monitor MySQL and MariaDB

A website, a CRM or a ticketing tool can run for years on one MySQL or MariaDB server nobody looks at, until it stops. MonitorFlow watches the database service and its port and tells you within a minute or two, before your users do.

## Why it matters

MySQL and MariaDB stop for ordinary reasons: a disk full of binary logs, the kernel killing the server for lack of memory, an upgrade that left a configuration it refuses, a crash recovery that takes long after a reboot. The application shows "Error establishing a database connection" or a blank page, and the first report comes from a customer.

## What MonitorFlow checks

| Catalogue entry | What it checks | Alert by default |
| --- | --- | --- |
| **"MySQL or MariaDB running"** (Linux) | The systemd service `mariadb` is active. Metric `service_mariadb_active`: 1 active, 0 not | Critical when it stops, for 1 minute |
| **"MySQL or MariaDB answers"** (Linux and Windows) | Port 3306 accepts a TCP connection on 127.0.0.1. Metric `port_3306_up` | Critical when it fails, for 1 minute |

**Pick the right unit name:** `mariadb` for MariaDB, `mysql` for MySQL on Debian and Ubuntu, `mysqld` for MySQL on RHEL and Rocky. The metric follows it: with `mysql`, it is `service_mysql_active`. A service that is not installed counts as stopped, so a wrong name raises an alert rather than hiding the problem.

A port check proves that the server accepts TCP connections, not that it answers queries. To go further, wrap `mysqladmin ping` in a script: it exits with 0 when the server is running, even without valid credentials, and 1 when it is not. See [How to monitor anything with your own script](monitor-with-your-own-script.md).

Disk space is checked on every server without setup: a warning at 85% full, critical at 95%, before binary logs fill the disk.

## Set it up

1. Open **Fleet**, click the database server, and stay on its **Sensors** tab.
2. Click **"Add a check"** and type `MariaDB` or `MySQL` in **"Find a check"**.
3. Pick **"MySQL or MariaDB running"**. In **"Service"**, keep `mariadb` or enter `mysql` or `mysqld`.
4. Leave **"Raise critical when it fails"** ticked. Under **"Add to"**, choose **"This server only"** or **"Every server of"** the site, then click **"Add check"**.
5. Do the same with **"MySQL or MariaDB answers"**. If the server listens only on another address (`bind-address`), enter it in **"Address"**.

The checks run every 60 seconds by default. For a whole site, open **Settings › Sites**, click **"Alerts and checks"** under the site and use **"Add a check"** there, but only if every server of the site runs the database: on the others, the missing service raises an alert.

## Change the alert

On the **Sensors** tab, open the check's menu and choose **"Edit"**. **"Alert"** sets critical or warning, **"For at least"** the minutes the problem must last (0 alerts at once; longer suits a database slow to restart after updates), and **"Apply to"** this server or every server of the site. On a site's **"Alerts and checks"** page, the same settings are under **"When to alert"**, with **"Raise"** for the severity.

## Where the alert goes

Alerts show at once in **Monitoring › Incidents**. To be told, add a channel under **Settings › Notifications**: email, a webhook, or Slack, Teams or Discord, for everything, one customer or one site. A critical alert nobody acknowledges within 15 minutes also goes to the nearest administrators who set how they are reached on the **On call** page.

## When the alert fires

The message names the check, its value, and the server with its site:

```
Critical: service_mariadb_active mariadb.service is 0 on db-2 (lyon)
Critical: port_3306_up is 0 on db-2 (lyon)
```

An email has that line as subject, after `[MonitorFlow]`, and a link to the alert. A message starting with `Resolved:` follows when the database is back.

Click **"Acknowledge"** on the alert's page, then check:

1. **Why it stopped:** `systemctl status mariadb` and `journalctl -u mariadb -n 50` (or your unit). The server's **Logs** tab shows the journal's warnings and errors too.
2. **Disk space:** `df -h`. Binary logs and large temporary tables fill volumes fast.
3. **Memory:** `journalctl -k | grep -i oom` shows whether the kernel killed `mysqld` or `mariadbd`.
4. **The service runs but the port fails:** check `bind-address`, `port` and `skip-networking`, and the local firewall.

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then open **Settings › Add servers** in the web interface and paste the command it shows on each server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md).
