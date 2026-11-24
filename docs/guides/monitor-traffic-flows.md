---
title: "How to see which addresses and applications use your bandwidth (NetFlow, sFlow, IPFIX)"
description: "Receive NetFlow, sFlow or IPFIX from your routers, firewalls and switches on a relay or the central server, see the largest addresses, applications and interfaces over time, and get alerted when one address uses too much."
slug: monitor-traffic-flows
lang: en
translation: surveiller-flux-reseau
---

# How to see which addresses and applications use your bandwidth (NetFlow, sFlow, IPFIX)

SNMP tells you that a link is full. Traffic flows tell you **who** fills it and **with what**: the addresses, the applications (HTTPS, SMB, RDP...) and the interfaces. Routers, firewalls and many switches export them as NetFlow, sFlow or IPFIX; MonitorFlow receives them on the **relay of the site** or on the **central server**.

## Why it matters

A saturated branch link slows everyone down, and the interface graph only says "full". A backup that started at the wrong time, a workstation syncing a cloud drive, or a compromised machine sending data out look the same on SNMP and very different on flows.

## What MonitorFlow shows

| View | What it says |
| --- | --- |
| **Totals** | The average rate and the volume of the period |
| **Chart** | The 5 largest sources, destinations, applications, protocols or interfaces over time, stacked, and the rest |
| **Sources and destinations** | The addresses that sent and received the most |
| **Applications** | The protocol and service port, named when known: `tcp/443` is HTTPS |
| **Protocols** | TCP, UDP, ICMP, GRE... |
| **Input and output interfaces** | The device's interfaces, named from its SNMP interfaces check |

Click an address to see only its traffic in every view. Periods go from 1 hour to 30 days. Each minute is kept 48 hours, then each hour 31 days.

NetFlow v5 and v9, IPFIX and sFlow v5 are read. When the device samples (1 packet in 1,000, for instance), MonitorFlow multiplies by the rate it announces, so the figures are what went through the link.

## Turn it on

1. Add the router, firewall or switch as a device if it is not one yet (see [network devices](monitor-network-devices.md)). Its SNMP interfaces check names its interfaces in the flows.
2. Open the device's page, tab **Flows**.
3. **Received by**: the relay of its site is chosen when there is one. **Source address**: only when the device exports from another address than its own, such as a loopback.
4. Click **"Receive flows"**.
5. On the device, export flows to the address of that relay (or of the central server), on UDP port **2055** (NetFlow), **6343** (sFlow) or **4739** (IPFIX). Every port takes every format.

The relay opens these ports only while a device sends it flows, and takes datagrams only from those devices. Allow them through any firewall between the devices and the relay. The first minute shows about a minute after the device starts sending.

## Get alerted

Under the views, **Alert on an address** adds an alert in one click: "above 100 Mbit/s for 5 minutes" opens an incident when any one address sends or receives more than that for the whole time. It is a threshold of the device, on the metric "Traffic of an address", so you can change it, silence it or add a critical level in the **Sensors** tab like any other.

## When nothing shows

- **"Nothing received yet"**: check on the device that the exporter points at the relay's address and the right port, and that a firewall lets UDP through.
- **Another address**: some devices export from a loopback or a management interface. Set it as **Source address**.
- **Interfaces shown as "ifIndex 7"**: the device has no SNMP interfaces check, or it has not run yet.
- **Coming from PRTG**: the importer lists NetFlow, sFlow and IPFIX sensors with this reminder; turn Flows on for each device instead.
