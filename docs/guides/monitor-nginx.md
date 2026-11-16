---
title: "How to monitor nginx"
description: "Get alerted when nginx stops, when ports 80 and 443 stop answering, or when its error log reports critical errors, on every Linux server."
slug: monitor-nginx
lang: en
translation: surveiller-nginx
---

# How to monitor nginx

nginx sits in front of most of what a Linux server offers: websites, APIs, reverse proxies. When it stops, everything behind it disappears at once. MonitorFlow watches the nginx service, its ports and its error log.

## Why it matters

nginx rarely crashes on its own. It stops because of what surrounds it: a configuration typo that keeps it down after the next restart, a moved certificate, a full disk, a package update. From outside it all looks the same: the site is down, and a visitor notices first. The error log usually said why long before: `[crit]` lines about files nginx cannot open, `[emerg]` when a configuration cannot load.

## What MonitorFlow checks

Search for "nginx" in the check catalogue:

| Catalogue entry | What it does | Alert by default |
| --- | --- | --- |
| **"nginx running"** | The systemd service `nginx` is active. Metric `service_nginx_active`, 1 when active | Critical when it stops, for 1 minute |
| **"Web server answers (HTTP)"** | Port 80 accepts TCP connections. Metric `port_80_up` | Critical when it fails, for 1 minute |
| **"Web server answers (HTTPS)"** | Port 443 accepts TCP connections. Metric `port_443_up` | Critical when it fails, for 1 minute |
| **"nginx critical errors"** | Counts lines of `/var/log/nginx/error.log` marked `[crit]`, `[alert]` or `[emerg]`. Metric `log_error_matches` | Warning on a matching line, at once |

A service that is not installed counts as stopped. A port check only opens a TCP connection: it proves nginx accepts connections, not that pages load; for that, see [How to monitor a website over HTTPS](monitor-website-https.md). The log check counts only lines written after it starts, and follows the file through rotation.

## Set it up

1. Open **Fleet** and click the server.
2. On its **"Sensors"** tab, choose **"Add a check"**.
3. In **"Find a check"**, type `nginx` and pick **"nginx running"**. The **"Service"** field holds `nginx`.
4. Keep **"Raise critical when it fails"** ticked.
5. Under **"Add to"**, choose **"This server only"** or **"Every server of"** its site.
6. Click **"Add check"**.

Do the same for the two port checks and **"nginx critical errors"**. A port check connects to `127.0.0.1`; if nginx listens elsewhere, enter that address in **"Address"**. For a whole site, open **Settings › Sites**, click **"Alerts and checks"** under the site, and use **"Add a check"** there.

**If the log check shows "access denied".** On Debian and Ubuntu, nginx logs belong to the `adm` group, which the agent's user, `monitorflow`, is not in. Run `sudo usermod -aG adm monitorflow`, then `sudo systemctl restart monitorflow-agent`. Note that `adm` can read other logs of `/var/log` too. The agent retries the check every 5 minutes.

## Change the alert

On the **"Sensors"** tab, open the row's menu and choose **"Edit"**. **"Alert"** chooses critical or warning, **"For at least"** how many minutes the problem must last (`0` alerts on the first bad value), and **"Apply to"** this server only or every server of the site. For a site, use **"When to alert"** on its **"Alerts and checks"** page.

## Where the alert goes

Every alert shows in **Monitoring › Incidents** and on the fleet. To be told, add a channel under **Settings › Notifications**: email, webhook, Slack, Teams or Discord, for everything, one customer or one site; **"Send test"** checks it. When nginx stops on several servers together, they are sent in one message. A critical alert nobody acknowledges within 15 minutes also goes to the nearest administrators who set how they are reached on the **On call** page.

## When the alert fires

```
Critical: service_nginx_active nginx.service is 0 on web-1 (paris)
Critical: port_443_up is 0 on web-1 (paris)
Warning: log_error_matches is 3 on web-1 (paris)
```

By email, the subject is this line after `[MonitorFlow]`, and the body links to the alert. A `Resolved:` message follows the fix. Click **"Acknowledge"**, then check:

1. **Why it stopped:** `systemctl status nginx` and `journalctl -u nginx -n 50`, or the server's **"Logs"** tab.
2. **The configuration:** `sudo nginx -t` names the file and line at fault.
3. **The error log:** `tail -n 50 /var/log/nginx/error.log`.
4. **Disk and memory:** both can stop nginx ([disk space](monitor-disk-space.md), [CPU and memory](monitor-cpu-memory.md)).

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then open **Settings › Add servers** in the web interface and paste the command it shows on each server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md).
