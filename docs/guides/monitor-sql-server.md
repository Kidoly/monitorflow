---
title: "How to monitor SQL Server"
description: "Watch SQL Server's connections, batch requests, page life expectancy, blocking, database and log sizes and Always On replicas, and get alerted when it stops answering, with MonitorFlow's ready-made check."
slug: monitor-sql-server
lang: en
translation: surveiller-sql-server
---

# How to monitor SQL Server

An ERP, an accounting package or a business application often depends on one SQL Server instance on one Windows server. When it stops or slows down, everyone who uses the application is stuck. MonitorFlow signs in with a login that may only read the server's state, and tells you within a minute or two when SQL Server stops answering or starts to struggle.

## Why it matters

SQL Server stops after a Windows update that restarted the server and left the service stopped, when a log or data drive fills up, when the service account's password changed, or after a crash. Users call to say the application "does not open", and time goes into finding which server is at fault.

Trouble often starts earlier: a session holds a lock and others queue behind it, the buffer pool is too small and pages leave memory within seconds, a transaction log stops growing because its drive is full, an Always On secondary falls behind. SQL Server's own counters show these first.

## What MonitorFlow checks

### The SQL Server check

The **"SQL Server"** entry of the catalogue (an **"Application check"**) works with SQL Server 2012 or later, on Windows or Linux. Every minute, it signs in and reads:

- whether it answers and the sign-in works, and how long that takes;
- user connections against the `user connections` setting (32,767 when unlimited);
- batch requests per second, the page life expectancy and the buffer cache hit ratio;
- requests blocked by another one's lock, the longest wait among them, and the longest running request;
- lock waits per second and deadlocks;
- the size of each database's data files and transaction log, and how full each log is;
- with Always On, each availability replica's connection and health, and for each database on a secondary, the log not yet sent, the log not yet replayed and how far behind it is (SQL Server 2016 or later).

Metrics are named after the check: `monitorflow_mssql_up`, `monitorflow_mssql_page_life_expectancy_seconds`, and so on (with another check name, that name replaces `mssql`). A database is the instance of its metrics (`shop`), a replica is `group/server` and a database on a replica `group/server/database`.

With **"Add the recommended alerts"** on, these alerts come with it:

| Metric | Warning | Critical | For |
| --- | --- | --- | --- |
| `monitorflow_mssql_up` (it answers and the sign-in works) | None | Under 1 | 2 minutes |
| `monitorflow_mssql_connections_ratio` | Over 0.8 (80%) | Over 0.95 | 5 minutes |
| `monitorflow_mssql_page_life_expectancy_seconds` | Under 300 | None | 15 minutes |
| `monitorflow_mssql_buffer_cache_hit_ratio` | Under 0.95 | None | 30 minutes |
| `monitorflow_mssql_processes_blocked` | Over 0 | None | 5 minutes |
| `monitorflow_mssql_deadlocks` (since the last check) | Over 0 | None | At once |
| `monitorflow_mssql_log_used_ratio` (per database) | Over 0.8 | Over 0.95 | 5 minutes |
| `monitorflow_mssql_ag_replica_connected` (per replica) | None | Under 1 | 2 minutes |
| `monitorflow_mssql_ag_replica_healthy` (per replica) | None | Under 1 | 5 minutes |
| `monitorflow_mssql_ag_log_send_queue_bytes` (per database on a secondary) | Over 1 GiB | None | 10 minutes |
| `monitorflow_mssql_ag_secondary_lag_seconds` (per database on a secondary) | Over 300 (5 minutes) | Over 1800 (30 minutes) | 5 minutes |

Always On alerts stay quiet on a server without availability groups: the metrics are not there. A page life expectancy of 300 seconds is the usual floor for a small server; on one with a lot of memory, raise it (a common rule is 300 per 4 GB of buffer pool).

### Service and port checks

Two simpler checks still have their place, for example on a server where you do not want a SQL Server login:

| Catalogue entry | What it checks | Alert by default |
| --- | --- | --- |
| **"SQL Server running"** | The Windows service `MSSQLSERVER`, the default instance, is running. Metric `service_mssqlserver_active`: 1 running, 0 not | Critical when it stops, for 1 minute |
| **"SQL Server answers"** | Port 1433 accepts a TCP connection on 127.0.0.1. Metric `port_1433_up` | Critical when it fails, for 1 minute |

**Named instances** run as `MSSQL$` followed by the instance name, like `MSSQL$SQLEXPRESS`: enter the service name, not its display name. A service that is not installed counts as stopped, so a wrong name raises an alert. To watch SQL Server Agent too, add **"A Windows service is running"** with `SQLSERVERAGENT` (or `SQLAgent$` and the instance name). On Linux, use **"A service is running"** with `mssql-server`.

SQL Server writes its errors to the Windows Application event log: **"Errors in the Application event log"** raises a warning on each new error or critical event there. Disk space of each drive is checked without setup.

## Create a login that only reads the server's state

The check needs a SQL Server login with one right: `VIEW SERVER STATE`, or `VIEW SERVER PERFORMANCE STATE` from SQL Server 2022. It lets the login read the server's counters and sessions, not your data, and it needs no user in any database. In SQL Server Management Studio, or with `sqlcmd`, as an administrator:

```sql
CREATE LOGIN monitorflow WITH PASSWORD = 'choose-a-long-password';
GRANT VIEW SERVER STATE TO monitorflow;
-- SQL Server 2022 and later: the narrower right is enough.
-- GRANT VIEW SERVER PERFORMANCE STATE TO monitorflow;
```

On an availability group, create the login on every replica, as logins are not copied with the databases.

**SQL Server authentication must be on.** Many servers accept Windows sign-ins only. In Management Studio, open the server's **Properties › Security**, choose **"SQL Server and Windows Authentication mode"**, then restart the service. Signing in with a Windows account is not supported by the check yet.

**TCP must be on, at a fixed port.** SQL Server Express ships with TCP/IP turned off, and a named instance picks its port at start: in SQL Server Configuration Manager, turn on TCP/IP and give the instance a fixed port, then enter that port in **"Port"**. The check does not ask SQL Server Browser for the port of a named instance.

## Set it up

1. Open **Fleet**, click the SQL Server machine, and stay on its **Sensors** tab.
2. Click **"Add a check"** and type `SQL Server` in **"Find a check"**.
3. Pick **"SQL Server"**, the **"Application check"**.
4. Keep **"Address"** at `127.0.0.1` and **"Port"** at `1433` if SQL Server runs on this server at its default port. Keep `monitorflow` in **"User name"** and enter the login's password in **"Password"**.
5. Leave **"Add the recommended alerts"** on, then click **"Add check"**.

The password is kept sealed on the central server, encrypted like its other secrets. It is never shown again, never written to a log, and the API only says whether one is set. If you later change **"Address"** or **"Port"**, enter the password again.

Under **"More options"**, **"Encryption"** is **"When the server offers it"** (the default: the session is encrypted with the certificate SQL Server has, even the one it makes itself at start, which is not verified), **"Required, certificate verified"** (the certificate must be signed by an authority the machine trusts, for the name in **"Address"**), or **"None"**. With **"None"**, the password crosses the network in clear: keep it for a check on the server itself, and only if SQL Server refuses encryption. When encryption is asked for and SQL Server does not offer it, the check fails rather than sending the password in clear.

The check runs every 60 seconds by default. To watch every SQL Server of a site at once, choose **"Every server of"** the site under **"Add to"**, if every server of the site runs SQL Server with the same login.

### Where the check runs

**"Checked from"** chooses who connects:

- **"This server's agent"** (the default): the agent on the SQL Server machine, Windows or Linux, connects to `127.0.0.1`. If its administrator set port targets (`sensors.port_targets` in `agent.toml`), the address and port must be among them.
- **"The central server"**, or **"Relay"** followed by the name of a relay of the site: to check it from outside, the way applications reach it. Enter the server's address in **"Address"**, and let the Windows firewall accept the port from the central server or the relay. The central server never connects to link-local addresses (`169.254.x.x`, `fe80::`).

A device, a machine without the agent, such as an Azure SQL Managed Instance, has the same check in its catalogue: it always runs from the central server or a relay, against the device's address.

### Through the API

The public API adds the same check: `POST /api/v1/servers/{id}/sensor_configs` with the kind, its parameters, and `default_alerts` to add the recommended alerts. `GET /api/v1/check-templates` lists every metric and alert of each kind.

```json
{
  "kind": "mssql",
  "interval_ms": 60000,
  "params": { "host": "127.0.0.1", "port": "1433", "user": "monitorflow", "password": "choose-a-long-password" },
  "default_alerts": true
}
```

### Service and port checks

1. In **"Find a check"**, type `SQL Server` and pick **"SQL Server running"**. Keep `MSSQLSERVER` in **"Service"** or enter your named instance.
2. Leave **"Raise critical when it fails"** ticked. Under **"Add to"**, choose **"This server only"** or **"Every server of"** the site, then click **"Add check"**.
3. Do the same with **"SQL Server answers"**, with your port if it is not 1433.

Add them to a whole site only if every server of the site runs SQL Server: on the others, the missing service raises an alert.

## Change the alert

On the **Sensors** tab, the recommended alerts are ordinary alerts, one line each under **"When to alert"**. Open a line's menu and choose **"Edit"**: **"Alert"** sets critical or warning, **"For at least"** the minutes the problem must last (0 alerts at once; longer avoids an alert while Windows Update restarts the server), and **"Apply to"** this server or every server of the site. A server running nightly jobs may block for a few minutes on purpose: raise **"For at least"** of `processes_blocked` rather than removing it.

The service and port checks work the same way: open the check's menu and choose **"Edit"**. On a site's **"Alerts and checks"** page, the same settings are under **"When to alert"**, with **"Raise"** for the severity.

## Where the alert goes

Alerts show at once in **Monitoring › Incidents**. To be told, add a channel under **Settings › Notifications**: email, a webhook, or Slack, Teams or Discord, for everything, one customer or one site. A critical alert nobody acknowledges within 15 minutes also goes to the nearest administrators who set how they are reached on the **On call** page.

## When the alert fires

The message names the check, its value, and the server with its site:

```
Critical: mssql_up is 0 on sql-1 (paris)
Warning: mssql_processes_blocked is 3 on sql-1 (paris)
Critical: mssql_log_used_ratio shop is 0.97 on sql-1 (paris)
Critical: service_mssqlserver_active MSSQLSERVER is 0 on sql-1 (paris)
```

An email has that line as subject, after `[MonitorFlow]`, and a link to the alert. A message starting with `Resolved:` follows when SQL Server is back.

When `mssql_up` is 0, its `reason` label says why, and its `error` label gives SQL Server's own words:

| `reason` | What to check |
| --- | --- |
| `connect` | Nothing answers at that address and port: SQL Server stopped, TCP/IP is off, the instance listens on another port, or a firewall refuses the connection |
| `timeout` | No answer in time: a firewall that drops packets, or a server too loaded to answer |
| `tls` | Encryption failed: SQL Server offers none while the check asks for it, or requires it while **"Encryption"** is **"None"**, or with **"Required, certificate verified"** its certificate is not trusted or not for the name in **"Address"** |
| `login` | Wrong login or password (error 18456), SQL Server authentication is off, or the login's password expired |
| `permission` | The login lacks `VIEW SERVER STATE` (error 300) |
| `protocol` | Something else answered on that port, or SQL Server answered in a way the check does not read |

Click **"Acknowledge"** on the alert's page, then check:

1. **The service:** `Get-Service MSSQLSERVER` in PowerShell, then `Start-Service MSSQLSERVER` if it only needs starting.
2. **Why it stopped:** the Application and System logs in Event Viewer, and SQL Server's `ERRORLOG` in the instance's `Log` folder.
3. **Disk space:** a full data or log drive stops databases. The **Sensors** tab shows each drive.
4. **Blocking:** `SELECT session_id, blocking_session_id, wait_time, wait_type FROM sys.dm_exec_requests WHERE blocking_session_id <> 0;` shows who waits and for whom.
5. **A log that fills up:** `SELECT name, log_reuse_wait_desc FROM sys.databases;` says what keeps the log from being reused: a log backup that does not run, a long transaction, or a replica behind.

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then open **Settings › Add servers** in the web interface, choose Windows, and run the line it shows in PowerShell as administrator on each server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md#windows).
