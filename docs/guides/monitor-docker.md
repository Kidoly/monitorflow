---
title: "How to monitor Docker"
description: "Watch each Docker or Podman container: whether it runs, its restarts, its health check, its processor and memory, and get alerted when the engine or a container stops."
slug: monitor-docker
lang: en
translation: surveiller-docker
---

# How to monitor Docker

When the Docker engine stops, every container on the server stops with it, and a single container can crash in a loop while the server looks fine. MonitorFlow watches each container, the engine and the runtime under it, the ports your containers publish, and the disk and memory they share.

## Why it matters

The engine stops more often than people expect: an update that restarts it with a configuration it rejects, a `daemon.json` edit, a full `/var/lib/docker`, the kernel killing it for lack of memory. Containers set to restart come back only if the engine does. A container can also exit, restart every few seconds, fail its health check or reach its memory limit on its own. Meanwhile the server answers ping and looks healthy, so the application's users find the outage.

## What MonitorFlow checks

### The Docker containers check

The **"Docker containers"** entry of the catalogue (an **"Application check"**) reads the local engine's API every minute, on Linux servers with the agent. For each watched container, it reads:

- whether it runs, with its image and state;
- its restarts, in total and since the last check;
- its health check, when it has one: 0 healthy, 1 starting, 2 unhealthy;
- its share of the machine's processor time, and its memory against its limit.

It also counts the watched containers that run and those that do not, and reports whether the engine answers.

**Which containers are watched:** those that run, and those stopped whose restart policy is `always` or `unless-stopped`, as they are meant to keep running. One-off containers that exited are left out. **"Containers"** narrows the list with a regular expression on their names.

Metrics are named after the check: `monitorflow_docker_up`, `monitorflow_docker_container_running`, and so on (with another check name, that name replaces `docker`). Each container's metrics carry its name.

With **"Add the recommended alerts"** on, these alerts come with it:

| Metric | Warning | Critical | For |
| --- | --- | --- | --- |
| `monitorflow_docker_up` (the engine answers) | None | Under 1 | 2 minutes |
| `monitorflow_docker_container_running` (per container) | None | Under 1 | 2 minutes |
| `monitorflow_docker_container_restarts` (per container, since the last check) | Over 0 | None | At once |
| `monitorflow_docker_container_health` (per container) | None | 2 or more (unhealthy) | 2 minutes |
| `monitorflow_docker_container_memory_ratio` (per container) | Over 0.9 (90%) | None | 5 minutes |

### Service and port checks

Search for "docker" in the check catalogue for the simpler checks:

| Catalogue entry | What it does | Alert by default |
| --- | --- | --- |
| **"Docker running"** | The systemd service `docker` is active. Metric `service_docker_active`, 1 when active | Critical when it stops, for 1 minute |
| **"containerd running"** | The systemd service `containerd`, the runtime under Docker and Kubernetes, is active. Metric `service_containerd_active` | Critical when it stops, for 1 minute |
| **"A port answers"** | A port a container publishes accepts TCP connections, like `8080`. Metric `port_8080_up` | Critical when it fails, for 1 minute |

These need no access to the engine. A port check per published port tells you the application inside answers, which is what users notice.

Two built-in checks matter on every Docker host, with no setup: disk space, which images, volumes and logs fill (warning over 85%, critical over 95%), and available memory (warning under 10%, critical under 5%). See [disk space](monitor-disk-space.md) and [CPU and memory](monitor-cpu-memory.md).

## Let the agent read the engine

The agent's user, `monitorflow`, has no access to the Docker socket, on purpose: whoever can use that socket can start a container that mounts the whole disk, so the access equals root on the machine. Grant it only if you accept that. The check itself only reads (`GET` requests): it never starts, stops or changes a container.

To grant it, add the agent's user to the `docker` group and restart the agent:

```sh
sudo usermod -aG docker monitorflow
sudo systemctl restart monitorflow-agent
```

Until then, the check reports `docker_up` 0 with the reason `permission`, and its error says what to run.

**Podman:** choose **"Podman"** in **"Engine"**. The check reads the rootful socket, `/run/podman/podman.sock`: turn it on with `sudo systemctl enable --now podman.socket`. That socket belongs to root; to let the agent's user open it, give it a group the same way, for example with `sudo systemctl edit podman.socket` and these lines, then `sudo systemctl restart podman.socket`:

```ini
[Socket]
SocketGroup=monitorflow
SocketMode=0660
```

The same warning applies: access to the Podman socket equals root.

## Set it up

1. Open **Fleet** and click the server.
2. On its **"Sensors"** tab, choose **"Add a check"**.
3. In **"Find a check"**, type `docker` and pick **"Docker containers"**, the **"Application check"**.
4. Leave **"Containers"** empty to watch every container meant to run, or enter a regular expression like `^web-` to watch only those whose names start with `web-`.
5. Leave **"Add the recommended alerts"** on.
6. Under **"Add to"**, choose **"This server only"**, or **"Every server of"** its site if all run Docker, then click **"Add check"**.

**"More options"** holds **"Engine"**: **"Docker"** (the default, `/var/run/docker.sock`) or **"Podman"**. The check always runs on the server's own agent, never from the central server or a relay, as the socket is local.

For the simpler checks, add **"Docker running"** (**"Service"** holds `docker`) and **"containerd running"** the same way, with **"Raise critical when it fails"** ticked. For each published port that matters, add **"A port answers"** with the host-side port, the left part of `-p 8080:80`. The check connects to `127.0.0.1`, which reaches ports published on every address. For a whole site, open **Settings › Sites**, click **"Alerts and checks"** under the site, and use **"Add a check"** there.

### Through the API

The public API adds the same check: `POST /api/v1/servers/{id}/sensor_configs` with the kind, its parameters, and `default_alerts` to add the recommended alerts. `GET /api/v1/check-templates` lists every metric and alert of each kind.

```json
{
  "kind": "docker",
  "interval_ms": 60000,
  "params": { "engine": "docker", "containers": "^web-" },
  "default_alerts": true
}
```

## Change the alert

On the **"Sensors"** tab, the recommended alerts are ordinary alerts, one line each under **"When to alert"**. Open a line's menu and choose **"Edit"**: **"Alert"** chooses critical or warning, **"For at least"** how many minutes the problem must last (a container slow to start after a deployment may need 3 or 5 on `container_running`), and **"Apply to"** this server only or every server of the site. The restart alert fires at once on purpose: a container that restarts by itself has crashed.

The service and port checks work the same way: open the row's menu and choose **"Edit"**. For a site, use **"When to alert"** on its **"Alerts and checks"** page.

## Where the alert goes

Every alert shows in **Monitoring › Incidents** and on the fleet. To be told, add a channel under **Settings › Notifications**: email, webhook, Slack, Teams or Discord, for everything, one customer or one site; **"Send test"** checks it. The same check failing on several servers, after an update for example, is sent in one message. A critical alert nobody acknowledges within 15 minutes also goes to the nearest administrators who set how they are reached on the **On call** page.

## When the alert fires

```
Critical: docker_up is 0 on app-1 (paris)
Critical: docker_container_running web-1 is 0 on app-1 (paris)
Warning: docker_container_restarts web-1 is 1 on app-1 (paris)
Critical: service_docker_active docker.service is 0 on app-1 (paris)
Critical: port_8080_up is 0 on app-1 (paris)
```

By email, the subject is this line after `[MonitorFlow]`, and the body links to the alert. A `Resolved:` message follows the fix.

When `docker_up` is 0, its `reason` label says why, and its `error` label gives the details:

| `reason` | What to check |
| --- | --- |
| `connect` | No engine socket: the engine stopped, is not installed, or **"Engine"** names the wrong one (for Podman, `podman.socket` is not on) |
| `permission` | The agent's user may not open the socket: see "Let the agent read the engine" above |
| `timeout` | The engine did not answer in time: it may be hung or overloaded |
| `protocol` | The engine answered with an error |

Click **"Acknowledge"**, then check:

1. **Why the engine stopped:** `systemctl status docker` and `journalctl -u docker -n 50`, or the server's **"Logs"** tab.
2. **The disk:** `df -h /var/lib/docker`, then `docker system df` for images, volumes and build cache.
3. **A container that stopped or restarts:** `docker ps -a` shows its state, `docker logs <name>` why, and `docker inspect <name>` its exit code and whether it was killed for memory (`OOMKilled`).
4. **An unhealthy container:** `docker inspect --format '{{json .State.Health}}' <name>` shows the last health check results.

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then open **Settings › Add servers** in the web interface and paste the command it shows on each server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md).
