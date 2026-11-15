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

## Databases

- [How to monitor PostgreSQL](monitor-postgresql.md): Get alerted when PostgreSQL stops or stops accepting connections on a Linux server, with two checks from the MonitorFlow catalogue.
- [How to monitor MySQL and MariaDB](monitor-mysql-mariadb.md): Get alerted when MySQL or MariaDB stops or stops accepting connections, with the service and port checks of the MonitorFlow catalogue.
- [How to monitor Redis](monitor-redis.md): Get alerted when Redis stops or stops accepting connections on a Linux server, with the service and port checks of MonitorFlow.
- [How to monitor MongoDB](monitor-mongodb.md): Get alerted when MongoDB stops or stops accepting connections on a Linux server, with the mongod service and port 27017 checks of MonitorFlow.
- [How to monitor SQL Server](monitor-sql-server.md): Get alerted when a SQL Server instance stops or stops accepting connections on Windows, with the MSSQLSERVER service and port 1433 checks.

## Web and virtualisation

- [How to monitor nginx](monitor-nginx.md): Get alerted when nginx stops, when ports 80 and 443 stop answering, or when its error log reports critical errors, on every Linux server.
- [How to monitor Apache](monitor-apache.md): Get alerted when the Apache web server stops or ports 80 and 443 stop answering, on Debian, Ubuntu, RHEL and Rocky servers.
- [How to monitor a website over HTTPS](monitor-website-https.md): Know when your website stops answering on port 443 or 80, and check its status code and certificate expiry with a short script.
- [How to monitor Docker](monitor-docker.md): Get alerted when the Docker engine or containerd stops on a Linux server, and when a published container port stops answering.
- [How to monitor Proxmox VE](monitor-proxmox-ve.md): Watch your Proxmox VE hosts: the web interface on port 8006, the cluster service, and the disk space, CPU and memory every VM depends on.
