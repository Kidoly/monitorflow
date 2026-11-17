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

## Moving to MonitorFlow

- [How to move from Zabbix or Prometheus to MonitorFlow](move-from-another-tool.md): Import your hosts, checks, thresholds, maintenance windows and notification channels from Zabbix or Prometheus, preview what moves, then install the agent.

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

## Linux services

- [How to monitor SSH and fail2ban on Linux](monitor-ssh-fail2ban.md): Get alerted when the SSH server stops, port 22 no longer answers, fail2ban stops banning, or failed logins pile up on a Linux server.
- [How to monitor cron and scheduled jobs on Linux](monitor-cron.md): Get alerted when cron stops on a Linux server, and when a nightly job such as a backup has not produced its result in time.
- [How to monitor time synchronisation (NTP) on Linux and Windows](monitor-time-sync.md): Get alerted when chrony, systemd-timesyncd or Windows Time stops, and when a server's clock drifts too far, before logins and certificates fail.
- [How to monitor Postfix on Linux](monitor-postfix.md): Get alerted when Postfix stops, port 25 no longer answers, or the mail queue grows, so mail from your servers and applications keeps flowing.
- [How to monitor NFS and SMB file shares](monitor-nfs-smb-shares.md): Get alerted when a file server stops answering on NFS or SMB, when its service stops, or when a mounted share fills up on a Linux client.

## Windows

- [How to monitor Windows services](monitor-windows-services.md): Get an alert when a Windows service stops or disappears, such as Windows Update, Print Spooler, DNS Server or Windows Time.
- [How to monitor IIS](monitor-iis.md): Get an alert when the IIS web server stops or no longer answers on ports 80 and 443 on your Windows servers.
- [How to monitor Active Directory domain controllers](monitor-active-directory.md): Watch your domain controllers: Active Directory, DNS Server, LDAP on port 389, Windows Time and System event log errors, with alerts.
- [How to monitor the Windows event log for errors](monitor-windows-event-log.md): Get an alert when Windows writes a critical or error event to the System or Application log, on one server or a whole site.
- [How to monitor Remote Desktop (RDP)](monitor-remote-desktop.md): Get an alert when Remote Desktop stops answering on port 3389 or its service stops, on your Windows servers.

## Administration

- [How to sign in to MonitorFlow with your own identity provider](sign-in-with-your-identity-provider.md): Let your colleagues sign in with the accounts and second factor they already have: OpenID Connect (Entra ID, Google, Keycloak, authentik) or an LDAP directory over TLS.
