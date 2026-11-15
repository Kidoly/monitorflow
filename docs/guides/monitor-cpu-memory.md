---
title: "How to monitor CPU and memory usage on Linux and Windows servers"
description: "Get alerted when a server's processors stay saturated or its memory runs out, with built-in thresholds you can change per server or site."
slug: monitor-cpu-memory
lang: en
translation: surveiller-cpu-memoire
---

# How to monitor CPU and memory usage on Linux and Windows servers

A server short of processor time or memory rarely stops at once. It slows down, then the kernel starts killing processes, and the first sign is a user saying the application is slow. MonitorFlow measures CPU and memory on every server from the moment its agent connects.

## Why it matters

A process stuck in a loop, a leak that grows for days, a backup running during busy hours, a VM given too little memory: all end the same way. Response times climb, swap fills up, and on Linux the out-of-memory killer stops a process, sometimes your database. A spike for a few seconds is normal. What you want to know is when it lasts.

## What MonitorFlow checks

Two checks run on every server, Linux and Windows, with nothing to add. The check catalogue lists them as already running:

| Check | What it measures | Alert by default |
| --- | --- | --- |
| **CPU usage** | Share of the processors in use, in total and per core (`cpu_usage_ratio`), and on Linux the load averages | Warning when CPU usage is over 90% for 10 minutes. No critical level |
| **Memory** | Available memory (`memory_available_ratio`), and swap | Warning when available memory is under 10% for 5 minutes, critical under 5% |

Available memory is what programs can still get without swapping, like the `available` column of `free`: the page cache does not count as used.

These built-in checks run every minute. The line **"Built-in checks run every 1 min on every server of"** the site, under the list on the **Sensors** tab, changes that for the whole site.

The catalogue also has **"Out of memory kills"**, not added by default. It counts lines matching `Out of memory|oom-kill` in `/var/log/kern.log`, so you know when the Linux kernel killed a process, even if memory looks fine a minute later.

## Set it up

CPU usage and memory need nothing. To add the out-of-memory check on a Linux server:

1. Open **Fleet**, click the server, and stay on its **Sensors** tab.
2. Choose **"Add a check"**, type `memory` in **"Find a check"**, and pick **"Out of memory kills"**. The form is filled in.
3. Under **"Add to"**, choose **"This server only"** or **"Every server of"** its site, then **"Add check"**.

It raises a warning on the first matching line written after it starts. `/var/log/kern.log` exists where rsyslog writes it, as on Debian and Ubuntu, and the agent's user must be in the `adm` group to read it: see [How to monitor log files](monitor-log-files.md). Kernel errors also show in the server's **Logs** tab, which forwards the warnings and errors of the systemd journal.

## Change the alert

On the server's **Sensors** tab, open the menu of the **CPU usage** or **Memory** row and choose **"Edit"**. Set **"Warning at"** and **"Critical at"** in percent (leave one empty to have none), and **"For at least"** a number of minutes. Under **"Apply to"**, choose this server only or every server of the site. For a build server that runs at full speed on purpose, raise the CPU warning to 98% for 30 minutes, for that server only.

For a whole site, open **Settings › Sites**, then **"Alerts and checks"** for the site, and use **"Edit"** under **"When to alert"**. A server's own value always wins over the site's.

An alert resolves when the value is back 2 points past its threshold, so a value sitting on the line does not open and close in turn.

## Where the alert goes

Alerts show at once in **Monitoring › Incidents**, on the fleet and on the server's page. To be told, add a channel under **Settings › Notifications**: email, webhook, Slack, Teams or Discord, for everything, one customer or one site. A critical alert nobody acknowledges within 15 minutes is also told to the nearest administrators, or to whoever the **On call** page names.

## When the alert fires

The message names the metric, its value as a fraction of 1, and the server and its site:

```
Warning: cpu_usage_ratio total is 0.96 on app-2 (paris)
Critical: memory_available_ratio is 0.04 on db-1 (paris)
```

A warning that gets worse sends `Escalated to critical: …`, and `Resolved: …` when it is over. Click **"Acknowledge"** on the alert's page so others know you are on it, then:

1. **Find the process.** `top` sorted by CPU or memory on Linux, Task Manager or `Get-Process | Sort-Object CPU -Descending` on Windows.
2. **Look at the trend.** Choose **"Open chart"** in the row's menu: a slow climb over days points to a leak, a step to a new job or a deployment.
3. **Check the kernel's view.** On Linux, `dmesg` or the **Logs** tab for out-of-memory kills, and `free -h` for swap.

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then paste the command from **Settings › Add servers** on each server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md).
