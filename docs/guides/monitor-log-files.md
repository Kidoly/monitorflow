---
title: "How to monitor log files for errors on Linux and Windows"
description: "Get alerted when an error, a failed login or any pattern you choose appears in a log file, and read your servers' system logs in one place."
slug: monitor-log-files
lang: en
translation: surveiller-fichiers-journaux
---

# How to monitor log files for errors on Linux and Windows

Most failures are written down before anyone notices them, in files nobody reads until something is broken. MonitorFlow counts the lines that matter and alerts you when one appears.

## Why it matters

A backup job that fails every night, a module timing out once an hour, I/O errors on a disk about to fail: each leaves a line in a file and nothing else. By the time a user complains, the log holds weeks of warnings.

## What MonitorFlow checks

The **"A log file gets a line"** check counts the new lines of a file that match a pattern, at every check. The pattern is plain text or a regular expression. The check catalogue has ready-made ones for Linux:

| Catalogue entry | File | Lines containing |
| --- | --- | --- |
| **"Errors in syslog"** | `/var/log/syslog` | `error` |
| **"Failed SSH logins"** | `/var/log/auth.log` | `Failed password` |
| **"Out of memory kills"** | `/var/log/kern.log` | `Out of memory\|oom-kill` |
| **"nginx critical errors"** | `/var/log/nginx/error.log` | `\[(crit\|alert\|emerg)\]` |

Its metric is the number of matching lines since the previous check, named after the file: a check on `/var/log/syslog` reports `log_syslog_matches`.

How it reads files:

- Only lines written after the check starts are counted, never the file's history. After an agent restart or an update, it resumes where it stopped.
- It follows a file through log rotation, renamed or truncated.
- On Linux it reads files under `/var/log` only. To watch files elsewhere, the server's administrator lists the folders in `log_dirs` under `[sensors]` in `/etc/monitorflow/agent.toml` and restarts the agent. On Windows the list is empty until someone sets it, for example `log_dirs = ['D:\Logs\MyApp']`, and gives the agent's service read access to the folder.
- The agent runs as the `monitorflow` user, which cannot read most files of `/var/log` at first. On Debian and Ubuntu, `syslog`, `auth.log` and `kern.log` belong to the `adm` group: add the user to it with `sudo usermod -aG adm monitorflow`, then `sudo systemctl restart monitorflow-agent`. The agent can then read every log of that group. Until then, the check's row stays at **"Waiting for first value"**.
- Only the counts reach the central server, not the lines themselves.

Separately, every Linux server sends the warnings and errors of its systemd journal to the central server, with nothing to set up. Read them in the server's **Logs** tab. For Windows event logs, see [How to monitor the Windows event log](monitor-windows-event-log.md).

## Set it up

1. Open **Fleet**, click the server, and stay on its **Sensors** tab.
2. Choose **"Add a check"** and type `log` in **"Find a check"**. Pick a ready-made entry, or **"A log file gets a line"** for your own file.
3. Fill in **"Log file"**, the full path, and **"Lines containing"**, the text or regular expression to look for, like `ERROR|FATAL` or `timeout after [0-9]+ ms`.
4. Leave **"Raise warning when it fails"** ticked.
5. Under **"Add to"**, choose **"This server only"** or **"Every server of"** its site, then **"Add check"**.

For a whole site, use **Settings › Sites**, then **"Alerts and checks"**, and **"Add a check"** there.

## Change the alert

By default, a single matching line raises a **warning** at once. On the **Sensors** tab, open the menu of the check's row and choose **"Edit"**:

- **"Alert"**: make it critical, for lines that mean an outage.
- **"For at least"**: a number of minutes. Each check counts the lines of its own interval, so with 5 minutes the alert opens only when matching lines keep coming for 5 minutes, and a single line is ignored.
- **"Apply to"**: this server only, or every server of the site.

The alert resolves on its own at the next check that finds no new matching line.

## Where the alert goes

Alerts show at once in **Monitoring › Incidents**, on the fleet and on the server's page. To be told, add a channel under **Settings › Notifications**: email, webhook, Slack, Teams or Discord, for everything, one customer or one site. A critical alert nobody acknowledges within 15 minutes is also told to the nearest administrators, or to whoever the **On call** page names.

## When the alert fires

The message gives how many lines matched during the last check, on which server:

```
Warning: log_syslog_matches is 3 on web-1 (paris)
```

Click **"Acknowledge"** on the alert's page, then read the lines on the server around that time: `grep -i error /var/log/syslog | tail -50`, or `Select-String -Path D:\Logs\MyApp\app.log -Pattern ERROR | Select-Object -Last 50` on Windows. On Linux, the **Logs** tab often shows the same problem from the journal's side.

If the check fires on harmless lines, make the pattern stricter rather than turning the alert off: `error` also matches `0 errors`.

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then paste the command from **Settings › Add servers** on each server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md).
