---
title: "How to monitor Docker"
description: "Get alerted when the Docker engine or containerd stops on a Linux server, and when a published container port stops answering."
slug: monitor-docker
lang: en
translation: surveiller-docker
---

# How to monitor Docker

When the Docker engine stops, every container on the server stops with it. MonitorFlow watches the engine, the runtime under it, the ports your containers publish, and the disk and memory they share.

## Why it matters

The engine stops more often than people expect: an update that restarts it with a configuration it rejects, a `daemon.json` edit, a full `/var/lib/docker`, the kernel killing it for lack of memory. Containers set to restart come back only if the engine does. Meanwhile the server answers ping and looks healthy, so the application's users find the outage.

## What MonitorFlow checks

Search for "docker" in the check catalogue:

| Catalogue entry | What it does | Alert by default |
| --- | --- | --- |
| **"Docker running"** | The systemd service `docker` is active. Metric `service_docker_active`, 1 when active | Critical when it stops, for 1 minute |
| **"containerd running"** | The systemd service `containerd`, the runtime under Docker and Kubernetes, is active. Metric `service_containerd_active` | Critical when it stops, for 1 minute |
| **"A port answers"** | A port a container publishes accepts TCP connections, like `8080`. Metric `port_8080_up` | Critical when it fails, for 1 minute |

MonitorFlow does not list containers or measure each one. The agent's user has no access to the Docker socket, on purpose: that access equals root on the machine. A port check per published port tells you the application inside answers, which is what users notice.

Two built-in checks matter on every Docker host, with no setup: disk space, which images, volumes and logs fill (warning over 85%, critical over 95%), and available memory (warning under 10%, critical under 5%). See [disk space](monitor-disk-space.md) and [CPU and memory](monitor-cpu-memory.md).

## Set it up

1. Open **Fleet** and click the server.
2. On its **"Sensors"** tab, choose **"Add a check"**.
3. In **"Find a check"**, type `docker` and pick **"Docker running"**. **"Service"** holds `docker`.
4. Keep **"Raise critical when it fails"** ticked.
5. Under **"Add to"**, choose **"This server only"**, or **"Every server of"** its site if all run Docker.
6. Click **"Add check"**.

Add **"containerd running"** the same way. For each published port that matters, add **"A port answers"** with the host-side port, the left part of `-p 8080:80`. The check connects to `127.0.0.1`, which reaches ports published on every address. For a whole site, open **Settings › Sites**, click **"Alerts and checks"** under the site, and use **"Add a check"** there.

## Change the alert

On the **"Sensors"** tab, open the row's menu and choose **"Edit"**. **"Alert"** chooses critical or warning, **"For at least"** how many minutes the problem must last (a container slow to start after a deployment may need 2 or 3 on its port check), and **"Apply to"** this server only or every server of the site. For a site, use **"When to alert"** on its **"Alerts and checks"** page.

## Where the alert goes

Every alert shows in **Monitoring › Incidents** and on the fleet. To be told, add a channel under **Settings › Notifications**: email, webhook, Slack, Teams or Discord, for everything, one customer or one site; **"Send test"** checks it. The same check failing on several servers, after an update for example, is sent in one message. A critical alert nobody acknowledges within 15 minutes also goes to the nearest administrators who set how they are reached on the **On call** page.

## When the alert fires

```
Critical: service_docker_active docker.service is 0 on app-1 (paris)
Critical: port_8080_up is 0 on app-1 (paris)
```

By email, the subject is this line after `[MonitorFlow]`, and the body links to the alert. A `Resolved:` message follows the fix. Click **"Acknowledge"**, then check:

1. **Why the engine stopped:** `systemctl status docker` and `journalctl -u docker -n 50`, or the server's **"Logs"** tab.
2. **The disk:** `df -h /var/lib/docker`, then `docker system df` for images, volumes and build cache.
3. **The container behind a port:** `docker ps -a` shows whether it exited, `docker logs <name>` why.

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then open **Settings › Add servers** in the web interface and paste the command it shows on each server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md).
