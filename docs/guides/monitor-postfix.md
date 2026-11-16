---
title: "How to monitor Postfix on Linux"
description: "Get alerted when Postfix stops, port 25 no longer answers, or the mail queue grows, so mail from your servers and applications keeps flowing."
slug: monitor-postfix
lang: en
translation: surveiller-postfix
---

# How to monitor Postfix on Linux

Postfix carries the mail your servers and applications send: password resets, invoices, alerts, cron reports. When it stops, or when it runs but cannot deliver, messages pile up in its queue without a word. Users notice first, when the email they wait for never comes. MonitorFlow watches the service, the SMTP port, and with a short script, the queue.

## Why it matters

Postfix stops after an upgrade or a configuration error. More often it runs and delivers nothing: the relay host refuses its password, a DNS change breaks delivery, the server's address lands on a blocklist, or the disk fills. Each time, mail waits in the queue and Postfix retries for days before it bounces. A queue that grows is the earliest sign, and nobody looks at it.

## What MonitorFlow checks

| Catalogue entry | What it does | Alert when added |
| --- | --- | --- |
| **"Postfix running"** | The systemd unit `postfix` is active. Metric `service_postfix_active` | Critical when it stops, for 1 minute |
| **"Mail server answers (SMTP)"** | Port 25 accepts TCP connections. Metric `port_25_up` | Critical when it fails, for 1 minute |
| **"Your own script"** | Runs a script you install and reads its exit code: 0 OK, 1 warning, 2 critical, 3 unknown. Metric `script_<name>_status` | Warning when it reports a problem, for 1 minute |

A missing unit counts as stopped. The port check connects from the agent to `127.0.0.1` by default, which suits Postfix listening on all addresses or on loopback only. To check another mail server, enter its name in **"Address"**.

## Set it up

1. Open **Fleet** and click the server.
2. On its **Sensors** tab, choose **"Add a check"**.
3. In **"Find a check"**, type `postfix` and pick **"Postfix running"**.
4. Leave **"Raise critical when it fails"** ticked, choose **"This server only"** or **"Every server of"** its site under **"Add to"**, then **"Add check"**.
5. Do the same with **"Mail server answers (SMTP)"**.

For a whole site at once, open **Settings › Sites**, click **"Alerts and checks"** under the site, and use **"Add a check"** in **"What is checked"**.

**The mail queue.** This script counts the messages in the queue, warns over 50 and reports critical over 500. Its `queue=` line is kept as a metric of the check. It uses `postqueue -j`, Postfix 3.1 or later, which any user may run unless `authorized_mailq_users` says otherwise:

```sh
#!/bin/sh
out=$(postqueue -j 2>/dev/null) || { echo "UNKNOWN: postqueue failed"; exit 3; }
count=$(printf '%s' "$out" | grep -c .)
echo "$count messages in the queue"
echo "queue=$count"
[ "$count" -gt 500 ] && exit 2
[ "$count" -gt 50 ] && exit 1
exit 0
```

Install it in the agent's scripts folder, which only root may write, and try it as the agent's user:

```sh
sudo install -d -o root -g root -m 0755 /etc/monitorflow/scripts.d
sudo install -o root -g root -m 0755 check_mailq /etc/monitorflow/scripts.d/
sudo -u monitorflow /etc/monitorflow/scripts.d/check_mailq; echo $?
```

Then add **"Your own script"** with `check_mailq` in **"Script"**, every 5 minutes. See [How to monitor with your own script](monitor-with-your-own-script.md) for the rules a script follows.

## Change the alert

On the **Sensors** tab, open the check's row menu and choose **"Edit"**. **"Alert"** switches between critical and warning, **"For at least"** sets how many minutes the problem must last, and **"Apply to"** chooses this server or every server of the site. On a site's **"Alerts and checks"** page, the same settings are under **"When to alert"**, where the severity is called **"Raise"**.

To change the queue limits, edit the two numbers in the script. A short burst of mail can fill the queue for a minute; set **"For at least"** to 15 minutes to be told only when it does not drain.

## Where the alert goes

Alerts show at once in **Monitoring › Incidents**, on the fleet and on the server's page. To be told, add a channel under **Settings › Notifications**: email, webhook, Slack, Teams or Discord, for everything, one customer or one site. A critical alert nobody acknowledges within 15 minutes is also told to the nearest administrators, or to whoever the **On call** page names.

An email channel that goes through the Postfix you are watching cannot tell you it stopped: add a chat channel or a webhook too.

## When the alert fires

The message names the check's metric, its value, and the server with its site:

```
Critical: service_postfix_active postfix.service is 0 on mail-1 (paris)
Critical: port_25_up is 0 on mail-1 (paris)
Warning: script_check_mailq_status is 1 on mail-1 (paris)
```

By email, the subject is the same line after `[MonitorFlow]`, and the body links to the alert. Click **"Acknowledge"** on its page to tell others you are on it, then check:

1. **The service:** `systemctl status postfix` and `journalctl -u postfix -n 50`. `postfix check` reports configuration errors.
2. **The queue:** `mailq` lists the waiting messages with the reason each one waits, such as a refused login or a host not found.
3. **Delivery:** `grep "status=deferred" /var/log/mail.log | tail` on Debian and Ubuntu, or `/var/log/maillog` on RHEL and Rocky. Once fixed, `postqueue -f` retries the queue.

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then paste the command from **Settings › Add servers** on each server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md).
