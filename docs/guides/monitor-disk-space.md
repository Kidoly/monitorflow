---
title: "How to monitor disk space on Linux and Windows servers"
description: "Get warned before a disk fills up and stops your services: per-disk alerts at 85% and 95% out of the box, plus slow-disk alerts."
slug: monitor-disk-space
lang: en
translation: surveiller-espace-disque
---

# How to monitor disk space on Linux and Windows servers

A full disk is the most common outage that nobody saw coming, though it was coming for weeks. MonitorFlow watches every disk of every server from the moment its agent connects, and warns you while there is still room to act.

## Why it matters

When a disk fills up, databases stop writing, logs are lost, updates fail halfway and services refuse to start. The cause is usually slow: logs never rotated, old backups, container images. Nothing breaks until the very last block is used.

A disk can also be slow rather than full: a failing drive or an overloaded storage array. Everything on it waits.

## What MonitorFlow checks

Two checks run on every server, with nothing to add. In the check catalogue they are listed as already running:

| Check | What it measures | Alert by default |
| --- | --- | --- |
| **Disk space** | Space used on each disk (`disk_used_ratio`, like `df`), and inodes where the file system has a limit | Warning when a disk is over 85% full for 5 minutes, critical over 95% |
| **Disk activity** | How busy each disk is (**Disk busy**) and how long it takes to answer one read or write (**Disk wait**, `diskio_wait_seconds`) | Warning when a disk's average wait is over 200 ms for 10 minutes |

On Linux, every mounted file system counts as a disk, network shares (NFS, SMB) included; pseudo file systems such as `tmpfs`, `proc` or `overlay` are left out. A share that stops answering is skipped for that round instead of blocking the others. On Windows, every fixed drive with a letter is watched (`C:\`, `D:\`).

## Set it up

There is nothing to add: the alerts apply to every disk of every server as soon as it connects. Open **Fleet**, click the server: its **Sensors** tab has a row per disk, and the **"Rule"** column says when each one alerts.

## Change the alert

Defaults rarely fit every disk. A 2 TB data volume at 85% still has 300 GB free; a 1 GB `/boot` at 85% is nearly full.

1. On the server's **Sensors** tab, open the menu of the disk's row and choose **"Edit"**.
2. Set **"Warning at"** and **"Critical at"** in percent, and **"For at least"** a number of minutes.
3. Under **"Apply to"**, choose this disk only, every disk of the server, or every server of the site (the site default). A value set for one disk wins over the one for every disk.
4. **"Save"**.

To stop alerting on a disk, such as a scratch volume that is meant to fill up, choose **"Turn off…"** in its row menu. For a whole site, open **Settings › Sites**, then **"Alerts and checks"** for the site, and use **"Edit"** under **"When to alert"**.

The slow-disk alert is changed the same way on the **Disk activity** rows. A busy disk that still answers fast is not a problem, so there is no alert on busy time.

An alert resolves when the value is back 2 points under its threshold, so a disk sitting at 85% does not open and close the alert in turn.

## Where the alert goes

Alerts show at once in **Monitoring › Incidents**, on the fleet and on the server's page. To be told, add a channel under **Settings › Notifications**: email, webhook, Slack, Teams or Discord, for everything, one customer or one site. A critical alert nobody acknowledges within 15 minutes is also told to the nearest administrators, or to whoever the **On call** page names.

## When the alert fires

The message names the disk, how full it is as a fraction of 1, and the server and its site:

```
Warning: disk_used_ratio /var is 0.87 on web-1 (paris)
Escalated to critical: disk_used_ratio /var is 0.96 on web-1 (paris)
```

Click **"Acknowledge"** on the alert's page so others know you are on it, then look for what grew:

- **Linux**: `df -h` and `df -i` (a disk can run out of inodes with space left), then `sudo du -xh --max-depth=1 /var | sort -h` to find the largest folders. Usual suspects are `/var/log`, the systemd journal (`journalctl --disk-usage`), old kernels, package caches and container images.
- **Windows**: Storage settings or `Get-ChildItem -Recurse | Sort-Object Length -Descending | Select-Object -First 20` in the suspect folder, and the logs of IIS or SQL Server if they run there.
- **A slow disk**: `iostat -x 5` on Linux shows which device waits, and the hypervisor or the storage array usually says why.

The row's chart (**"Open chart"** in its menu) shows how fast the disk filled, which tells a runaway log from slow growth.

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then paste the command from **Settings › Add servers** on each server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md).
