---
title: "How to get MonitorFlow alerts on your phone"
description: "Install MonitorFlow on your phone's home screen, get push notifications when you are on call, and acknowledge or silence an alert in two taps, without a store or an outside service."
slug: alerts-on-your-phone
lang: en
translation: alertes-sur-votre-telephone
---

# How to get MonitorFlow alerts on your phone

When you are on call, the alert should wake you on your phone, and dealing with it should take two taps: say you are on it, or silence it for an hour. MonitorFlow's web interface installs on a phone like an app, keeps you signed in for weeks, and sends push notifications through your phone's own push service. No store, no account with us, no relay of ours.

## What you get

- **Notifications** on Android (Chrome, Firefox, Edge) and on iPhone and iPad (iOS and iPadOS 16.4 or later), whenever you would get an email or chat message as the person on call: during your turns, when a follow-up names you, and as an administrator during a gap of a place you administer.
- **Acknowledge** from the notification itself on Android; on iOS, a tap opens the alert with its Acknowledge button.
- **Silence** for 30 minutes, 1, 4 or 24 hours: the alert's check only, or its whole server.
- **Pages made for a phone:** open alerts, one alert, who is on call now and next, and your phones.
- **A long session:** 90 days, or 30 days unused, on that phone only. Remove the phone and it is signed out at once.

## Before you start

- The central server must reach your phone's push service over **outbound HTTPS** (port 443): `web.push.apple.com` and its subdomains for iPhone and iPad, `fcm.googleapis.com` for Chrome on Android, `updates.push.services.mozilla.com` for Firefox, `*.notify.windows.com` for Edge. It connects to them directly, not through a proxy. Without this, everything below works except the notifications themselves, and the phone's page says the push service is unreachable.
- Your phone must open the web interface's address, on the local network, a VPN or the Internet, and trust its certificate (see the trust page of your server).
- To be told, you need to be on an on-call schedule (Monitoring › On call). A team where everyone wants every alert can make one schedule with all of them "on call at once".

## Set it up

1. On the phone, open MonitorFlow's address and sign in, with your password or your sign-in provider.
2. Add it to the home screen:
   - **iPhone, iPad:** Share, then **Add to Home Screen**. Open MonitorFlow from the home screen from then on: iOS sends notifications only there.
   - **Android:** the browser's menu, then **Install app** or **Add to Home screen**.
3. Open the app and go to **This phone**. Check its name and tap **Add this phone**.
4. Tap **Turn on notifications** and allow them when the phone asks.
5. Tap **Send test**. A notification should show within seconds.

The **On call** page of the full interface says, under "How you are reached", how many of your phones get notifications, and its **Send test** tests them too.

## What a notification looks like

> **MonitorFlow**
> Critical: disk_used_percent /var is 97.25 on web-1 (paris)
> [Acknowledge] [Silence…]

It says what and where, never a password or a link that acts by itself. Several alerts sent together are one notification that opens their list, and you get at most three per minute: the third says how many more there are. When the alert resolves, its notification is replaced by the resolution.

- **Acknowledge** tells the server you are on it, with your phone's session: nobody else is told next. If the phone was signed out, the alert opens so you can sign in.
- **Silence…** opens the alert with the choices: this check or the whole server, and for how long. The alert stays open and visible; notifications about it stop until the end, and what is still open then is sent.

## Your phones

**This phone** lists every phone you added, when it was last used, and whether its notifications are on. **Remove** signs that phone out and stops its notifications; do it from another phone if one is lost. An administrator who disables your account ends every session at once, phones included, and changing your password signs your other sessions out, phones too.

## When it does not work

- **"Add MonitorFlow to the Home Screen first"**: on iPhone and iPad, Safari offers notifications only to the app opened from the home screen.
- **"Notifications are blocked"**: allow them for MonitorFlow in the phone's settings (or the site's settings of the browser), then tap Turn on notifications again.
- **"Could not connect to …push…" or "No answer from …"**: the central server cannot reach the push service; open outbound HTTPS to it in the firewall.
- **"The push service no longer knows this phone"**: the browser dropped its subscription (app data cleared, reinstalled). Open the app on that phone and turn notifications on again.
- **Nothing arrives, no error**: check that you are on call now (On call tab), that a silence does not cover the alert, and that the phone is not in a focus or do-not-disturb mode.

## For administrators

The server signs its notifications with its own key, made on first use and kept sealed in the database, so every central server of a cluster sends with the same one. Nothing is sent through a service of ours: Apple, Google, Mozilla or Microsoft carry an encrypted message they cannot read. Every change to a phone is in the audit log (`user.phone_add`, `user.phone_push_on`, `user.phone_push_off`, `user.phone_rename`, `user.phone_remove`), and so are acknowledgements and silences, as from anywhere. Native applications in the stores are planned; the design is in [ADR-0186](../adr/0186-phones-and-push-notifications.md).
