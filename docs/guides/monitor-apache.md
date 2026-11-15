---
title: "How to monitor Apache"
description: "Get alerted when the Apache web server stops or ports 80 and 443 stop answering, on Debian, Ubuntu, RHEL and Rocky servers."
slug: monitor-apache
lang: en
translation: surveiller-apache
---

# How to monitor Apache

The Apache HTTP Server still runs many websites, intranets and PHP applications. When it stops, every site on the server stops with it. MonitorFlow watches the Apache service and its ports, and tells you within a minute or two.

## Why it matters

Apache stops for ordinary reasons: a module or virtual host that fails after an edit and a restart, a moved certificate, all workers stuck behind a slow application, a full disk, a package update. A stopped Apache is silent, and the first report comes from a user who cannot reach the site.

## What MonitorFlow checks

Search for "apache" in the check catalogue:

| Catalogue entry | What it does | Alert by default |
| --- | --- | --- |
| **"Apache running"** | The systemd service is active: `apache2` on Debian and Ubuntu, `httpd` on RHEL and Rocky. Metric `service_apache2_active` or `service_httpd_active`, 1 when active | Critical when it stops, for 1 minute |
| **"Web server answers (HTTP)"** | Port 80 accepts TCP connections. Metric `port_80_up` | Critical when it fails, for 1 minute |
| **"Web server answers (HTTPS)"** | Port 443 accepts TCP connections. Metric `port_443_up` | Critical when it fails, for 1 minute |

A service that is not installed counts as stopped, which also catches a wrong unit name. A port check only opens a TCP connection: it proves Apache accepts connections, not that pages load; for that, see [How to monitor a website over HTTPS](monitor-website-https.md).

To catch errors early, add **"A log file gets a line"** on `/var/log/apache2/error.log` or `/var/log/httpd/error_log`, with a pattern such as `\[[a-z0-9_]*:(crit|alert|emerg)\]`. The agent's user, `monitorflow`, cannot read that folder by default. See [How to monitor log files](monitor-log-files.md).

## Set it up

1. Open **Fleet** and click the server.
2. On its **"Sensors"** tab, choose **"Add a check"**.
3. In **"Find a check"**, type `apache` and pick **"Apache running"**.
4. **"Service"** holds `apache2`. On RHEL, Rocky and other Red Hat family systems, replace it with `httpd`.
5. Keep **"Raise critical when it fails"** ticked.
6. Under **"Add to"**, choose **"This server only"**, or **"Every server of"** its site when all run the same system.
7. Click **"Add check"**.

Add the two port checks the same way. A port check connects to `127.0.0.1`; if Apache listens elsewhere, enter that address in **"Address"**. For a whole site, open **Settings › Sites**, click **"Alerts and checks"** under the site, and use **"Add a check"** there.

## Change the alert

On the **"Sensors"** tab, open the row's menu and choose **"Edit"**. **"Alert"** chooses critical or warning, **"For at least"** how many minutes the problem must last (raise it if a nightly job restarts Apache, or `0` to alert on the first bad value), and **"Apply to"** this server only or every server of the site. For a site, use **"When to alert"** on its **"Alerts and checks"** page.

## Where the alert goes

Every alert shows in **Monitoring › Incidents** and on the fleet. To be told, add a channel under **Settings › Notifications**: email, webhook, Slack, Teams or Discord, for everything, one customer or one site; **"Send test"** checks it. The same check failing on several servers is sent in one message. A critical alert nobody acknowledges within 15 minutes also goes to the nearest administrators who set how they are reached on the **On call** page.

## When the alert fires

```
Critical: service_apache2_active apache2.service is 0 on web-2 (lyon)
Critical: port_80_up is 0 on web-2 (lyon)
```

By email, the subject is this line after `[MonitorFlow]`, and the body links to the alert. A `Resolved:` message follows the fix. Click **"Acknowledge"**, then check:

1. **Why it stopped:** `systemctl status apache2` (or `httpd`) and `journalctl -u apache2 -n 50`, or the server's **"Logs"** tab.
2. **The configuration:** `sudo apachectl configtest` names the file and line at fault.
3. **The error log:** `tail -n 50 /var/log/apache2/error.log`, or `/var/log/httpd/error_log`.
4. **Disk and memory:** both can stop Apache ([disk space](monitor-disk-space.md), [CPU and memory](monitor-cpu-memory.md)).

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then open **Settings › Add servers** in the web interface and paste the command it shows on each server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md).
