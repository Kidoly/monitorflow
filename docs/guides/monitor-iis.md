---
title: "How to monitor IIS"
description: "Get an alert when the IIS web server stops or no longer answers on ports 80 and 443 on your Windows servers."
slug: monitor-iis
lang: en
translation: surveiller-iis
---

# How to monitor IIS

IIS serves websites, intranets, APIs and the web part of many business applications on Windows Server. When it stops, every site on the server goes with it. MonitorFlow checks that the IIS service runs and that its ports answer, and alerts you when they do not.

## Why it matters

IIS stops after a failed update, a bad configuration change, or a restart that did not bring it back. A port can also stop answering while the service runs: a binding removed, a firewall rule. Users see an error or a timeout; you hear it when they call.

## What MonitorFlow checks

Two kinds of checks from the catalogue cover IIS:

| Catalogue entry | What it checks | Alert by default |
| --- | --- | --- |
| IIS running | The `W3SVC` service (World Wide Web Publishing Service) is running | Critical after 1 minute |
| Web server answers (HTTP) | Port 80 accepts connections | Critical after 1 minute |
| Web server answers (HTTPS) | Port 443 accepts connections | Critical after 1 minute |

**IIS running** creates the check `service_w3svc`, whose alert watches `service_w3svc_active` with `W3SVC` as instance. A paused or missing service counts as not running.

The port checks connect to the port, on this server by default (`127.0.0.1`), and count it as up when the connection is accepted. They are named `port_80` and `port_443`, and their alerts watch `port_80_up` and `port_443_up`. They do not read the page or the certificate: a site that answers with an error still passes. For that, see [How to monitor anything with your own script](monitor-with-your-own-script.md).

The "A log file gets a line" check cannot follow IIS's own logs yet, as they are named by date. Errors IIS writes to the System and Application event logs can be watched, see [How to monitor the Windows event log](monitor-windows-event-log.md).

## Set it up

From a Windows server with the agent installed:

1. Open **Fleet**, click the server, and stay on its **Sensors** tab.
2. Click **"Add a check"**.
3. In **"Find a check"**, type `iis` and choose **IIS running**. The **"Service"** field holds `W3SVC`.
4. Leave **"Raise critical when it fails"** ticked. Under **"Add to"**, choose **"This server only"** or **"Every server of"** its site, then click **"Add check"**.
5. Click **"Add a check"** again, type `https`, and choose **Web server answers (HTTPS)**. Keep `443` in **"Port"**. If the site listens only on one address, enter it in **"Address"**. Add the HTTP one too if you serve port 80.

To check from outside, add the port check on another server with the web server's name in **"Address"**: it also catches a firewall blocking the port.

For every web server of a site, open **Settings › Sites**, then **"Alerts and checks"** for the site, and use **"Add a check"** there.

## Change the alert

On the server's **Sensors** tab, open the check's row menu and choose **"Edit"**. Set **"Alert"** to critical or warning, change **"For at least"** (in minutes; 0 alerts on the first bad value), and choose under **"Apply to"** whether it is for this server only or every server of the site. A site's settings are under **"When to alert"** on its **"Alerts and checks"** page.

During a planned restart, use **"Silence for"** on the server's page.

## Where the alert goes

Every alert shows in **Monitoring › Incidents**. To be told, add a channel under **Settings › Notifications**: email, a webhook, Slack, Teams or Discord, for everything, one customer or one site. Alerts that open at the same time on a site, like the service and its ports, are sent together in one message. A critical alert nobody acknowledges within 15 minutes also goes to the nearest administrators who set how they are reached on the **On call** page.

## When the alert fires

The messages look like this:

```
Critical: service_w3svc_active W3SVC is 0 on web-1 (paris)
Critical: port_443_up is 0 on web-1 (paris)
```

By email, the subject is that line after `[MonitorFlow]`, with a link to the alert. Click **"Acknowledge"** on the alert's page, then check:

1. **The service**: `Get-Service W3SVC`. If it is stopped, `Start-Service W3SVC`, and look in the System event log for why.
2. **The site and its bindings** in IIS Manager: is the site started, does its binding still list the port and certificate?
3. **The application pool**: a stopped pool gives errors while the port answers.
4. **The firewall**, when only a check from another server fails.

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then open **Settings › Add servers** in the web interface, choose Windows, and run the line it shows in PowerShell as administrator on each Windows server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md).
