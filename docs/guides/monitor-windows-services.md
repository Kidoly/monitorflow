---
title: "How to monitor Windows services"
description: "Get an alert when a Windows service stops or disappears, such as Windows Update, Print Spooler, DNS Server or Windows Time."
slug: monitor-windows-services
lang: en
translation: surveiller-services-windows
---

# How to monitor Windows services

Most of what a Windows server does runs as a service: a web server, a database, a print queue, the clock, updates. When one stops, Windows does not tell anyone. MonitorFlow checks the services you choose and alerts you when one is not running.

## Why it matters

A service stops after a crash, a failed update or an expired password on its account, and many do not restart by themselves. The server still answers ping, so the first sign is often a user: printing no longer works, or updates stopped weeks ago.

## What MonitorFlow checks

The check **"A Windows service is running"** asks the Windows Service Control Manager about one service. It uses the **service name**, not the display name: `W32Time`, not "Windows Time". In the Services console, the name is under **Service name** in the service's properties; in PowerShell, `Get-Service` lists it in the `Name` column.

The catalogue has entries for common services, with the name filled in:

| Catalogue entry | Service name |
| --- | --- |
| Windows Update running | `wuauserv` |
| Print Spooler running | `Spooler` |
| DNS Server running | `DNS` |
| Windows Time running | `W32Time` |
| IIS running | `W3SVC` |
| SQL Server running | `MSSQLSERVER` |
| Active Directory running | `NTDS` |

A paused service is not running. A service that does not exist, removed or mistyped, is reported as not running too, so a wrong name raises an alert instead of passing silently.

The check is named after the service: for `Spooler`, it is `service_spooler`, and its alert watches `service_spooler_active` with the service name as instance. By default, the alert is **critical** when the service has not been running for **1 minute**.

## Set it up

From a server with the agent installed:

1. Open **Fleet**, click the server, and stay on its **Sensors** tab.
2. Click **"Add a check"**.
3. In **"Find a check"**, type the service, like `spooler`.
4. Choose the entry. In **"Service"**, check the name, or enter your own for the plain **"A Windows service is running"** check.
5. Leave **"Raise critical when it fails"** ticked.
6. Under **"Add to"**, choose **"This server only"** or **"Every server of"** its site, then click **"Add check"**.

For every server of a site, open **Settings › Sites**, then **"Alerts and checks"** for the site, and use **"Add a check"** under **"What is checked"**.

## Change the alert

On the server's **Sensors** tab, open the check's row menu and choose **"Edit"**. There you can:

- set **"Alert"** to critical or warning when it stops;
- change **"For at least"**, the minutes the service must be down before the alert opens. 0 alerts on the first bad value;
- choose under **"Apply to"** whether the change is for this server only or every server of the site.

For a site, the same settings are under **"When to alert"** on the site's **"Alerts and checks"** page.

## Where the alert goes

Every alert shows in **Monitoring › Incidents**. To be told, add a channel under **Settings › Notifications**: email, a webhook, Slack, Teams or Discord, for everything, one customer or one site. The same service down on several servers is sent together in one message. A critical alert nobody acknowledges within 15 minutes also goes to the nearest administrators who set how they are reached on the **On call** page.

## When the alert fires

The message names the check, the service, the value and the server with its site:

```
Critical: service_spooler_active Spooler is 0 on print-1 (paris)
```

By email, the subject is that line after `[MonitorFlow]`, with a link to the alert in the body. When the service runs again, `Resolved: service_spooler_active Spooler on print-1 (paris)` follows.

Click **"Acknowledge"** on the alert's page, then check:

1. **Its state**: `Get-Service Spooler` in PowerShell. Stopped, or missing?
2. **Why it stopped**: the System event log has the Service Control Manager's events, like 7031 or 7034 for a service that ended unexpectedly. To be told of those errors too, see [How to monitor the Windows event log](monitor-windows-event-log.md).
3. **Start it**: `Start-Service Spooler`. If it stops again, check its account.

For what a service cannot show, like a queue length, see [How to monitor anything with your own script](monitor-with-your-own-script.md).

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then open **Settings › Add servers** in the web interface, choose Windows, and run the line it shows in PowerShell as administrator on each Windows server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md).
