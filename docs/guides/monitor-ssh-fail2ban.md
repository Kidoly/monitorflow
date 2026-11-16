---
title: "How to monitor SSH and fail2ban on Linux"
description: "Get alerted when the SSH server stops, port 22 no longer answers, fail2ban stops banning, or failed logins pile up on a Linux server."
slug: monitor-ssh-fail2ban
lang: en
translation: surveiller-ssh-fail2ban
---

# How to monitor SSH and fail2ban on Linux

SSH is how you reach a Linux server when something goes wrong. When it stops, you find out at the worst moment, in the middle of an incident, with only a console left. fail2ban, which bans addresses that guess passwords, fails just as quietly. MonitorFlow watches both, and counts failed logins.

## Why it matters

A bad `sshd_config` after an upgrade, or a removed package, stops the SSH server. Nothing else breaks, so nobody notices until they need to log in. fail2ban stops when a filter no longer parses, and brute-force attempts then run unchecked.

## What MonitorFlow checks

Four entries of the check catalogue cover this, all on Linux:

| Catalogue entry | What it does | Alert when added |
| --- | --- | --- |
| **"SSH server running"** | The systemd unit `ssh` (Debian, Ubuntu) or `sshd` (RHEL, Rocky) is active. Metric `service_ssh_active` or `service_sshd_active` | Critical when it stops, for 1 minute |
| **"SSH answers"** | Port 22 accepts TCP connections. Metric `port_22_up` | Critical when it fails, for 1 minute |
| **"fail2ban running"** | The unit `fail2ban` is active. Metric `service_fail2ban_active` | Critical when it stops, for 1 minute |
| **"Failed SSH logins"** | Counts lines with `Failed password` in `/var/log/auth.log`. Metric `log_auth_matches` | Warning on a matching line, at once |

A missing unit counts as stopped. A port check connects from the agent, to `127.0.0.1` by default. On Ubuntu 22.10 and later, SSH starts on demand from `ssh.socket` and `ssh.service` may be inactive until the first login: enter `ssh.socket` as the service there.

The log check counts lines written after it starts, and needs a file to read. RHEL and Rocky write these lines to `/var/log/secure`; a system without rsyslog keeps them only in the journal, with no `auth.log`. The agent's user must be able to read the file. On Debian and Ubuntu, `auth.log` belongs to the `adm` group, which reads every log in `/var/log`: `sudo usermod -aG adm monitorflow`, then `sudo systemctl restart monitorflow-agent`. Without access, the check reports "access denied" and tries again every five minutes.

## Set it up

1. Open **Fleet** and click the server.
2. On its **Sensors** tab, choose **"Add a check"**.
3. In **"Find a check"**, type `ssh` and pick **"SSH server running"**. Check the **"Service"** field: `ssh`, `sshd` or `ssh.socket`.
4. Leave **"Raise critical when it fails"** ticked.
5. Under **"Add to"**, choose **"This server only"** or **"Every server of"** its site, then **"Add check"**.
6. Do the same for **"SSH answers"**, **"fail2ban running"** and **"Failed SSH logins"**, whose **"Log file"** is `/var/log/secure` on RHEL and Rocky.

For a whole site at once, open **Settings › Sites**, click **"Alerts and checks"** under the site, and use **"Add a check"** in **"What is checked"**.

## Change the alert

On the **Sensors** tab, open the check's row menu and choose **"Edit"**. **"Alert"** switches between critical and warning, **"For at least"** sets how many minutes the problem must last, and **"Apply to"** chooses this server or every server of the site. On a site's **"Alerts and checks"** page, the same settings are under **"When to alert"**, where the severity is called **"Raise"**.

On a server open to the internet, failed logins never stop, and a warning for each one is noise. Set **"For at least"** to 10 or 15 minutes: the alert then opens only when attempts keep coming, at every check, for that long.

## Where the alert goes

Alerts show at once in **Monitoring › Incidents**, on the fleet and on the server's page. To be told, add a channel under **Settings › Notifications**: email, webhook, Slack, Teams or Discord, for everything, one customer or one site. A critical alert nobody acknowledges within 15 minutes is also told to the nearest administrators, or to whoever the **On call** page names.

## When the alert fires

The message names the check's metric, its value, and the server with its site:

```
Critical: service_ssh_active ssh.service is 0 on web-1 (paris)
Critical: port_22_up is 0 on web-1 (paris)
Warning: log_auth_matches is 14 on web-1 (paris)
```

By email, the subject is the same line after `[MonitorFlow]`, and the body links to the alert. Click **"Acknowledge"** on its page to tell others you are on it, then check:

1. **The service:** from a console, `systemctl status ssh` and `journalctl -u ssh -n 50`. After a configuration change, `sshd -t` shows the error.
2. **The port, if the service runs:** `ss -ltnp | grep :22`.
3. **fail2ban:** `systemctl status fail2ban` and `fail2ban-client status sshd`.
4. **Failed logins:** `grep "Failed password" /var/log/auth.log | tail` shows the addresses.

To count other lines, see [How to monitor log files](monitor-log-files.md).

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then paste the command from **Settings › Add servers** on each server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md).
