---
title: "How to monitor switches, printers, UPSs and other network devices"
description: "Watch what cannot run the agent: ping, TCP and UDP ports, and SNMP v2c or v3 for interfaces, traffic, UPS battery, temperatures and printer supplies, checked from a relay of the site or the central server."
slug: monitor-network-devices
lang: en
translation: surveiller-equipements-reseau
---

# How to monitor switches, printers, UPSs and other network devices

Not everything can run the MonitorFlow agent. A switch, a router, a printer, a UPS or a NAS is still worth watching, and so is an address seen from somewhere else: a branch office that loses its link looks fine from inside and dead from the central server. MonitorFlow checks these over the network, from the **relay of their site** or from the **central server**, and alerts like for any server.

## Why it matters

A switch port that goes down takes the servers behind it offline; a UPS that runs on battery gives you minutes, not hours; a printer out of toner stops an office. None of them sends an email on its own.

## What MonitorFlow checks

| Check | What it measures | Alert by default |
| --- | --- | --- |
| **Ping** | Whether the address answers, the round trip and the packet loss over a few pings | Critical after 2 minutes without an answer, a warning above 20% loss for 5 minutes |
| **TCP port** | Whether a port accepts connections | Critical when it closes |
| **UDP port** | A DNS or NTP server answering; for other ports, that the device does not refuse the packet | Critical when it stops answering |
| **SNMP** | Uptime, interfaces up or down, traffic in and out, errors and discards; processor, memory and storage; UPS battery, runtime and whether it is on battery; temperatures; printer supplies | Depends on the kind of device, below |

A UDP port that is not DNS or NTP has no answer to expect: MonitorFlow says it is up as long as the device does not refuse it. A port that stays silent and one blocked by a firewall look the same.

## Add a device

1. Open **Fleet** and click **"Add a device"**.
2. Give it a **Name** and its **Address** (an IP address or a DNS name).
3. **Checked from**: the relay of its site is chosen when there is one. Choose the central server to see the device from outside.
4. Keep **"Ping it every minute…"** ticked for a ping with its alerts.
5. For SNMP, tick **"Watch it over SNMP"**, enter the community (SNMP v2c) or the user, authentication and privacy passwords (SNMP v3, SHA and AES), then click **"Detect"**: MonitorFlow asks the device what it answers and picks its **Kind of device**.
6. Click **"Add the device"**.

The device gets its own page, with its checks, their values and their alerts, and it can be silenced or put on a dashboard like a server. It is never shown as offline or waiting for an agent: its checks say what is wrong.

## What each kind of device gets

| Kind | Checks | Alerts |
| --- | --- | --- |
| Switch | SNMP, interfaces, temperatures | No SNMP answer; a link above 90% for 10 minutes; more than one error a second; above 70 °C, critical above 80 °C |
| Router, Firewall | SNMP, interfaces, processor and memory, temperatures | The same, and the processor above 90% |
| NAS | SNMP, interfaces, processor, memory and storage, temperatures | Storage above 85%, critical above 95% |
| UPS | SNMP, UPS battery | Critical on battery, battery low, under 10 minutes of runtime |
| Printer | SNMP, printer supplies | A supply under 10%, critical under 2% |
| Generic | SNMP, interfaces | No SNMP answer; a link above 90% |

An interface that is down raises nothing by default, since a switch's unused ports are down. To be told when one port goes down, add an alert on that interface from the device's page.

## Change the credentials

On the device's page, open **Details**: the **SNMP** line says the version, the user and the port, never a password. **"Change…"** replaces them; a password left empty keeps the stored one.

## When the alert fires

- **No SNMP answer**: check the community or the v3 passwords, and that the device allows SNMP from the relay or the central server.
- **Ping fails from the relay but not from the central server** (or the other way): the problem is on the network between them, not on the device.
- **On battery**: the UPS lost mains power. Its runtime left says how long you have.

## Your own servers' traffic

Servers with the agent measure the traffic, errors and drops of their own network interfaces too, with nothing to add: their **Sensors** tab has a row per interface.
