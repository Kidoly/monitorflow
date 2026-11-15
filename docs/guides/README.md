---
title: "How to monitor your servers with MonitorFlow"
description: "Step-by-step guides to monitor databases, web servers, Linux and Windows services, disks and logs with MonitorFlow, and get alerted."
slug: guides
lang: en
translation: guides
---

# How to monitor your servers with MonitorFlow

Each guide explains what can go wrong, what MonitorFlow checks, how to add the check, what the alert looks like and what to check first when it fires. They start from a server where the MonitorFlow agent is installed.

Guides are also available [in French](fr/README.md).

## Servers

- [How to get alerted when a server goes down](server-down-alerts.md): Know within minutes when a Linux or Windows server stops reporting, without setting anything up, and get told by email or chat.
- [How to monitor CPU and memory usage on Linux and Windows servers](monitor-cpu-memory.md): Get alerted when a server's processors stay saturated or its memory runs out, with built-in thresholds you can change per server or site.
- [How to monitor disk space on Linux and Windows servers](monitor-disk-space.md): Get warned before a disk fills up and stops your services: per-disk alerts at 85% and 95% out of the box, plus slow-disk alerts.
- [How to monitor log files for errors on Linux and Windows](monitor-log-files.md): Get alerted when an error, a failed login or any pattern you choose appears in a log file, and read your servers' system logs in one place.
- [How to monitor anything with your own script (Bash or PowerShell)](monitor-with-your-own-script.md): Turn any shell or PowerShell script into a check with an alert: exit codes for OK, warning and critical, and name=value lines as metrics.
