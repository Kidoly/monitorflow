---
title: "How to monitor a website over HTTPS"
description: "Know when your website stops answering on port 443 or 80, and check its status code and certificate expiry with a short script."
slug: monitor-website-https
lang: en
translation: surveiller-site-web-https
---

# How to monitor a website over HTTPS

A website can be down while its server looks fine: the web server stopped, a firewall rule changed, or the certificate expired overnight. MonitorFlow checks that the site's ports answer, from the web server or from another machine, and a short script adds the status code and the certificate's days left.

## Why it matters

Visitors do not report a down site; they leave. An expired certificate is worse: browsers show a full-page warning, usually on a weekend, because renewal failed quietly weeks before. Checking from outside also catches a network or firewall change in front of the server.

## What MonitorFlow checks

| Catalogue entry | What it does | Alert by default |
| --- | --- | --- |
| **"Web server answers (HTTPS)"** | Port 443 accepts TCP connections. Metric `port_443_up` | Critical when it fails, for 1 minute |
| **"Web server answers (HTTP)"** | Port 80 accepts TCP connections. Metric `port_80_up` | Critical when it fails, for 1 minute |
| **"Your own script"** | Runs a script you install, read as OK, warning or critical | Warning when it reports a problem, for 1 minute |

A port check opens a TCP connection, nothing more. It sends no request and reads neither the status code nor the certificate: a site answering `502 Bad Gateway` or with an expired certificate still passes. The script below covers that.

A port check connects to `127.0.0.1` by default. In **"Address"**, you can give the site's name instead and add the check on another server, whose agent then connects across the network like a visitor. Each server can limit where its port checks connect with `port_targets` in its `agent.toml`; anywhere by default.

## Set it up

1. Open **Fleet** and click the server that runs the check: the web server, or another one that reaches the site.
2. On its **"Sensors"** tab, choose **"Add a check"**.
3. In **"Find a check"**, type `https` and pick **"Web server answers (HTTPS)"**.
4. **"Port"** holds `443`. In **"Address"**, keep `127.0.0.1` or enter the site's name, like `www.example.com`.
5. Keep **"Raise critical when it fails"** ticked, choose **"This server only"** under **"Add to"**, and click **"Add check"**.

### Status code and certificate, with a script

The agent runs only scripts installed in `/etc/monitorflow/scripts.d`, and passes no arguments unless the server allows it, so write the address into the script. Save this as `check_https.sh`, changing the first two lines:

```sh
#!/bin/sh
URL=https://www.example.com/
HOST=www.example.com

code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 "$URL")
end=$(timeout 5 openssl s_client -connect "$HOST:443" -servername "$HOST" </dev/null 2>/dev/null \
  | openssl x509 -noout -enddate | cut -d= -f2)
[ -n "$end" ] || { echo "CRITICAL: no certificate from $HOST"; exit 2; }
days=$(( ($(date -d "$end" +%s) - $(date +%s)) / 86400 ))
echo "http_code=$code"
echo "cert_days=$days"
[ "$code" = 200 ] || exit 2
[ "$days" -ge 7 ] || exit 2
[ "$days" -ge 21 ] || exit 1
exit 0
```

Exit code 0 is OK, 1 a warning, 2 critical. curl refuses an invalid certificate, which gives code `000` and a critical result. Each `name=value` line becomes a metric: `script_check_https_http_code` and `script_check_https_cert_days`. Install and try it:

```sh
sudo install -d -o root -g root -m 0755 /etc/monitorflow/scripts.d
sudo install -o root -g root -m 0755 check_https.sh /etc/monitorflow/scripts.d/
sudo monitorflow-agent sensors probe script --param script=check_https.sh
```

Then **"Add a check"**, pick **"Your own script"**, enter `check_https.sh` in **"Script"**, choose every 15 minutes under **"How often"**, and click **"Add check"**. More in [How to monitor with your own script](monitor-with-your-own-script.md).

## Change the alert

On the **"Sensors"** tab, open the row's menu and choose **"Edit"**. **"Alert"** chooses critical or warning, **"For at least"** how many minutes the problem must last (`0` alerts on the first bad value, best for a script run every 15 minutes), and **"Apply to"** this server only or every server of the site.

## Where the alert goes

Every alert shows in **Monitoring › Incidents** and on the fleet. To be told, add a channel under **Settings › Notifications**: email, webhook, Slack, Teams or Discord, for everything, one customer or one site; **"Send test"** checks it. A critical alert nobody acknowledges within 15 minutes also goes to the nearest administrators who set how they are reached on the **On call** page.

## When the alert fires

```
Critical: port_443_up is 0 on web-1 (paris)
Warning: script_check_https_status is 1 on web-1 (paris)
```

By email, the subject is this line after `[MonitorFlow]`, and the body links to the alert. Click **"Acknowledge"**, then check:

1. **From where it fails:** if only the check from another server fails, look at the network, firewall or DNS between them.
2. **The web server:** see [nginx](monitor-nginx.md) or [Apache](monitor-apache.md).
3. **The certificate:** run the script by hand to see `http_code` and `cert_days`; check that renewal runs and the web server was reloaded after it.

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then open **Settings › Add servers** in the web interface and paste the command it shows on each server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md).
