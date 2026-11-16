---
title: "How to monitor Proxmox VE"
description: "Watch your Proxmox VE hosts: the web interface on port 8006, the cluster service, and the disk space, CPU and memory every VM depends on."
slug: monitor-proxmox-ve
lang: en
translation: surveiller-proxmox-ve
---

# How to monitor Proxmox VE

A Proxmox VE host carries many virtual machines and containers: when it runs short of disk or memory, or loses its cluster service, all of them suffer. MonitorFlow runs on the host like on any Linux server, and watches the Proxmox services next to disk, CPU and memory.

## Why it matters

Proxmox problems start small. Storage fills with snapshots and backups until VMs freeze on write. Memory is overcommitted and the kernel kills processes. `pve-cluster` stops after a network split or a full root disk, and the cluster configuration turns read-only: nobody can start, stop or migrate a VM. `pveproxy` stops and the web interface goes with it, found out when you need it most.

## What MonitorFlow checks

Proxmox VE is Debian underneath, so the Linux agent installs on the host and it appears in the fleet like any server. Search for "proxmox" in the check catalogue:

| Catalogue entry | What it does | Alert by default |
| --- | --- | --- |
| **"Proxmox web interface running"** | The systemd service `pveproxy`, the web interface and API, is active. Metric `service_pveproxy_active` | Critical when it stops, for 1 minute |
| **"Proxmox web interface answers"** | Port 8006 accepts TCP connections. Metric `port_8006_up` | Critical when it fails, for 1 minute |
| **"Proxmox cluster service running"** | `pve-cluster`, which holds the cluster's configuration, is active. Metric `service_pve_cluster_active` | Critical when it stops, for 1 minute |

Built in, with nothing to add:

- **Disk space** of each mounted file system: warning over 85% full, critical over 95%, for 5 minutes. Storage that is not a mounted file system, like an LVM-thin pool, is not measured. See [disk space](monitor-disk-space.md).
- **Disk activity**: a warning when a disk takes over 200 ms on average to answer, for 10 minutes.
- **Memory**: warning under 10% available, critical under 5%, for 5 minutes. **CPU**: warning over 90% for 10 minutes. See [CPU and memory](monitor-cpu-memory.md).
- **Offline**: critical when the host has not reported for 5 minutes. See [server down alerts](server-down-alerts.md).

Other services, like `pvedaemon` or `pvestatd`, can be watched with the plain **"A service is running"** check.

## Set it up

1. Install the agent on each host: open **Settings › Add servers** and paste the command in the host's shell. Proxmox has no `sudo` by default: as root, remove the word `sudo` from the command.
2. Open **Fleet** and click the host.
3. On its **"Sensors"** tab, choose **"Add a check"**.
4. In **"Find a check"**, type `proxmox` and pick **"Proxmox cluster service running"**. **"Service"** holds `pve-cluster`.
5. Keep **"Raise critical when it fails"** ticked.
6. Under **"Add to"**, choose **"Every server of"** the site that holds your hosts, or **"This server only"**.
7. Click **"Add check"**.

Add the two web interface checks the same way. For all hosts at once, open **Settings › Sites**, click **"Alerts and checks"** under the site, and use **"Add a check"** there.

## Change the alert

On the **"Sensors"** tab, open the row's menu and choose **"Edit"**. For a service, **"Alert"** chooses critical or warning; for disk or memory, set **"Warning at"** and **"Critical at"** in percent. **"For at least"** sets how many minutes the problem must last, and **"Apply to"** this server only or every server of the site, so a host whose storage runs full by design can get its own disk levels. For a site, use **"When to alert"** on its **"Alerts and checks"** page.

## Where the alert goes

Every alert shows in **Monitoring › Incidents** and on the fleet. To be told, add a channel under **Settings › Notifications**: email, webhook, Slack, Teams or Discord, for everything, one customer or one site; **"Send test"** checks it. The same check failing on several hosts is sent in one message. A critical alert nobody acknowledges within 15 minutes also goes to the nearest administrators who set how they are reached on the **On call** page.

## When the alert fires

```
Critical: service_pve_cluster_active pve-cluster.service is 0 on pve1 (paris)
Warning: disk_used_ratio / is 0.87 on pve1 (paris)
```

Disk and memory values are fractions: `0.87` is 87%. By email, the subject is this line after `[MonitorFlow]`, and the body links to the alert. Click **"Acknowledge"**, then check:

1. **The service:** `systemctl status pve-cluster` and `journalctl -u pve-cluster -n 50`, or the host's **"Logs"** tab.
2. **The cluster:** `pvecm status` shows whether the node has quorum.
3. **The root disk:** a full `/` stops `pve-cluster`; `df -h /` and `du -xsh /var/log /var/lib/vz` find what grew.
4. **The web interface:** `systemctl restart pveproxy` once the cause is fixed.

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then open **Settings › Add servers** in the web interface and paste the command it shows on each server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md).
