---
title: "How to get alerted when a server goes down"
description: "Know within minutes when a Linux or Windows server stops reporting, without setting anything up, and get told by email or chat."
slug: server-down-alerts
lang: en
translation: alerte-serveur-hors-ligne
---

# How to get alerted when a server goes down

A server that stops answering is the alert everyone expects, and the one that is easiest to miss: nobody looks at a machine that works, so a crash at night is found in the morning, by a user. MonitorFlow tells you within minutes, with no check to add.

## Why it matters

A server goes down for many reasons: a kernel panic, a full disk that stops services, a host that lost power, a network change, a VM paused by mistake. Each time, what runs on it stops too. Without an alert, you learn it from a ticket, a missed backup or a website that no longer loads, often hours later.

## What MonitorFlow checks

The agent on each server keeps a connection open to the central server and reports every 30 seconds. The central server watches that connection itself:

- A server with no contact for **more than 5 minutes** opens a **critical** alert: the server is offline. The alert starts from the last contact, so its duration is the real outage.
- It resolves on its own when the agent connects again.
- 5 minutes is longer than an agent restart, an update or a reboot of the central server, so routine work raises nothing. After the central server itself restarts, servers get 5 minutes to reconnect.
- When a site goes through a **relay** and the relay stops, one alert opens for the relay and says how many servers behind it cannot report. You get one message, not one per server.

This is built in. Every server added to MonitorFlow has it, Linux and Windows alike, and nothing on the server needs to be open: the agent only connects out.

## Set it up

There is nothing to add. To check that it is on for a server:

1. Open **Fleet** and click the server.
2. In the facts at the top of its page, **"Offline alerts"** shows **"On"**.

To watch a service from the outside as well, so you know when it stops answering even though the agent still runs, add a port check on another server. Open that other server, choose **"Add a check"**, pick **"A port answers"**, and enter the port and, in **"Address"**, the name or IP address of the server to watch. It raises a critical alert when the port does not answer for a minute.

## Change the alert

The 5-minute delay is fixed: one good default is simpler than a setting. What you can change is whether a machine is watched:

- On the server's page, **"Offline alerts"** has **"Turn off"**, for a machine you power off on purpose, like a test VM. Turning it off closes an open offline alert.
- For a whole site, open **Settings › Sites**: each site has its own offline alerts switch.
- For planned work, use a silence instead: on the server's page, **"Silence for"** a time, or **Monitoring › Silences** for a site or everything. Alerts still open and show during a silence, but nobody is told. What is still open when it ends is sent.

## Where the alert goes

Every alert shows at once in **Monitoring › Incidents** and on the fleet. To be told, add a channel under **Settings › Notifications**: an email address, a webhook, or a Slack, Teams or Discord channel, for everything, one customer or one site. **"Send test"** sends a test message right away.

If several servers of a site go down together, they are sent together in one message. A critical alert nobody acknowledges within 15 minutes is also told, in person, to the nearest administrators who set how they are reached on the **On call** page. Set up an on-call schedule and a follow-up there to choose who is told and when.

## When the alert fires

The message reads like this, with the server and its site:

```
Critical: web-1 (paris) is offline
```

By email, the subject is the same line after `[MonitorFlow]`, and the body has a link to the alert. When the server is back, a second message says `Resolved: web-1 (paris) is back online`.

Click **"Acknowledge"** on the alert's page to tell others you are on it. Then check, in this order:

1. **Is the machine up?** Ping it, or open its console in your hypervisor or cloud.
2. **Is the agent running?** On Linux, `systemctl status monitorflow-agent` and `journalctl -u monitorflow-agent`. On Windows, `Get-Service MonitorFlowAgent`, and the agent's log in `C:\ProgramData\MonitorFlow\agent\logs\agent.log`.
3. **Can it reach the central server?** The agent needs ports 7443 and 7445 of the central server, or of its relay. A firewall or DNS change is a common cause.

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then paste the command from **Settings › Add servers** on each server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md).
