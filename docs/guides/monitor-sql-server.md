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

The check signs in with a SQL Server login, or with a Windows account (below). Either needs one right: `VIEW SERVER STATE`, or `VIEW SERVER PERFORMANCE STATE` from SQL Server 2022. It lets the login read the server's counters and sessions, not your data, and it needs no user in any database. In SQL Server Management Studio, or with `sqlcmd`, as an administrator:

```sql
CREATE LOGIN monitorflow WITH PASSWORD = 'choose-a-long-password';
GRANT VIEW SERVER STATE TO monitorflow;
-- SQL Server 2022 and later: the narrower right is enough.
-- GRANT VIEW SERVER PERFORMANCE STATE TO monitorflow;
```

On an availability group, create the login on every replica, as logins are not copied with the databases.

**A SQL Server login needs SQL Server authentication on.** Many servers accept Windows sign-ins only. In Management Studio, open the server's **Properties › Security**, choose **"SQL Server and Windows Authentication mode"**, then restart the service. Or sign in with a Windows account instead.

### Or sign in with a Windows account

On a Windows server, the agent can sign in with a Windows account, through Windows itself (Kerberos in a domain, NTLM otherwise): no SQL Server login, no password to store. The agent runs as `NT SERVICE\MonitorFlowAgent`, which is the account SQL Server sees when it runs on the same machine. Give it the right:

```sql
CREATE LOGIN [NT SERVICE\MonitorFlowAgent] FROM WINDOWS;
GRANT VIEW SERVER STATE TO [NT SERVICE\MonitorFlowAgent];
```

When the agent checks a SQL Server on another machine of the domain, SQL Server sees the agent's computer account, its name followed by `$`: `CREATE LOGIN [EXAMPLE\APP1$] FROM WINDOWS;`, then the same `GRANT`. To use another account, a service account of the domain for example, enter it as `EXAMPLE\svc-monitor` (or `svc-monitor@example.com`) in **"User name"** with its password, kept sealed like a SQL Server login's.

Kerberos needs the server's full name in **"Address"** (`db1.example.com`) and SQL Server's service principal name, `MSSQLSvc/db1.example.com:1433`, registered in the domain, which SQL Server does itself when its service account may. With `127.0.0.1` or an address, Windows signs in with NTLM, which works for a SQL Server on the same machine and in most domains.

A Windows sign-in runs only on the agent of a Windows server: the central server and relays run on Linux and cannot sign in to Windows, so the form offers it on Windows servers only, and the API refuses it with **"Checked from"** set to a probe.

**TCP must be on.** SQL Server Express ships with TCP/IP turned off: in SQL Server Configuration Manager, turn on TCP/IP for the instance, then restart it.

**A named instance** (`SQLEXPRESS`, `SALES`) usually picks its port at start. Enter its name in **"Named instance"** and leave **"Port"** empty: at each check, MonitorFlow asks SQL Server Browser (the `SQLBrowser` service, UDP port 1434) for the instance's port, so it follows the instance when the port changes. SQL Server Browser must run and the Windows firewall must accept UDP 1434 from whoever checks. If you gave the instance a fixed port, enter it in **"Port"** too: Browser is then not asked. With the agent's port targets set, both port 1434 and the instance's port must be among them.

## Set it up

1. Open **Fleet**, click the SQL Server machine, and stay on its **Sensors** tab.
2. Click **"Add a check"** and type `SQL Server` in **"Find a check"**.
3. Pick **"SQL Server"**, the **"Application check"**.
4. Keep **"Address"** at `127.0.0.1` and leave **"Port"** empty if SQL Server runs on this server at its default port, 1433. For a named instance, enter its name in **"Named instance"**.
5. In **"Sign in with"**, keep **"A SQL Server login"**: leave **"User name"** empty for `monitorflow` or enter the login, and its password in **"Password"**. Or choose **"A Windows account"** and leave **"User name"** and **"Password"** empty to sign in as the agent's account.
6. Leave **"Add the recommended alerts"** on, then click **"Add check"**.

The password is kept sealed on the central server, encrypted like its other secrets. It is never shown again, never written to a log, and the API only says whether one is set. If you later change **"Address"** or **"Port"**, enter the password again.

Under **"More options"**, **"Encryption"** is **"When the server offers it"** (the default: the session is encrypted with the certificate SQL Server has, even the one it makes itself at start, which is not verified), **"Required, certificate verified"** (the certificate must be signed by an authority the machine trusts, for the name in **"Address"**), **"Strict (TDS 8.0), certificate verified"**, or **"None"**. Strict encryption, from SQL Server 2022, starts TLS before anything else is said, as a website does, and the certificate is verified as with **"Required, certificate verified"**; choose it when SQL Server has **"Force Strict Encryption"** on, which refuses every other client. With **"None"**, the password crosses the network in clear: keep it for a check on the server itself, and only if SQL Server refuses encryption. When encryption is asked for and SQL Server does not offer it, the check fails rather than sending the password in clear.

The check runs every 60 seconds by default. To watch every SQL Server of a site at once, choose **"Every server of"** the site under **"Add to"**, if every server of the site runs SQL Server with the same login.

### Where the check runs

**"Checked from"** chooses who connects:

- **"This server's agent"** (the default): the agent on the SQL Server machine, Windows or Linux, connects to `127.0.0.1`. Only this one signs in with a Windows account. If its administrator set port targets (`sensors.port_targets` in `agent.toml`), the address and port must be among them.
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

A named instance, signing in as the agent's Windows account, with strict encryption:

```json
{
  "kind": "mssql",
  "interval_ms": 60000,
  "params": { "host": "db1.example.com", "instance": "SALES", "auth": "windows", "tls": "strict" },
  "default_alerts": true
}
```

`instance` names the instance (its port then comes from SQL Server Browser unless `port` is given), `auth` is `sql` (the default) or `windows`, and `tls` is `prefer`, `verify`, `strict` or `off`.

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
| `connect` | Nothing answers at that address and port: SQL Server stopped, TCP/IP is off, the instance listens on another port, or a firewall refuses the connection. With a named instance, SQL Server Browser did not answer on UDP 1434, does not know the instance, or gave a port outside the agent's port targets |
| `timeout` | No answer in time: a firewall that drops packets, or a server too loaded to answer |
| `tls` | Encryption failed: SQL Server offers none while the check asks for it, or requires it while **"Encryption"** is **"None"**, or with **"Required, certificate verified"** or strict its certificate is not trusted or not for the name in **"Address"**. Strict needs SQL Server 2022 or later, with a certificate configured |
| `login` | Wrong login or password (error 18456), SQL Server authentication is off, or the login's password expired. With a Windows account: no login for it in SQL Server (error 18456), or Windows refused to sign in (the `error` label gives its code) |
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
