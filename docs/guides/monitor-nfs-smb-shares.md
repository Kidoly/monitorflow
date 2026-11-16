---
title: "How to monitor NFS and SMB file shares"
description: "Get alerted when a file server stops answering on NFS or SMB, when its service stops, or when a mounted share fills up on a Linux client."
slug: monitor-nfs-smb-shares
lang: en
translation: surveiller-partages-nfs-smb
---

# How to monitor NFS and SMB file shares

When a file server stops answering, every machine that depends on it feels it at once. MonitorFlow watches the file server, the shares from the clients' side, and the space left on them.

## Why it matters

A file server stops after an update, a network change or a crashed service. The clients do not crash: their processes wait on the mount, a backup never ends, a web application stops answering. The first report is "the server is slow", not "the share is down".

## What MonitorFlow checks

| Catalogue entry | Where | What it does | Alert when added |
| --- | --- | --- | --- |
| **"NFS answers"** | Linux | Port 2049 accepts TCP connections. Metric `port_2049_up` | Critical when it fails, for 1 minute |
| **"File sharing answers (SMB)"** | Linux, Windows | Port 445 accepts TCP connections. Metric `port_445_up` | Critical when it fails, for 1 minute |
| **"A service is running"** | Linux | The file server's unit: `nfs-server` for NFS, `smbd` for Samba on Debian and Ubuntu, `smb` on RHEL and Rocky | Critical when it stops, for 1 minute |
| **"A Windows service is running"** | Windows | `LanmanServer`, the Windows file sharing service | Critical when it stops, for 1 minute |
| **"Disk space"** | Linux | Built in: space used on every mount, NFS and SMB shares included. Metric `disk_used_ratio` | Warning over 85% full, critical over 95%, for 5 minutes |

A port check connects from the agent, to `127.0.0.1` by default: on the file server itself, that checks the server answers. On a client, enter the file server's name in **"Address"** instead: that checks the client can reach it, through the network and firewalls in between. If the client's administrator limited where port checks may connect (`port_targets` in `agent.toml`), the file server must be in that list.

On Linux clients, a mounted share is measured like a local disk, every check, at its mount point. A share that hangs is skipped for that round instead of blocking the agent, so a server that stopped answering shows on the port check, not on disk space. Windows skips mapped network drives.

## Set it up

1. Open **Fleet** and click the file server.
2. On its **Sensors** tab, choose **"Add a check"**.
3. In **"Find a check"**, type `nfs` or `smb` and pick **"NFS answers"** or **"File sharing answers (SMB)"**.
4. Leave **"Raise critical when it fails"** ticked, choose **"This server only"** under **"Add to"**, then **"Add check"**.
5. Add the service: search `service`, pick **"A service is running"** (or **"A Windows service is running"**) and enter the unit or service name.
6. On each client that mounts the share, add the same port check with the file server's name in **"Address"**.

Disk space needs no step: it is already on every server.

## Change the alert

On the **Sensors** tab, open the check's row menu and choose **"Edit"**. For a port or a service, **"Alert"** switches between critical and warning and **"For at least"** sets how many minutes it must last. For a share's disk space, set **"Warning at"** and **"Critical at"** in percent. **"Apply to"** chooses that one mount, the server, or every server of the site. On a site's **"Alerts and checks"** page, the same settings are under **"When to alert"**, where the severity is called **"Raise"**.

## Where the alert goes

Alerts show at once in **Monitoring › Incidents**, on the fleet and on the server's page. To be told, add a channel under **Settings › Notifications**: email, webhook, Slack, Teams or Discord, for everything, one customer or one site. A critical alert nobody acknowledges within 15 minutes is also told to the nearest administrators, or to whoever the **On call** page names.

## When the alert fires

The message names the check's metric, its value, and the server that runs the check, with its site:

```
Critical: port_2049_up is 0 on app-1 (paris)
Warning: disk_used_ratio /mnt/backup is 0.88 on app-1 (paris)
```

From a client, the server named is the client, not the file server. By email, the subject is the same line after `[MonitorFlow]`, with a link to the alert. **"Acknowledge"** tells others you are on it. Then check:

1. **The file server:** is it up, and does its service run? `systemctl status nfs-server` or `smbd`, `Get-Service LanmanServer` on Windows.
2. **The network, if only some clients fail:** from a client, `nc -zv <file server> 2049` (or 445).
3. **Space:** `df -h <mount point>` on the client, then what grew on the file server. See [How to monitor disk space](monitor-disk-space.md).

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then paste the command from **Settings › Add servers** on each server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md).
