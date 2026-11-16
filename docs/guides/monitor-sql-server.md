---
title: "How to monitor SQL Server"
description: "Get alerted when a SQL Server instance stops or stops accepting connections on Windows, with the MSSQLSERVER service and port 1433 checks."
slug: monitor-sql-server
lang: en
translation: surveiller-sql-server
---

# How to monitor SQL Server

An ERP, an accounting package or a business application often depends on one SQL Server instance on one Windows server. When it stops, everyone who uses the application is stuck. MonitorFlow watches the SQL Server service and its port and tells you within a minute or two.

## Why it matters

SQL Server stops after a Windows update that restarted the server and left the service stopped, when a log or data drive fills up, when the service account's password changed, or after a crash. Users call to say the application "does not open", and time goes into finding which server is at fault.

## What MonitorFlow checks

| Catalogue entry | What it checks | Alert by default |
| --- | --- | --- |
| **"SQL Server running"** | The Windows service `MSSQLSERVER`, the default instance, is running. Metric `service_mssqlserver_active`: 1 running, 0 not | Critical when it stops, for 1 minute |
| **"SQL Server answers"** | Port 1433 accepts a TCP connection on 127.0.0.1. Metric `port_1433_up` | Critical when it fails, for 1 minute |

**Named instances** run as `MSSQL$` followed by the instance name, like `MSSQL$SQLEXPRESS`: enter the service name, not its display name. A named instance usually picks its port at start, so give it a fixed port in SQL Server Configuration Manager before checking it. SQL Server Express ships with TCP/IP turned off: the port check fails until you turn it on.

A service that is not installed counts as stopped, so a wrong name raises an alert. To watch SQL Server Agent too, add **"A Windows service is running"** with `SQLSERVERAGENT` (or `SQLAgent$` and the instance name).

SQL Server writes its errors to the Windows Application event log: **"Errors in the Application event log"** raises a warning on each new error or critical event there. Disk space of each drive is checked without setup.

On Linux, use the generic checks: **"A service is running"** with `mssql-server`, and **"A port answers"** with 1433.

## Set it up

1. Open **Fleet**, click the Windows server, and stay on its **Sensors** tab.
2. Click **"Add a check"** and type `SQL Server` in **"Find a check"**.
3. Pick **"SQL Server running"**. Keep `MSSQLSERVER` in **"Service"** or enter your named instance.
4. Leave **"Raise critical when it fails"** ticked. Under **"Add to"**, choose **"This server only"** or **"Every server of"** the site, then click **"Add check"**.
5. Do the same with **"SQL Server answers"**, with your port if it is not 1433.

The checks run every 60 seconds by default. For a whole site, open **Settings › Sites**, click **"Alerts and checks"** under the site and use **"Add a check"** there, but only if every server of the site runs SQL Server: on the others, the missing service raises an alert.

## Change the alert

On the **Sensors** tab, open the check's menu and choose **"Edit"**. **"Alert"** sets critical or warning, **"For at least"** the minutes the problem must last (0 alerts at once; longer avoids an alert while Windows Update restarts the server), and **"Apply to"** this server or every server of the site. On a site's **"Alerts and checks"** page, the same settings are under **"When to alert"**, with **"Raise"** for the severity.

## Where the alert goes

Alerts show at once in **Monitoring › Incidents**. To be told, add a channel under **Settings › Notifications**: email, a webhook, or Slack, Teams or Discord, for everything, one customer or one site. A critical alert nobody acknowledges within 15 minutes also goes to the nearest administrators who set how they are reached on the **On call** page.

## When the alert fires

The message names the check, its value, and the server with its site:

```
Critical: service_mssqlserver_active MSSQLSERVER is 0 on sql-1 (paris)
Critical: port_1433_up is 0 on sql-1 (paris)
```

An email has that line as subject, after `[MonitorFlow]`, and a link to the alert. A message starting with `Resolved:` follows when SQL Server is back.

Click **"Acknowledge"** on the alert's page, then check:

1. **The service:** `Get-Service MSSQLSERVER` in PowerShell, then `Start-Service MSSQLSERVER` if it only needs starting.
2. **Why it stopped:** the Application and System logs in Event Viewer, and SQL Server's `ERRORLOG` in the instance's `Log` folder.
3. **Disk space:** a full data or log drive stops databases. The **Sensors** tab shows each drive.
4. **The service runs but the port fails:** check in SQL Server Configuration Manager that TCP/IP is on and which port it uses, and the Windows firewall.

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then open **Settings › Add servers** in the web interface, choose Windows, and run the line it shows in PowerShell as administrator on each server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md#windows).
