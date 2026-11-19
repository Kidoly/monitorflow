---
title: "How to monitor VMware vSphere and Hyper-V"
description: "Watch the hosts, clusters, virtual machines and datastores of a vCenter, an ESXi host or a Hyper-V host, get alerted on disconnected hosts, full datastores and old snapshots, and tie each virtual machine to its server."
slug: monitor-vmware-hyper-v
lang: en
translation: surveiller-vmware-hyper-v
---

# How to monitor VMware vSphere and Hyper-V

A hypervisor carries many servers at once. When a host loses its vCenter, a datastore fills up or a snapshot is forgotten for weeks, every virtual machine on it is at risk, including those without the MonitorFlow agent. MonitorFlow reads vCenter and ESXi through their API, and Hyper-V through the agent of the Windows host, without an agent in each virtual machine.

## Why it matters

- **A full datastore** stops every virtual machine writing to it, all at once.
- **A disconnected host** keeps its virtual machines running, but nobody can manage or migrate them, and high availability may not cover them.
- **An old snapshot** grows with every write until it fills its datastore, and slows the machine down.
- **A replica that stopped** (Hyper-V) means the disaster recovery plan no longer works, and nobody sees it until it is needed.

## VMware: add the vCenter or the ESXi host

### 1. Make a read-only account

MonitorFlow only reads. An account with the built-in **Read-only** role is enough.

- **vCenter**: in the vSphere Client, *Administration › Single Sign On › Users and Groups*, add a user such as `monitor@vsphere.local`. Then *Administration › Access Control › Global Permissions*, add it with the role **Read-only** and tick **Propagate to children**.
- **A standalone ESXi host**: in the host client, *Host › Manage › Security & users › Users*, add a user. Then *Host › Actions › Permissions*, give it the role **Read-only**.

A missing permission only hides objects from the inventory; it never gives an error.

### 2. Add it as a device

In the fleet, **Add a device**: its name and address, then tick **It is a vCenter or an ESXi host** and enter the user and password. **Test the connection**: MonitorFlow connects from the relay of the site (or the central server) and shows what answers and its certificate.

Most vCenters and ESXi hosts use a certificate of their own authority. MonitorFlow then shows its **SHA-256 fingerprint** and does not send the password until you trust it. Compare it with the one the vCenter shows (open its address in a browser and look at the certificate's details), or read it from a machine you trust:

```
openssl s_client -connect vcenter.example.lan:443 </dev/null 2>/dev/null | openssl x509 -noout -fingerprint -sha256
```

Tick **I compared it: trust this certificate**, test again: the sign-in result lists what the account sees. **Add the device**.

When the vCenter's certificate changes later, its page says so with the new fingerprint: open the device's **Details**, **vCenter › Change…**, test and trust the new one.

## What MonitorFlow checks

Every 5 minutes, one request reads the whole inventory. The device's page lists:

| What | Shown |
| --- | --- |
| **Hosts** | Connected, disconnected, not responding or in maintenance; processor and memory in use; overall and hardware status; alarms |
| **Clusters** | Hosts in use out of all hosts; alarms |
| **Virtual machines** | Running or stopped; processor, memory and storage; snapshots and the oldest one's age; alarms; the server it is |
| **Datastores** | Free space and capacity; reachable or not; alarms |

Alerts that come with it, changed on the device's page like any threshold:

| Alert | Default |
| --- | --- |
| The API stops answering | Critical after 2 minutes |
| Host disconnected | Critical after 5 minutes |
| Datastore full | Warning under 10% free, critical under 5%, for 10 minutes |
| Snapshot older than 7 days | Warning, per virtual machine |
| Red alarm on a host or datastore | Warning |
| Hardware sensor not green | Warning when yellow, critical when red, for 5 minutes |

## Hyper-V: watch it with the agent

Install the MonitorFlow agent on the Windows server that runs Hyper-V. Hyper-V has no read-only role: its management commands answer only administrators and members of **Hyper-V Administrators**. Add the agent's account to that group, in PowerShell as an administrator, then restart the agent:

```
Add-LocalGroupMember -Group "Hyper-V Administrators" -Member "NT SERVICE\MonitorFlowAgent"
Restart-Service MonitorFlowAgent
```

The agent only runs a fixed script that reads; it takes no command from the server. On the server's page, **Watch Hyper-V** adds the check every 5 minutes. Each virtual machine shows its state, processor, memory, storage, checkpoints (Hyper-V's snapshots) and replication health, with these alerts:

| Alert | Default |
| --- | --- |
| Hyper-V cannot be read | Warning after 15 minutes, with the reason, such as the group above |
| Checkpoint older than 7 days | Warning |
| Replication health | Warning, critical when Hyper-V says critical, for 10 minutes |

## Virtual machines and the servers you already monitor

MonitorFlow ties each virtual machine to the server or device of the same customer it is: by its hardware ID (the agent reports it), its network card, its address, or its name when nothing else matches. The virtual machine's line names its server and how it was matched, and the server's page says "Virtual machine web-01 on host esx-02 of vcenter-1". When a match is wrong, **Not this one** removes it for good; **Match automatically** hands it back.

A virtual machine without the agent has **Install the agent**: it adds a server waiting for its agent, named after the virtual machine, and opens its page with the install command.

## When an alert fires

- **Datastore full**: look for old snapshots on its virtual machines first, then thin disks that grew; move a virtual machine to another datastore.
- **Host disconnected**: check the host's management network and its `hostd` and `vpxa` services; its virtual machines still run.
- **Old snapshot**: ask who took it before removing it; a backup tool may have left it behind after a failed job.
- **Hyper-V cannot be read**: the message says why, usually the group membership above.

## Moving from another tool

The import reads Zabbix hosts with the VMware template (the vCenter's URL and user; enter the password after) and PRTG's VMware and Hyper-V sensors. See [how to move from Zabbix or Prometheus](move-from-another-tool.md) and [from PRTG](move-from-prtg.md).
