---
title: "How to monitor MySQL and MariaDB"
description: "Watch MySQL and MariaDB connections, slow queries, the InnoDB buffer pool and replication, and get alerted when the database stops answering, with MonitorFlow's ready-made check."
slug: monitor-mysql-mariadb
lang: en
translation: surveiller-mysql-mariadb
---

# How to monitor MySQL and MariaDB

A website, a CRM or a ticketing tool can run for years on one MySQL or MariaDB server nobody looks at, until it stops. MonitorFlow logs in with a read-only account, reads the server's status, and tells you within a minute or two when it stops answering or starts to struggle, before your users do.

## Why it matters

MySQL and MariaDB stop for ordinary reasons: a disk full of binary logs, the kernel killing the server for lack of memory, an upgrade that left a configuration it refuses, a crash recovery that takes long after a reboot. The application shows "Error establishing a database connection" or a blank page, and the first report comes from a customer.

Trouble often starts earlier: connections reach `max_connections`, slow queries pile up, the buffer pool is too small for the data, a replica stops copying. The server's own status shows these first.

## What MonitorFlow checks

### The MySQL or MariaDB check

The **"MySQL or MariaDB"** entry of the catalogue (an **"Application check"**) works with MySQL 5.7 or later and MariaDB 10.3 or later. Every minute, it logs in and reads:

- whether it answers and the login works, and how long that takes;
- connections against `max_connections`, and queries running now;
- queries per second, slow queries (over `long_query_time`) and failed connection attempts;
- the InnoDB buffer pool hit ratio and row locks waited for;
- the size of each schema the account can see, every 10 minutes;
- on a replica, whether its I/O and SQL threads run, and how far behind it is.

Metrics are named after the check: `monitorflow_mysql_up`, `monitorflow_mysql_connections_ratio`, and so on (with another check name, that name replaces `mysql`).

With **"Add the recommended alerts"** on, these alerts come with it:

| Metric | Warning | Critical | For |
| --- | --- | --- | --- |
| `monitorflow_mysql_up` (it answers and the login works) | None | Under 1 | 2 minutes |
| `monitorflow_mysql_connections_ratio` | Over 0.8 (80%) | Over 0.95 | 5 minutes |
| `monitorflow_mysql_slow_queries` (since the last check) | Over 10 | None | 5 minutes |
| `monitorflow_mysql_buffer_pool_hit_ratio` | Under 0.95 | None | 30 minutes |
| `monitorflow_mysql_lock_waits` | Over 10 | None | 5 minutes |
| `monitorflow_mysql_replica_io_running` (on a replica) | None | Under 1 | 2 minutes |
| `monitorflow_mysql_replica_sql_running` (on a replica) | None | Under 1 | 2 minutes |
| `monitorflow_mysql_replication_delay_seconds` (on a replica) | Over 300 (5 minutes) | Over 1800 (30 minutes) | 5 minutes |

Replication alerts stay quiet on a server that is not a replica: the metric is not there.

### Service and port checks

Two simpler checks still have their place, for example on a server where you do not want a database account:

| Catalogue entry | What it checks | Alert by default |
| --- | --- | --- |
| **"MySQL or MariaDB running"** (Linux) | The systemd service `mariadb` is active. Metric `service_mariadb_active`: 1 active, 0 not | Critical when it stops, for 1 minute |
| **"MySQL or MariaDB answers"** (Linux and Windows) | Port 3306 accepts a TCP connection on 127.0.0.1. Metric `port_3306_up` | Critical when it fails, for 1 minute |

**Pick the right unit name:** `mariadb` for MariaDB, `mysql` for MySQL on Debian and Ubuntu, `mysqld` for MySQL on RHEL and Rocky. The metric follows it: with `mysql`, it is `service_mysql_active`. A service that is not installed counts as stopped, so a wrong name raises an alert rather than hiding the problem.

A port check proves that the server accepts TCP connections, not that it answers queries: the MySQL or MariaDB check does. For anything else, wrap `mysqladmin ping` or your own query in a script. See [How to monitor anything with your own script](monitor-with-your-own-script.md).

Disk space is checked on every server without setup: a warning at 85% full, critical at 95%, before binary logs fill the disk.

## Create a read-only account

`PROCESS` and `REPLICATION CLIENT` are all the check needs. They let it read the status and the replication, not your data. When the agent runs the check on the database server, create the account for `localhost`:

```sql
CREATE USER 'monitorflow'@'localhost' IDENTIFIED BY 'choose-a-long-password';
GRANT PROCESS, REPLICATION CLIENT ON *.* TO 'monitorflow'@'localhost';
```

The check connects over TCP to `127.0.0.1`: if the server runs with `skip-name-resolve`, create the account for `'127.0.0.1'` instead. When the central server or a relay runs it, use the network they connect from, like `'monitorflow'@'10.0.0.%'`.

Schema sizes cover the schemas the account can see. To include a schema, you may add `GRANT SELECT ON shop.* TO 'monitorflow'@'localhost';`. This is optional, and it lets the account read that schema's data.

**MySQL 8 and encryption:** keep **"Encryption"** at its default. With **"None"**, MySQL 8's usual sign-in needs an RSA key exchange the check does not support, so the check fails. MySQL 8 offers TLS out of the box, so the default works.

## Set it up

1. Open **Fleet**, click the database server, and stay on its **Sensors** tab.
2. Click **"Add a check"** and type `MariaDB` or `MySQL` in **"Find a check"**.
3. Pick **"MySQL or MariaDB"**, the **"Application check"**.
4. Keep **"Address"** at `127.0.0.1` and **"Port"** at `3306` if the database runs on this server. Keep `monitorflow` in **"User name"** and enter the account's password in **"Password"**.
5. Leave **"Add the recommended alerts"** on, then click **"Add check"**.

The password is kept sealed on the central server, encrypted like its other secrets. It is never shown again, never written to a log, and the API only says whether one is set. If you later change **"Address"** or **"Port"**, enter the password again.

Under **"More options"**, **"Encryption"** is **"When the server offers it"** (the default: encrypted if the server offers it, certificate not verified), **"Required, certificate verified"**, or **"None"**.

The check runs every 60 seconds by default. To watch every database server of a site at once, choose **"Every server of"** the site under **"Add to"**, or add it from **Settings › Sites**, **"Alerts and checks"**, if every server of the site runs the database with the same account.

### Where the check runs

**"Checked from"** chooses who connects:

- **"This server's agent"** (the default): the agent on the database server connects to `127.0.0.1`. If its administrator set port targets (`sensors.port_targets` in `agent.toml`), the address and port must be among them.
- **"The central server"**, or **"Relay"** followed by the name of a relay of the site: to check it from outside, the way applications reach it. Enter the server's address in **"Address"**, make the database listen on it (`bind-address`, and no `skip-networking`), and create the account for the central server's or relay's address. The central server never connects to link-local addresses (`169.254.x.x`, `fe80::`).

A device, a machine without the agent, has the same check in its catalogue: it always runs from the central server or a relay, against the device's address.

### Through the API

The public API adds the same check: `POST /api/v1/servers/{id}/sensor_configs` with the kind, its parameters, and `default_alerts` to add the recommended alerts. `GET /api/v1/check-templates` lists every metric and alert of each kind.

```json
{
  "kind": "mysql",
  "interval_ms": 60000,
  "params": { "host": "127.0.0.1", "port": 3306, "user": "monitorflow", "password": "choose-a-long-password" },
  "default_alerts": true
}
```

### Service and port checks

1. In **"Find a check"**, type `MariaDB` or `MySQL` and pick **"MySQL or MariaDB running"**. In **"Service"**, keep `mariadb` or enter `mysql` or `mysqld`.
2. Leave **"Raise critical when it fails"** ticked. Under **"Add to"**, choose **"This server only"** or **"Every server of"** the site, then click **"Add check"**.
3. Do the same with **"MySQL or MariaDB answers"**. If the server listens only on another address (`bind-address`), enter it in **"Address"**.

Add them to a whole site only if every server of the site runs the database: on the others, the missing service raises an alert.

## Change the alert

On the **Sensors** tab, the recommended alerts are ordinary alerts, one line each under **"When to alert"**. Open a line's menu and choose **"Edit"**: **"Alert"** sets critical or warning, **"For at least"** the minutes the problem must last (0 alerts at once; longer suits a database slow to restart after updates), and **"Apply to"** this server or every server of the site.

The service and port checks work the same way: open the check's menu and choose **"Edit"**. On a site's **"Alerts and checks"** page, the same settings are under **"When to alert"**, with **"Raise"** for the severity.

## Where the alert goes

Alerts show at once in **Monitoring › Incidents**. To be told, add a channel under **Settings › Notifications**: email, a webhook, or Slack, Teams or Discord, for everything, one customer or one site. A critical alert nobody acknowledges within 15 minutes also goes to the nearest administrators who set how they are reached on the **On call** page.

## When the alert fires

The message names the check, its value, and the server with its site:

```
Critical: mysql_up is 0 on db-2 (lyon)
Warning: mysql_slow_queries is 24 on db-2 (lyon)
Critical: service_mariadb_active mariadb.service is 0 on db-2 (lyon)
Critical: port_3306_up is 0 on db-2 (lyon)
```

An email has that line as subject, after `[MonitorFlow]`, and a link to the alert. A message starting with `Resolved:` follows when the database is back.

When `mysql_up` is 0, its `reason` label says why, and its `error` label gives the server's own words:

| `reason` | What to check |
| --- | --- |
| `connect` | Nothing answers at that address and port: the server stopped, listens elsewhere (`bind-address`, `port`, `skip-networking`), or a firewall refuses the connection |
| `timeout` | No answer in time: a firewall that drops packets, or a server too loaded to answer |
| `tls` | Encryption failed: with **"Required, certificate verified"**, the certificate must be valid for the name in **"Address"** |
| `login` | Wrong user name or password, or the account's host part does not match where the check connects from |
| `permission` | The account lacks `PROCESS` or `REPLICATION CLIENT` |
| `protocol` | Something else answered on that port, or the server asked for a sign-in the check does not support (see MySQL 8 and encryption above) |

Click **"Acknowledge"** on the alert's page, then check:

1. **Why it stopped:** `systemctl status mariadb` and `journalctl -u mariadb -n 50` (or your unit). The server's **Logs** tab shows the journal's warnings and errors too.
2. **Disk space:** `df -h`. Binary logs and large temporary tables fill volumes fast.
3. **Memory:** `journalctl -k | grep -i oom` shows whether the kernel killed `mysqld` or `mariadbd`.
4. **The service runs but the port fails:** check `bind-address`, `port` and `skip-networking`, and the local firewall.
5. **Connections or slow queries:** `SHOW FULL PROCESSLIST;` shows who is connected and what runs.

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then open **Settings › Add servers** in the web interface and paste the command it shows on each server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md).
