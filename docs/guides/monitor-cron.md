---
title: "How to monitor cron and scheduled jobs on Linux"
description: "Get alerted when cron stops on a Linux server, and when a nightly job such as a backup has not produced its result in time."
slug: monitor-cron
lang: en
translation: surveiller-cron
---

# How to monitor cron and scheduled jobs on Linux

Backups, certificate renewals, log cleanups and reports all run from cron. When cron stops, or when one job quietly fails every night, nothing tells you: there is no error on screen, only a result that is missing the day you need it. MonitorFlow watches the cron service, and with a short script, the result of the jobs that matter.

## Why it matters

Cron can stop after an upgrade, a removed package or a mistake during maintenance. More often, cron runs fine and one job fails: a full disk, an expired password, a moved path, a script that exits early. Its email goes to a mailbox nobody reads, or nowhere. You find out when you need last night's backup and the newest one is three weeks old.

## What MonitorFlow checks

| Catalogue entry | What it does | Alert when added |
| --- | --- | --- |
| **"Cron running"** | The systemd unit `cron` (Debian, Ubuntu) or `crond` (RHEL, Rocky) is active. Metric `service_cron_active` or `service_crond_active` | Critical when it stops, for 1 minute |
| **"Your own script"** | Runs a script you install on the server and reads its exit code: 0 OK, 1 warning, 2 critical, 3 unknown. Metric `script_<name>_status` | Warning when it reports a problem, for 1 minute |

A missing unit counts as stopped. The service check tells you cron runs; it cannot tell whether a job did its work. For that, check the job's result: the age of the file it writes.

## Set it up

**The cron service:**

1. Open **Fleet** and click the server.
2. On its **Sensors** tab, choose **"Add a check"**.
3. In **"Find a check"**, type `cron` and pick **"Cron running"**. Set **"Service"** to `cron` or `crond`.
4. Leave **"Raise critical when it fails"** ticked, choose **"This server only"** or **"Every server of"** its site under **"Add to"**, then **"Add check"**.

For a whole site at once, open **Settings › Sites**, click **"Alerts and checks"** under the site, and use **"Add a check"** in **"What is checked"**.

**A job's result.** This script reports critical when no file in the backup folder is newer than 26 hours, which leaves a nightly job two hours of slack:

```sh
#!/bin/sh
dir=/srv/backup
if [ -n "$(find "$dir" -type f -mmin -1560 2>/dev/null | head -n 1)" ]; then
  echo "OK: a backup is less than 26 hours old"
  exit 0
fi
echo "CRITICAL: no backup in $dir for 26 hours"
exit 2
```

Install it as root in the agent's scripts folder, which only root may write:

```sh
sudo install -d -o root -g root -m 0755 /etc/monitorflow/scripts.d
sudo install -o root -g root -m 0755 check_backup_age /etc/monitorflow/scripts.d/
sudo -u monitorflow /etc/monitorflow/scripts.d/check_backup_age; echo $?
```

The second line runs it as the agent's user, who must be able to list the folder. Then, on the **Sensors** tab, **"Add a check"**, pick **"Your own script"**, enter `check_backup_age` in **"Script"**, and choose **"How often"**: every 15 minutes or every hour is plenty. [How to monitor with your own script](monitor-with-your-own-script.md) explains the rules a script must follow.

## Change the alert

On the **Sensors** tab, open the check's row menu and choose **"Edit"**. **"Alert"** switches between critical and warning, **"For at least"** sets how many minutes the problem must last, and **"Apply to"** chooses this server or every server of the site. On a site's **"Alerts and checks"** page, the same settings are under **"When to alert"**, where the severity is called **"Raise"**.

A script's alert is a warning by default; a missing backup deserves **"Critical when it fails"**. With a check every hour, the alert opens at the second failed run. Set **"For at least"** to 0 to alert on the first one.

## Where the alert goes

Alerts show at once in **Monitoring › Incidents**, on the fleet and on the server's page. To be told, add a channel under **Settings › Notifications**: email, webhook, Slack, Teams or Discord, for everything, one customer or one site. A critical alert nobody acknowledges within 15 minutes is also told to the nearest administrators, or to whoever the **On call** page names.

## When the alert fires

The message names the check's metric, its value, and the server with its site:

```
Critical: service_cron_active cron.service is 0 on db-1 (paris)
Critical: script_check_backup_age_status is 2 on db-1 (paris)
```

By email, the subject is the same line after `[MonitorFlow]`, and the body links to the alert. Click **"Acknowledge"** on its page to tell others you are on it, then check:

1. **Cron:** `systemctl status cron` (or `crond`) and `journalctl -u cron -n 50`.
2. **The job:** `grep CRON /var/log/syslog | tail` on Debian and Ubuntu, or `journalctl -u cron`, shows whether it started. Run its command by hand as its user to see the error.
3. **Its target:** a full disk, a share that is not mounted or a changed password are the usual causes. See [How to monitor disk space](monitor-disk-space.md).

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then paste the command from **Settings › Add servers** on each server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md).
