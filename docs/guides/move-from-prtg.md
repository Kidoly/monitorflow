---
title: "How to move from PRTG to MonitorFlow"
description: "Read your PRTG devices, sensors, limits, maintenance windows and notifications, preview what moves, and create the matching servers, checks, thresholds, silences and channels in one click."
slug: move-from-prtg
lang: en
translation: migrer-depuis-prtg
---

# How to move from PRTG to MonitorFlow

Retyping hundreds of devices, sensors and limits is the hardest part of switching tools. MonitorFlow reads your PRTG configuration, through its API or from its configuration file, and shows what it would create before creating anything. PRTG is only read: nothing is changed in it, and it can keep running beside MonitorFlow until you switch.

## What moves, and how

| In PRTG | In MonitorFlow | How faithfully |
| --- | --- | --- |
| Device, with its address | Server waiting for its agent, in the site you choose | Exact. The device's groups are kept as its groups |
| Probe and group | Groups of the servers below them | Exact |
| Ping sensor | The offline alert every server has | Approximated: MonitorFlow knows a server is down when its agent stops answering; ping times are not measured |
| CPU load sensor, with limits on its Total channel | CPU usage threshold of the server | Exact |
| Memory sensor, with limits on its percent available channel | Available memory threshold | Exact |
| Disk free and volume sensors, with limits on a percent free channel | Disk space threshold for that drive or mount point | Approximated: read as space used |
| Windows service sensor | Windows service check, critical when it stops | Exact |
| Port sensor | Port check of the device's address | Approximated: checked by the server's own agent, not from a probe |
| HTTP and HTTP advanced sensors | Port check of the page's host and port | Approximated: the page, its status code and load time are not checked |
| SSL certificate sensor | Port check of its host and port | Approximated: the certificate and its expiry are not checked |
| Event log sensors | Event log check of the same log, levels, sources and event IDs | Exact, or approximated when a message filter or a count limit cannot be moved |
| Maintenance window | Silence of the servers it covers | Exact; at most 30 days |
| Paused device, group or probe | Silence of 30 days, the longest there is | Approximated |
| Notification template sending email | Email channel | Approximated: it gets every incident, where PRTG used it only where a trigger named it. Its mail server is not in PRTG's API: add an email channel with yours if the preview says so |
| Notification template calling a URL | Slack, Teams, Discord or webhook channel, by the URL | Approximated: MonitorFlow sends its own message |

What cannot be moved is listed too, with the reason: SNMP traffic sensors, devices watched only over SNMP (switches, printers), sensors of a kind MonitorFlow has no check for, limits on channels MonitorFlow has no threshold for (response times, certificate days left), sensors paused by a schedule or a dependency, a single paused sensor (MonitorFlow silences whole servers), and SMS or push notifications.

## Before you start

You need to be an administrator of the site the servers go to, and one of:

- **An API token** of a PRTG user who can see the devices to move. In PRTG, open **Setup › Account Settings › API Keys** and add a key with read access.
- **A user name and its passhash.** In PRTG, open **Setup › Account Settings › My Account** and click **Show passhash**.
- **The configuration file**, `PRTG Configuration.dat`, copied from `C:\ProgramData\Paessler\PRTG Network Monitor\` on the PRTG core server. It holds every setting, channel limits included; its maintenance times are read as UTC, since the file does not say PRTG's time zone.

The token or passhash is used for this one reading only: MonitorFlow never stores it nor writes it to a log. Use an `https://` address so it does not travel in clear text.

## Read and preview

1. Open the import page and choose **PRTG**.
2. Type the PRTG address, like `https://prtg.example.com`, and the token, or the user and passhash. Or upload the configuration file instead.
3. Choose the site the servers go to.

MonitorFlow reads the probes, groups, devices and sensors, the limits of the channels it can move, the maintenance windows and the notification templates. A large installation takes a minute: requests are sent a few at a time so PRTG is not slowed down.

The preview lists everything in three parts: what maps exactly, what is approximated (with how it differs) and what cannot be moved (with why). Untick what you do not want: unticking a server unticks its checks, thresholds and silences.

## Confirm, then install the agents

Click **Confirm**. The servers appear in the fleet as waiting for their agent, with the install command; their checks, thresholds and silences are ordinary ones you can change. Install the agent on each machine: it takes over the server of its host name, and monitoring starts.

Changed your mind? **Undo** removes what the import created, except servers an agent has taken over.

## With the API

Everything above is in the public API, with an API key of a site administrator:

```bash
# The tools an import reads, and the fields of their connection.
curl -H "Authorization: Bearer $MF_KEY" https://monitorflow.example.com/api/v1/imports/sources

# Read PRTG and get the preview.
curl -H "Authorization: Bearer $MF_KEY" -H "Content-Type: application/json" \
  -d '{"source": "prtg", "site_id": "…", "connection": {"url": "https://prtg.example.com", "apitoken": "…"}}' \
  https://monitorflow.example.com/api/v1/imports

# Confirm it, leaving out some items by their ID.
curl -H "Authorization: Bearer $MF_KEY" -H "Content-Type: application/json" \
  -d '{"skip": []}' https://monitorflow.example.com/api/v1/imports/<id>/confirm
```

To read the configuration file instead, send `"files": [{"name": "PRTG Configuration.dat", "content": "<base64>", "base64": true}]` in place of `connection`.

## If something goes wrong

- **"refused the credentials"**: the token or passhash is wrong, or the user has no API access.
- **"answered with a redirect"**: PRTG sends plain HTTP to HTTPS; type the `https://` address.
- **A device is missing**: the PRTG user cannot see it. Read with a user who sees every device to move.
- **A sensor is approximated or cannot be moved**: the preview says why. Add what you need by hand from the server's **Sensors** tab after the import.
