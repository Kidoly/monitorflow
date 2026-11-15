---
title: "How to monitor anything with your own script (Bash or PowerShell)"
description: "Turn any shell or PowerShell script into a check with an alert: exit codes for OK, warning and critical, and name=value lines as metrics."
slug: monitor-with-your-own-script
lang: en
translation: surveiller-avec-votre-script
---

# How to monitor anything with your own script (Bash or PowerShell)

Some things only you know how to check: that last night's backup exists, that an internal API returns the right answer. MonitorFlow runs a script you install on the server and turns its result into an alert. Checks written in the usual monitoring plugin format work as they are.

## Why it matters

Built-in checks see that a service runs and a port answers. They cannot see that the backup job wrote an empty file, or that a queue has grown for hours. A ten-line script that tests the real result catches it.

## What MonitorFlow checks

The **"A script reports OK"** check, listed as **"Your own script"** in the check catalogue, runs a script at every check and reads:

- **Its exit code**, as its status (`script_…_status`): `0` OK, `1` warning, `2` critical, `3` unknown. A script that times out, is killed, exits with another code or writes more than 64 KiB counts as `3`.
- **Lines of the form `name=value`** on its standard output, each stored as a metric of the server, like `age_seconds=5400`. Names use lower-case letters, digits and `_`, start with a letter, and at most 32 are kept. Other lines, like a summary on the first line, are ignored.
- **How long it took**, as `script_…_duration_seconds`.

The check is named after the file: `check_backup.sh` gives `script_check_backup_status`. You can chart the `name=value` metrics on a dashboard.

For safety, the central server can only name a script; it can never send one. The agent runs only files an administrator installed in its scripts folder:

- **Linux**: `/etc/monitorflow/scripts.d`. The folder and the file belong to root (or the agent's user) and are not writable by others. The file is executable and starts with `#!`, or is a compiled program. It runs as the agent's user, `monitorflow`, with a minimal `PATH`, from `/`, with no input.
- **Windows**: `C:\ProgramData\MonitorFlow\scripts.d`, a `.ps1` run by Windows PowerShell 5.1 or an `.exe`. Batch files are refused. It runs as the agent's service account.

The central server sends no arguments unless the server's administrator allows them, so write the values a script needs inside it.

## Set it up

Write the script. This one checks that a backup is less than 26 hours old:

```sh
#!/bin/sh
newest=$(find /srv/backup -type f -printf '%T@\n' 2>/dev/null | sort -n | tail -1)
[ -z "$newest" ] && { echo "No backup found"; exit 2; }
age=$(( $(date +%s) - ${newest%.*} ))
echo "Newest backup is $((age / 3600)) h old"
echo "age_seconds=$age"
[ "$age" -lt 93600 ] && exit 0
[ "$age" -lt 172800 ] && exit 1
exit 2
```

Install it as root, check that the `monitorflow` user can read what it tests, and try it with the agent's own checks:

```sh
install -d -o root -g root -m 0755 /etc/monitorflow/scripts.d
install -o root -g root -m 0755 check_backup.sh /etc/monitorflow/scripts.d/
monitorflow-agent sensors probe script --param script=check_backup.sh
```

On Windows, copy a `.ps1` into `C:\ProgramData\MonitorFlow\scripts.d` from an elevated PowerShell, end it with `exit 0`, `1`, `2` or `3`, and catch errors yourself: an uncaught error exits with 1, a warning.

Then, in the web interface:

1. Open **Fleet**, click the server, and stay on its **Sensors** tab.
2. Choose **"Add a check"**, type `script` in **"Find a check"**, and pick **"Your own script"**.
3. In **"Script"**, enter the file name only: `check_backup.sh`. Choose **"How often"**: every hour is plenty for a daily backup.
4. Leave **"Raise warning when it fails"** ticked, choose **"This server only"** or **"Every server of"** its site, and **"Add check"**.

A packaged plugin works too. The catalogue's **"Pending updates (check_apt)"** needs `check_apt` linked into the folder first: `ln -s /usr/lib/nagios/plugins/check_apt /etc/monitorflow/scripts.d/`. Link plugins one by one, never the whole folder.

If the check's row stays at **"Waiting for first value"**, the agent refused the script: it is missing, writable by others, or has no `#!` line. `sensors probe` gives the reason. Fix it on the server: the agent tries again within 5 minutes.

## Change the alert

By default, any status other than OK for one minute raises a **warning**. On the **Sensors** tab, open the menu of the check's row and choose **"Edit"**: **"Alert"** makes it critical, **"For at least"** sets how long the status must last, **"Timeout"** gives a slow script more time, and **"Apply to"** chooses this server or the whole site.

## Where the alert goes

Alerts show at once in **Monitoring › Incidents**, on the fleet and on the server's page. To be told, add a channel under **Settings › Notifications**: email, webhook, Slack, Teams or Discord, for everything, one customer or one site. A critical alert nobody acknowledges within 15 minutes is also told to the nearest administrators, or to whoever the **On call** page names.

## When the alert fires

The message gives the status code and the server:

```
Warning: script_check_backup_status is 2 on files-1 (paris)
```

Click **"Acknowledge"** on the alert's page, then run the script by hand with `monitorflow-agent sensors probe`, as above, to see its output. A status of `3` usually means the script itself failed: a missing command in the agent's minimal `PATH`, a file the agent's user cannot read, or a timeout.

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then paste the command from **Settings › Add servers** on each server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md).
