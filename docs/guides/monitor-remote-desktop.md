---
title: "How to monitor Remote Desktop (RDP)"
description: "Get an alert when Remote Desktop stops answering on port 3389 or its service stops, on your Windows servers."
slug: monitor-remote-desktop
lang: en
translation: surveiller-bureau-a-distance
---

# How to monitor Remote Desktop (RDP)

Remote Desktop is how administrators manage Windows servers, and on a terminal server it is how users work every day. When it stops answering, you lose your way in, often at the moment you need it most. MonitorFlow checks that Remote Desktop answers and that its service runs, and alerts you when they do not.

## Why it matters

Remote Desktop stops answering after a service crash, an update that changed a setting, or a firewall rule pushed by policy. On an administration server, you find out when you try to connect during an incident; on a terminal server, your users find out first, all at once.

## What MonitorFlow checks

| Check | What it checks | Alert by default |
| --- | --- | --- |
| Remote Desktop answers (catalogue) | Port 3389 accepts connections | Critical after 1 minute |
| A Windows service is running, with `TermService` | The Remote Desktop Services service is running | Critical after 1 minute |

**Remote Desktop answers** connects to port 3389 and counts it as up when the connection is accepted. By default it connects to the server itself (`127.0.0.1`). The check is named `port_3389`, and its alert watches `port_3389_up`. It does not open a session: it tells you the port answers, not that sign-ins work.

**Remote Desktop Services** has the service name `TermService`. The catalogue has no entry for it, so use the plain **"A Windows service is running"** check with that name. It is named `service_termservice`, and its alert watches `service_termservice_active` with `TermService` as instance. A missing or paused service counts as not running.

## Set it up

On the server itself, from a Windows server with the agent installed:

1. Open **Fleet**, click the server, and stay on its **Sensors** tab.
2. Click **"Add a check"**, type `remote desktop` in **"Find a check"**, and choose **Remote Desktop answers**. Keep `3389` in **"Port"** and `127.0.0.1` in **"Address"**; enter another port if you moved RDP to one.
3. Leave **"Raise critical when it fails"** ticked. Under **"Add to"**, choose **"This server only"** or **"Every server of"** its site, then click **"Add check"**.
4. Click **"Add a check"** again, type `windows service`, choose **"A Windows service is running"**, enter `TermService` in **"Service"**, and add it the same way.

A check on `127.0.0.1` does not see a firewall that blocks port 3389 from the network. To test the way you connect, add the port check on **another server** of the same network and enter the Windows server's name or address in **"Address"**. On another Windows server, the **Remote Desktop answers** entry works as is. On a Linux server, choose the plain **"A port answers"** check and enter `3389`.

For every Windows server of a site, open **Settings › Sites**, then **"Alerts and checks"** for the site, and use **"Add a check"** there.

## Change the alert

On the server's **Sensors** tab, open the check's row menu and choose **"Edit"**. Set **"Alert"** to critical or warning, change **"For at least"** (minutes the problem must last before the alert opens; 0 alerts on the first bad value), and choose under **"Apply to"** whether it is for this server only or every server of the site. For a site, the same settings are under **"When to alert"** on its **"Alerts and checks"** page.

For a planned restart, use **"Silence for"** on the server's page.

## Where the alert goes

Every alert shows in **Monitoring › Incidents**. To be told, add a channel under **Settings › Notifications**: email, a webhook, Slack, Teams or Discord, for everything, one customer or one site. The same check failing on several servers is sent together in one message. A critical alert nobody acknowledges within 15 minutes also goes to the nearest administrators who set how they are reached on the **On call** page.

## When the alert fires

The messages look like this:

```
Critical: port_3389_up is 0 on ts-1 (paris)
Critical: service_termservice_active TermService is 0 on ts-1 (paris)
```

By email, the subject is that line after `[MonitorFlow]`, with a link to the alert. Click **"Acknowledge"** on the alert's page, then check:

1. **The service**: `Get-Service TermService`, and `Start-Service TermService` if it stopped.
2. **Is Remote Desktop still allowed?** In System Properties, or a group policy that turned it off.
3. **The firewall**: `Test-NetConnection <server> -Port 3389` from another machine, and the rules for Remote Desktop on the server.
4. **The System event log**, for the reason the service stopped.

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then open **Settings › Add servers** in the web interface, choose Windows, and run the line it shows in PowerShell as administrator on each Windows server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md).
