---
title: "How to monitor a website over HTTPS"
description: "Know when your website stops answering as it should: its status code, the text on the page, how long it takes, its redirects and when its certificate expires."
slug: monitor-website-https
lang: en
translation: surveiller-site-web-https
---

# How to monitor a website over HTTPS

A website can be down while its server looks fine: the application answers `502 Bad Gateway`, a deploy left an error page, a redirect loops, or the certificate expired overnight. MonitorFlow requests the page the way a visitor does and tells you what is wrong, from the web server itself or from another machine.

## Why it matters

Visitors do not report a down site; they leave. An expired certificate is worse: browsers show a full-page warning, usually on a weekend, because renewal failed quietly weeks before. Checking from outside also catches a network or firewall change in front of the server.

## What MonitorFlow checks

| Catalogue entry | What it does | Alerts by default |
| --- | --- | --- |
| **"A website answers"** | Requests the page: its status code, how long it takes, its redirects and, over https, its certificate | Critical when it fails for 2 minutes |
| **"A page shows the right content"** | The same, with a text the page must hold (a product name) or must not hold (an error message) | The same |
| **"A certificate does not expire"** | The same, for an `https://` address | The same |
| **"An API answers"** | The same, with the status code expected, extra headers and a password or token | The same |

Every website check comes with these alerts, which you can change like any other:

- critical when the page does not answer as expected for 2 minutes: no answer, a status code not expected, the text missing, too many redirects;
- critical when the certificate is not valid for 5 minutes: expired, self-signed, signed by an authority the checking machine does not trust, or for another name;
- warning when the certificate has fewer than 14 days left, critical under 7;
- warning when the page takes more than 3 seconds for 5 minutes.

An expired certificate does not hide the rest: the page is still read, so you see both its status code and what is wrong with the certificate.

The check runs on the agent of the server you add it to, unless **"Checked from"** names the central server or a relay of the site. Add it to the web server to check the site as it serves it, to another server to check it across the network, or have the central server or a relay check it from outside, like a visitor. A device, which has no agent, gets website checks too, run from the central server or a relay. Each server can limit where its checks connect with `port_targets` in its `agent.toml`, redirects included; anywhere by default. The central server never checks a link-local address (`169.254.x.x`, `fe80::`), where cloud machines serve their own credentials, even after a redirect.

## Set it up

1. Open **Fleet** and click the server that runs the check: the web server, or another one that reaches the site.
2. On its **"Sensors"** tab, choose **"Add a check"**.
3. In **"Find a check"**, type `website` and pick **"A website answers"**.
4. In **"Page address"**, enter the address as a visitor types it, like `https://www.example.com/`. In **"Text the page must hold"**, enter a text that only shows when the page works, like `Add to cart`, or leave it empty.
5. Keep **"Raise critical when it fails"** ticked, choose **"This server only"** under **"Add to"**, and click **"Add check"**.

**"More options"** holds the rest: a text the page must not hold, the status codes that pass (`200-299` by default), whether redirects are followed, `GET` or `HEAD`, IPv4 or IPv6, extra headers (sent only to the host of the address, never over http after https), and a user name and password for a page behind a password. The password is sent over https only, kept sealed and never shown again; without a user name, it is sent as a bearer token, as many APIs want.

The **"Websites"** list of the **"Sensors"** tab then shows each site: whether it answers, why not, its status code, its response time, where its redirects end, and its certificate with its days left, its expiry and who issued it.

## Change the alert

On the **"Sensors"** tab, open the row's menu and choose **"Edit"**. **"Alert"** chooses critical or warning, **"For at least"** how many minutes the problem must last (`0` alerts on the first bad value), and **"Apply to"** this server only or every server of the site. The certificate's days left and the response time are edited as numbers: warn under 30 days rather than 14, for example.

## Where the alert goes

Every alert shows in **Monitoring › Incidents** and on the fleet. To be told, add a channel under **Settings › Notifications**: email, webhook, Slack, Teams or Discord, for everything, one customer or one site; **"Send test"** checks it. A critical alert nobody acknowledges within 15 minutes also goes to the nearest administrators who set how they are reached on the **On call** page.

## When the alert fires

```
Critical: http_www_example_com_up is 0 on web-1 (paris)
Warning: http_www_example_com_cert_days_left is 12 on web-1 (paris)
```

By email, the subject is this line after `[MonitorFlow]`, and the body links to the alert. Click **"Acknowledge"**, then check:

1. **From where it fails:** if only the check from another server fails, look at the network, firewall or DNS between them.
2. **The web server:** see [nginx](monitor-nginx.md) or [Apache](monitor-apache.md).
3. **What it says:** the **"Websites"** list says why the last run failed, like "Answered 503, which is not expected", and what is wrong with the certificate. Check that renewal runs and that the web server was reloaded after it.

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then open **Settings › Add servers** in the web interface and paste the command it shows on each server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md).
