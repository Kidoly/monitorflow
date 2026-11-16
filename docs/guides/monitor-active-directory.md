---
title: "How to monitor Active Directory domain controllers"
description: "Watch your domain controllers: Active Directory, DNS Server, LDAP on port 389, Windows Time and System event log errors, with alerts."
slug: monitor-active-directory
lang: en
translation: surveiller-active-directory
---

# How to monitor Active Directory domain controllers

When a domain controller fails, sign-ins fail, group policies stop applying, and name resolution breaks. With two controllers, the second one often hides the failure until it fails too. MonitorFlow checks the services a domain controller depends on and alerts you when one stops.

## Why it matters

A domain controller depends on Active Directory Domain Services, DNS for clients to find it, LDAP for applications, and an accurate clock, since Kerberos refuses tickets when clocks drift apart by a few minutes. Any of them can stop after an update or a restart, and users only see the effects: "my password does not work".

## What MonitorFlow checks

The catalogue has an entry for each part, to add on every domain controller:

| Catalogue entry | What it checks | Alert by default |
| --- | --- | --- |
| Active Directory running | The `NTDS` service is running | Critical after 1 minute |
| DNS Server running | The `DNS` service is running | Critical after 1 minute |
| LDAP answers | Port 389 accepts connections | Critical after 1 minute |
| Windows Time running | The `W32Time` service is running | Critical after 1 minute |
| Errors in the System event log | Critical and error events written to the System log | Warning on each new error |

The service checks are named after the service: `service_ntds`, `service_dns` and `service_w32time`, whose alerts watch `service_ntds_active` and so on, with the service name as instance. A missing or paused service counts as not running. The port check, `port_389`, connects to port 389 on the server and its alert watches `port_389_up`. The event log check, `eventlog_system`, counts new critical and error events and its alert watches `eventlog_system_matches`.

The Windows Time check tells you the service runs, not how far the clock is off.

## Set it up

From a domain controller with the agent installed:

1. Open **Fleet**, click the server, and stay on its **Sensors** tab.
2. Click **"Add a check"**, type `active directory` in **"Find a check"**, and choose **Active Directory running**. The **"Service"** field holds `NTDS`.
3. Leave **"Raise critical when it fails"** ticked, choose **"This server only"** under **"Add to"**, and click **"Add check"**.
4. Do the same for **DNS Server running**, **LDAP answers**, **Windows Time running** and **Errors in the System event log**.

If your domain controllers have a site of their own in MonitorFlow, add the checks once for all of them: open **Settings › Sites**, then **"Alerts and checks"** for that site, and use **"Add a check"** there. Every domain controller added to the site later gets them too.

To know that clients can reach LDAP, add **LDAP answers** on a member server as well, with the domain controller's name or address in **"Address"**.

## Change the alert

On the server's **Sensors** tab, open the check's row menu and choose **"Edit"**. Set **"Alert"** to critical or warning, change **"For at least"** (minutes the problem must last; 0 alerts on the first bad value), and choose under **"Apply to"** whether it is for this server only or every server of the site. For a site, the same settings are under **"When to alert"** on its **"Alerts and checks"** page.

If the event log check is too noisy, raise its **"For at least"** so an alert opens only when errors keep coming.

## Where the alert goes

Every alert shows in **Monitoring › Incidents**. To be told, add a channel under **Settings › Notifications**: email, a webhook, Slack, Teams or Discord, for everything, one customer or one site. The same check failing on several domain controllers is sent together in one message. A critical alert nobody acknowledges within 15 minutes also goes to the nearest administrators who set how they are reached on the **On call** page.

## When the alert fires

The messages look like this:

```
Critical: service_ntds_active NTDS is 0 on dc-1 (paris)
Critical: port_389_up is 0 on dc-1 (paris)
Warning: eventlog_system_matches is 2 on dc-1 (paris)
```

By email, the subject is that line after `[MonitorFlow]`, with a link to the alert. Click **"Acknowledge"** on the alert's page, then check:

1. **The services**: `Get-Service NTDS, DNS, W32Time` in PowerShell, and `Start-Service` for one that stopped.
2. **The domain controller's health**: `dcdiag` and `repadmin /replsummary` show failed tests and replication errors.
3. **The clock**: `w32tm /query /status` shows the time source and the last sync.
4. **The event logs**: the System log, and the Directory Service and DNS Server logs in Event Viewer.

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then open **Settings › Add servers** in the web interface, choose Windows, and run the line it shows in PowerShell as administrator on each Windows server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md).
