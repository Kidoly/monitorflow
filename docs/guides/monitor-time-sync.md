---
title: "How to monitor time synchronisation (NTP) on Linux and Windows"
description: "Get alerted when chrony, systemd-timesyncd or Windows Time stops, and when a server's clock drifts too far, before logins and certificates fail."
slug: monitor-time-sync
lang: en
translation: surveiller-synchronisation-heure
---

# How to monitor time synchronisation (NTP) on Linux and Windows

A server's clock drifts by a few seconds a day once nothing corrects it. For a while nothing shows. Then Kerberos logins fail, TLS certificates look not yet valid, database replicas disagree, and the logs of two machines no longer line up when you need them most. MonitorFlow tells you when the time service stops, and a short script can watch the drift itself.

## Why it matters

The time service stops after an upgrade, a configuration mistake or a conflict between two services that both want the clock. A VM restored from a snapshot or resumed after a pause also starts with the wrong time. By the time someone notices, the clock is minutes off, and the symptoms point everywhere but the clock: an Active Directory domain refuses logins at five minutes of difference by default.

## What MonitorFlow checks

| Catalogue entry | System | What it does | Alert when added |
| --- | --- | --- | --- |
| **"Time sync running"** | Linux | The systemd unit `chrony` is active. Use `chronyd` on RHEL and Rocky, or `systemd-timesyncd` where that is the time service. Metric `service_chrony_active` | Critical when it stops, for 1 minute |
| **"Windows Time running"** | Windows | The Windows service `W32Time` is running. Metric `service_w32time_active` | Critical when it stops, for 1 minute |

A missing unit or service counts as stopped, so a check for the wrong name alerts at once and shows you the mistake. MonitorFlow does not measure the clock's offset itself. A script can, as shown below.

## Set it up

1. Open **Fleet** and click the server.
2. On its **Sensors** tab, choose **"Add a check"**.
3. In **"Find a check"**, type `time` and pick **"Time sync running"** on Linux or **"Windows Time running"** on Windows.
4. Check the **"Service"** field: on Linux, `systemctl list-units --type=service | grep -E 'chrony|timesyncd'` shows the one your server runs.
5. Leave **"Raise critical when it fails"** ticked, choose **"This server only"** or **"Every server of"** its site under **"Add to"**, then **"Add check"**.

For a whole site at once, open **Settings › Sites**, click **"Alerts and checks"** under the site, and use **"Add a check"** in **"What is checked"**.

**Watching the offset with chrony.** This script reports a warning when the clock is more than 0.1 s off, critical over 1 s, and critical when chrony is not synchronised. Its `offset_seconds=` line is kept as a metric of the check:

```sh
#!/bin/sh
out=$(chronyc tracking 2>/dev/null) || { echo "UNKNOWN: chronyc failed"; exit 3; }
if printf '%s\n' "$out" | grep -q '^Leap status *: Not synchronised'; then
  echo "CRITICAL: not synchronised"; exit 2
fi
offset=$(printf '%s\n' "$out" | awk '/^System time/ {print $4}')
echo "Clock offset $offset s"
echo "offset_seconds=$offset"
awk -v o="$offset" 'BEGIN { exit (o > 1) ? 2 : (o > 0.1) ? 1 : 0 }'
```

Install it in the agent's scripts folder and try it as the agent's user:

```sh
sudo install -d -o root -g root -m 0755 /etc/monitorflow/scripts.d
sudo install -o root -g root -m 0755 check_clock /etc/monitorflow/scripts.d/
sudo -u monitorflow /etc/monitorflow/scripts.d/check_clock; echo $?
```

Then add **"Your own script"** with `check_clock` in **"Script"**. See [How to monitor with your own script](monitor-with-your-own-script.md) for the rules a script follows, on Linux and Windows.

## Change the alert

On the **Sensors** tab, open the check's row menu and choose **"Edit"**. **"Alert"** switches between critical and warning, **"For at least"** sets how many minutes the problem must last, and **"Apply to"** chooses this server or every server of the site. On a site's **"Alerts and checks"** page, the same settings are under **"When to alert"**, where the severity is called **"Raise"**.

To change the offset limits, edit the two numbers in the script's last line. A script's alert is a warning by default: choose **"Critical when it fails"** for domain controllers and database servers.

## Where the alert goes

Alerts show at once in **Monitoring › Incidents**, on the fleet and on the server's page. To be told, add a channel under **Settings › Notifications**: email, webhook, Slack, Teams or Discord, for everything, one customer or one site. A critical alert nobody acknowledges within 15 minutes is also told to the nearest administrators, or to whoever the **On call** page names.

## When the alert fires

The message names the check's metric, its value, and the server with its site:

```
Critical: service_chrony_active chrony.service is 0 on db-1 (paris)
Critical: service_w32time_active W32Time is 0 on dc-1 (paris)
Warning: script_check_clock_status is 1 on db-1 (paris)
```

By email, the subject is the same line after `[MonitorFlow]`, and the body links to the alert. Click **"Acknowledge"** on its page to tell others you are on it, then check:

1. **The service:** `systemctl status chrony` and `journalctl -u chrony -n 50`, or `Get-Service W32Time` on Windows. Two time services fighting over the clock is a common cause.
2. **Its sources:** `chronyc sources` shows whether a time server answers; `w32tm /query /status` on Windows. A firewall blocking UDP port 123 leaves the service running with nothing to follow.
3. **The offset:** `chronyc tracking`, or `w32tm /stripchart /computer:<time server> /samples:3` on Windows.

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then paste the command from **Settings › Add servers** on each server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md).
