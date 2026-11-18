---
title: "How to move from Zabbix or Prometheus to MonitorFlow"
description: "Import your hosts, checks, thresholds, maintenance windows and notification channels from Zabbix or Prometheus, preview what moves, then install the agent."
slug: move-from-another-tool
lang: en
translation: venir-d-un-autre-outil
---

# How to move from Zabbix or Prometheus to MonitorFlow

Switching monitoring tools usually means typing everything again: hundreds of hosts, their checks, the thresholds someone tuned over the years, maintenance windows and who gets told. MonitorFlow reads your current tool's configuration instead, shows you what it can do the same way, what it does differently and what it cannot do, and creates nothing until you confirm. Your servers then only need the MonitorFlow agent, and the old tool can keep running until you switch it off.

## What gets moved

| In your current tool | In MonitorFlow |
| --- | --- |
| Hosts, targets | Servers waiting for their agent, with their name, addresses and system |
| Items, scrape jobs, probes | Checks: services, TCP ports, log patterns; CPU, memory and disks are measured on every server already |
| Triggers, alerting rules | Alerts with warning and critical levels, how long a problem must last, and the recovery margin |
| Maintenances, silences | Silences of the servers they cover |
| Media types and user media, receivers | Email, webhook, Slack, Teams and Discord notification channels |

Every line of the preview is in one of three lists:

- **Same in MonitorFlow**: it maps exactly.
- **Approximated**: MonitorFlow does something close, and the line says how it differs. A web scenario of several steps becomes a website check of its first page, for example, and a recurring maintenance becomes a silence of its next window.
- **Cannot be moved**: the line says why, with the expression or key it came from, so you can decide what to do about it.

## Import from Zabbix

You need a Zabbix administrator account. Zabbix 6.0, 6.4 and 7.0 are supported.

**From its API** (recommended: it includes maintenances and who gets notified):

1. In Zabbix, go to **Users › API tokens** and create a token for a Super admin user.
2. In MonitorFlow, go to **Settings › Move from another tool**, choose **Zabbix**, then **Connect to its API**.
3. Type the address of the Zabbix web interface (such as `https://zabbix.example.com`) and the token, then **Read and preview**.

MonitorFlow only reads from Zabbix and does not keep the token.

**From export files**: in Zabbix, select your hosts under **Data collection › Hosts** and choose **Export** (YAML, XML or JSON). Export the templates they use and your media types the same way, then upload all the files at once. Exports carry no maintenances or user media, so silences and the recipients of email channels come only through the API.

What maps how:

- CPU, memory, file system and disk activity items are measured by every MonitorFlow server already. Their triggers become alerts with the same levels, macros resolved per host.
- `net.tcp.service` and `net.tcp.port` become TCP port checks; `systemd.unit.*` and Windows `service.*` become service checks; `log[]` and `logrt[]` become log checks.
- Web scenarios, HTTP agent items, `web.page.*` and `web.certificate.get` become website checks: the first step's URL, required status codes and string, redirects, headers and user name. Triggers on `web.test.fail`, `web.test.rspcode` and on those items are their alert, triggers on `web.test.time` and on a certificate's expiry thresholds on the check. Later steps, variables, a POST and a required string that is a regular expression are noted. Passwords are not imported: set them in the check afterwards. `proc.num` becomes a check of the systemd service of that name.
- Agent availability triggers and `agent.ping` are MonitorFlow's offline alert, which every server has.
- Network interface traffic, SNMP, `system.run` and UserParameters cannot be moved yet.

## Import from Prometheus

**From its API**: choose **Prometheus**, then **Connect to its API**, and type the Prometheus address (such as `http://prometheus.example.com:9090`) and, optionally, the Alertmanager address. With a live Prometheus, MonitorFlow sees the targets your file service discovery resolved and the real host names node exporter reports. Alertmanager's API hides secrets, so webhook and chat receivers come from files instead.

**From files**: upload `prometheus.yml`, the rule files, the file service discovery files and `alertmanager.yml` together.

What maps how:

- Node exporter targets become servers. A blackbox exporter's TCP probes become port checks and its HTTP probes website checks with the module's status codes, method, headers, redirects and body conditions (plain text; a real regular expression is noted), on the server that runs the exporter. TCP probes with `tls: true` become website checks of the certificate. Rules on `probe_success` and `probe_http_status_code` are their alert, rules on `probe_duration_seconds` and `probe_ssl_earliest_cert_expiry - time()` thresholds on the check.
- The usual node rules (target down, CPU, load, memory, file system space, disk latency, systemd units) become alerts with their levels and `for:` durations, and `probe_success == 0` the alert of each probe.
- Any other expression is listed as cannot be moved, with the expression.
- Receivers become channels: email, webhook, Slack, Microsoft Teams and Discord. Alertmanager routes are not moved: in MonitorFlow, a channel covers everything or one site.
- Active silences become silences of the servers their matchers select.

## Preview, confirm, undo

1. Read the three lists. Untick anything you do not want: what depends on it (the checks of a server you unticked) is left out too.
2. Choose **Create**. Everything is created in one step and written to the audit log.
3. Importing the same configuration again creates nothing twice: lines MonitorFlow has already show **Already there**.
4. Changed your mind? Open the import under **Earlier imports** and choose **Undo this import**. It removes what the import created, except servers whose agent has already arrived, which keep their checks.

Everything here is also in the public API, under `/api/v1/imports`: list the sources, read files or connect, read the preview, confirm with the items to leave out, and undo.

## Install the agents

Imported servers show **Waiting for agent** in the fleet and raise no offline alerts. On a server's page, **Make an install command** gives a ready-to-paste command for that server. You can also use an install key of the site from **Settings › Add servers**: an agent that enrolls with the same host name, or the short form of it, takes over the imported server with all its checks and alerts.

Keep the old tool running while the agents arrive. Both will notify you during that time; switch the old one off once every server reports to MonitorFlow.
