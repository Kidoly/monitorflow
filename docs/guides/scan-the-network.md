---
title: "How to find what is on your network"
description: "Scan a range of addresses from the central server or a relay, see which machines can take the agent and which devices to watch, and add them in one click. Scans can run every day and say what is new."
slug: scan-the-network
lang: en
translation: analyser-le-reseau
---

# How to find what is on your network

Taking over a network rarely comes with a complete list of what is on it. MonitorFlow can look for you: give it a range of addresses, and it lists what answers, with what to do about each one. Nothing is added until you say so.

## What a scan does

From the **central server** or a **relay of the site** (the relay by default, since it sits on the network it scans), MonitorFlow tries each address of the range:

- one **ping**;
- a **TCP connection** to a few ports: by default 22 (SSH), 23, 80, 135, 139, 443, 445, 3389 (RDP), 5985 (WinRM), 8080 and 8443;
- **SNMP**, with the credentials of the devices you already watch on that site and any you give the scan;
- the **name** from reverse DNS and, on the probe's own network, the **MAC address**.

It is polite: at most 50 probes a second, one second of wait each, so a /24 takes about two minutes. It never goes outside the range, and it never scans names, only IP addresses.

## Start a scan

1. Open **Fleet** and click **"Scan the network"**, then **"New scan"**.
2. Give it a name and the **addresses**: `192.168.1.0/24`, a range like `192.168.1.10-50`, or single addresses, separated by commas or lines (at most 1,024 addresses).
3. Keep **Scanned from** as proposed, or pick the central server.
4. Optionally give SNMP credentials to try, and choose to **scan again** every hour, day or week.
5. Click **"Scan now"**. The page follows the scan as it goes.

## What it proposes

| What answered | Proposed |
| --- | --- |
| SSH open | **Install the agent**: a Linux or Unix server. The button opens the install command of the site |
| RDP or WinRM open | **Install the agent**: a Windows machine |
| SNMP, and it is a switch, router, firewall, printer or UPS | **Add as a device** with the checks and alerts of its kind (its template), even if SSH is open |
| SNMP otherwise | **Add as a device** with the template detection suggests, which you can change |
| Anything else | **Add as a device** with its ping and a check of each open port |

What the site already watches is listed under **Already monitored**, with how it was recognised: a device at the same address, or a server whose agent reports the same MAC address or has the same name. It is never proposed again.

Click **Add** on one line, or **"Add all"** for every proposed device. A device gets its checks from the same place as the scan. **Dismiss** stops proposing an address for this scan; **"Show dismissed"** brings it back.

## Scans that run by themselves

A scan that runs every day compares what answered with the scan before: new addresses are marked **New**, and those that stopped answering are listed as gone. Tick **"Send a notification when something new answers or something is gone"** to hear about it on the site's notification channels. It is off by default.

## Good to know

- A relay that is not connected keeps its scan waiting, and starts it when it comes back.
- A relay older than this feature says it does not run scans yet: update it, as on the Relays page.
- MAC addresses, and so recognising a server by them, are only seen on the probe's own network.
- Through the API, everything here is under `/api/v1/network-scans` (see the API documentation in Settings › API keys).
