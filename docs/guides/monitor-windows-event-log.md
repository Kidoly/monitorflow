---
title: "How to monitor the Windows event log for errors"
description: "Get an alert when Windows writes a critical or error event to the System or Application log, on one server or a whole site."
slug: monitor-windows-event-log
lang: en
translation: surveiller-journal-evenements-windows
---

# How to monitor the Windows event log for errors

Windows writes what goes wrong to its event logs: a crashed service, a disk reporting bad blocks, a failed update. Nobody reads Event Viewer every morning, so these errors wait there until someone looks for the cause of an outage. MonitorFlow tells you when a new error is written.

## Why it matters

Many failures announce themselves first. A disk controller logs errors for days before a volume goes offline. A service that crashes and restarts by itself leaves only an event behind. Being told when the error is written lets you act before the outage.

## What MonitorFlow checks

The check **"Windows logs an error"** counts the new **critical** and **error** events of one event log. The catalogue has two entries with the log filled in:

| Catalogue entry | Event log |
| --- | --- |
| Errors in the System event log | `System`: services, drivers, disks, the kernel |
| Errors in the Application event log | `Application`: applications and their services |

How it works:

- Only events written after the check starts are counted. The history already in the log is not, so adding the check does not raise an alert for last month's errors.
- At each run, the agent counts the matching events written since the previous run. The check is named after the log, like `eventlog_system`, and its alert watches `eventlog_system_matches`, the number of new errors.
- By default, the alert is a **warning** as soon as one new error appears, and it resolves when a run finds none.
- The agent reads `Application` and `System` by default. Another log, like `Microsoft-Windows-TaskScheduler/Operational`, must first be listed in `event_channels` under `[sensors]` in the agent's `agent.toml` on that server, and the agent restarted: what the agent may read is decided on the server itself.

Events are counted, not copied, and Windows event logs are not shown in the **Logs** tab yet: the alert says how many errors arrived, Event Viewer says which.

## Set it up

From a Windows server with the agent installed:

1. Open **Fleet**, click the server, and stay on its **Sensors** tab.
2. Click **"Add a check"**, type `event` in **"Find a check"**, and choose **Errors in the System event log**. The **"Event log"** field holds `System`.
3. Leave **"Raise warning when it fails"** ticked. Under **"Add to"**, choose **"This server only"** or **"Every server of"** its site, then click **"Add check"**.
4. Add **Errors in the Application event log** the same way if you want both.

To add it to every Windows server of a site, open **Settings › Sites**, then **"Alerts and checks"** for the site, and use **"Add a check"** there.

## Change the alert

On the server's **Sensors** tab, open the check's row menu and choose **"Edit"**:

- **"Alert"**: warning or critical on an error.
- **"For at least"**: 0 alerts on the first error. A few minutes means an alert opens only when errors keep coming run after run, which quiets a log where an isolated error is common.
- **"Apply to"**: this server only, or every server of the site.

For a site, the same settings are under **"When to alert"** on its **"Alerts and checks"** page.

Start with the System log, usually quieter, and add the Application log where its errors mean something.

## Where the alert goes

Every alert shows in **Monitoring › Incidents**. To be told, add a channel under **Settings › Notifications**: email, a webhook, Slack, Teams or Discord, for everything, one customer or one site. The same check alerting on several servers is sent together in one message. A warning is told once; a critical one nobody acknowledges within 15 minutes also goes to the nearest administrators who set how they are reached on the **On call** page.

## When the alert fires

The message gives the number of new errors and the server with its site:

```
Warning: eventlog_system_matches is 3 on app-1 (paris)
```

By email, the subject is that line after `[MonitorFlow]`, with a link to the alert. Click **"Acknowledge"** on the alert's page, then:

1. Open **Event Viewer** on the server, or run `Get-WinEvent -FilterHashtable @{LogName='System'; Level=1,2} -MaxEvents 20` in PowerShell for the latest critical and error events.
2. Look at the **source** and **event ID**: they say which service, driver or application wrote it.
3. If it is a service that stopped, add a check that it runs: see [How to monitor Windows services](monitor-windows-services.md).

For text log files rather than event logs, see [How to monitor log files](monitor-log-files.md).

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then open **Settings › Add servers** in the web interface, choose Windows, and run the line it shows in PowerShell as administrator on each Windows server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md).
