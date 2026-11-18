# ADR-0156: VMware and Hyper-V

Date: 2026-11-18
Status: accepted

## Context

Many fleets run on VMware vSphere or Hyper-V rather than Proxmox. People moving from Zabbix or PRTG watch their hypervisors there: hosts that lose their vCenter, datastores filling up, snapshots forgotten for months, replicas that stopped, and every virtual machine whether or not it has an agent. MonitorFlow only knew Proxmox, and only to snapshot a server before an AI change (ADR-0044, ADR-0093). The roadmap asks for hosts, clusters, virtual machines and datastores of vCenter or a standalone ESXi through their API, Hyper-V hosts through the Windows agent, without an agent in each virtual machine, and virtual machines tied to the servers already monitored.

What exists to build on: devices watched by checks from a probe, the central server or a relay (ADR-0148), with sealed credentials per device as SNMP has (ADR-0149, ADR-0125); PowerShell run safely by the Windows agent (ADR-0048, ADR-0084); agents reporting their SMBIOS UUID and MAC addresses (ADR-0093); servers waiting for their agent (ADR-0146).

## Decision

### VMware: a device checked through the vSphere API

**A vCenter or an ESXi host is a device.** It is added like any device (ADR-0148): a name, an address, the probe that checks it. Its check is of kind `vmware`, from a crate of its own, `monitorflow-vmware`, which a probe runs and agents never get. One check reads the whole inventory every 5 minutes by default. Its page then lists the hosts, clusters, virtual machines and datastores found, and its incidents are those of its check, so thresholds, silences, notifications and dashboards apply unchanged.

**The vSphere Web Services API (SOAP, `vim25`), not the REST API.** A standalone ESXi host has no REST API; snapshots, triggered alarms and hardware sensors are only in `vim25`; and govmomi's simulator, vcsim, implements it fully, which makes real tests possible. A collection logs in once (`SessionManager.Login`) and keeps the session cookie for the next ones, logging in again only when the API says the session is gone. It creates one `ContainerView` of the root folder over hosts, clusters, virtual machines and datastores, and reads every property it needs in one `RetrievePropertiesEx`, then `ContinueRetrievePropertiesEx` while a token comes back, at most 1,000 objects a page and 64 MiB in all. No call writes anything. Answers are parsed with `quick-xml`, already in the workspace; SOAP faults become errors with their fault string, bounded to 256 characters.

**A read-only account is enough.** The check needs a user with vCenter's built-in **Read-only** role, granted on the vCenter (or on the ESXi host) and propagated to its children. Nothing in it changes anything: no privilege beyond the role is needed, and a missing one only hides objects from the inventory. The guide says so step by step for vCenter and ESXi.

**Credentials are the device's, sealed.** One row of `vmware_credentials` per device: the port (443), the user, the certificate fingerprint to trust (below), and the password sealed as `vmware_credentials.secret` (ADR-0125). The API never returns the password, only whether it is set, and refuses the credentials as check parameters. When the server sends a probe its checks, it adds them to the device's `vmware` checks, as it does for SNMP, so a relay holds them in memory only.

**Certificates are always verified.** A vCenter or ESXi host usually has a certificate of its own CA or a self-signed one. The check trusts the machine's roots (`rustls-native-certs`), or one certificate pinned by its SHA-256 fingerprint. "Add a device" tests the connection from the probe first: it shows the certificate's fingerprint, subject and expiry and whether the roots trust it; when they do not, the person compares the fingerprint with the one the vCenter shows and confirms it, which stores it. There is no "ignore certificate errors". When the certificate changes, the check fails with "the certificate changed: its fingerprint is now …", and the device's page offers to test and confirm the new one.

**What it measures.** Every object is an instance named after the object (the virtual machine's, host's, datastore's or cluster's name; the same name twice in one inventory gets its managed object ID after it). Each sample carries a `virt` label, `vm`, `host`, `datastore` or `cluster`, and the check's metrics are, prefixed with its name (`vmware`):

| Object | Metrics |
| --- | --- |
| The vCenter or host itself | `up` (1 when the API answers and the login works; 0 with `reason`: `connect`, `tls`, `certificate`, `login`, `api`), labels `product` and `version` |
| Host | `host_connected` (1 connected, 0 disconnected or not responding), `host_maintenance`, `host_powered_on`, `host_cpu_usage_ratio`, `host_memory_usage_ratio`, `host_memory_bytes`, `host_cpu_cores`, `host_status`, `host_hardware_status`, `host_alarms_critical`, `host_alarms_warning` |
| Cluster | `cluster_hosts`, `cluster_hosts_effective`, `cluster_status`, `cluster_alarms_critical`, `cluster_alarms_warning` |
| Virtual machine | `vm_powered_on`, `vm_cpu_count`, `vm_cpu_usage_ratio`, `vm_memory_bytes`, `vm_memory_usage_ratio`, `vm_storage_used_bytes`, `vm_storage_provisioned_bytes`, `vm_snapshots`, `vm_snapshot_age_seconds` (the oldest, absent without one), `vm_status`, `vm_alarms_critical`, `vm_alarms_warning` |
| Datastore | `datastore_accessible`, `datastore_capacity_bytes`, `datastore_free_bytes`, `datastore_free_ratio`, `datastore_status`, `datastore_alarms_critical`, `datastore_alarms_warning` |

A `status` is vSphere's overall status as a number: 0 green, 1 yellow, 2 red, absent when gray (unknown). `host_hardware_status` is the worst of the host's hardware sensors (`numericSensorInfo`), absent when the host has none. Alarms are the triggered alarms on the object itself, red counted as critical and yellow as warning. Templates are left out.

**What it describes.** Besides numbers, each collection writes one `_info` sample per object (value 1) whose labels describe it: its managed object ID (`key`), and for a virtual machine its BIOS UUID, host, power state, guest host name, guest operating system, addresses and MAC addresses (from VMware Tools, as many as fit the 256 bytes of a label); for a host its cluster, product, vendor and model, connection state and whether it is in maintenance; for a datastore its type. The label limits of ADR-0013 apply as to any sample.

### Hyper-V: a check of the Windows agent

**A new Windows kind, `hyperv`, in `monitorflow-sensors`.** Hyper-V has no remote API worth opening, and the Windows agent already runs on the host. The kind runs a fixed PowerShell script built into the agent, with `-EncodedCommand` and the job object of ADR-0084, that reads `Get-VM`, `Get-VMSnapshot`, `Get-VMReplication`, `Get-VMNetworkAdapter` and `Get-VMHardDiskDrive | Get-VHD` of the Hyper-V module, and the SMBIOS UUID of each virtual machine from WMI (`Msvm_VirtualSystemSettingData.BIOSGUID` in `root\virtualization\v2`, the UUID the guest reads, not the VM ID). It prints JSON that portable code parses, so parsing is tested on every platform. The script takes no parameter from the server: the check has none besides its timeout. It writes the same metrics and `_info` samples as VMware for virtual machines (`vm_*`, with `virt=vm`), named after the check (`hyperv`), plus `vm_replication_health` (0 normal, 1 warning, 2 critical, absent without replication) and labels for the replication mode and state. Hyper-V's checkpoints are its snapshots: `vm_snapshots` and `vm_snapshot_age_seconds` count them. The host itself is the agent's server, measured by its usual checks.

**What the agent needs.** Hyper-V has no read-only role: its module answers only members of the local **Hyper-V Administrators** group or administrators. The agent's virtual account is in neither (ADR-0047). Watching Hyper-V is therefore a deliberate step on the host, written in the guide and in the check's error: `Add-LocalGroupMember -Group "Hyper-V Administrators" -Member "NT SERVICE\MonitorFlowAgent"`, then restarting the agent. The script itself only reads, and runs nothing the server sends. Linux agents refuse the kind as unsupported (ADR-0048). A host without the role says "Hyper-V is not installed on this server".

**One click to start.** On a Windows server's page, "Watch Hyper-V" (`POST /api/servers/{id}/hyperv`) adds the check every 5 minutes with its thresholds.

### The inventory and matching

**From the samples.** The server reads the `_info` samples and the `virt` samples of every probe result and every agent batch as they arrive, after validation, and keeps the latest view of each object in PostgreSQL (`virtual_objects`): its source (the vCenter device or the Hyper-V host), kind, key, name, parent (a virtual machine's host, a host's cluster), state, descriptive fields and its latest numbers, and when it was last seen. Writes are batched off the stream, one per source at most every 10 seconds, so a slow database never stalls a stream. No protocol message is added: relays forward and buffer these samples like any other, and an older relay runs no `vmware` check at all. An object not seen for 15 minutes shows "Not seen since"; after a day it is removed.

**Tied to servers already monitored.** Each virtual machine is matched to a server or device of the same customer as its source, never another's, by the first signal that matches exactly one: its SMBIOS UUID against agents' `system_uuid` (in both byte orders, as older VMware virtual hardware reports the first three fields swapped), a MAC address against agents' `mac_addresses`, an address against a device's or an imported server's `address`, then its guest host name or name against a server's host name, compared on the first label without case. Several equal candidates give no match. `matched_by` says which signal matched, and the server's page says "Virtual machine DC0_H0_VM0 on host esx-02 of vcenter-1, matched by its hardware ID". A person corrects a match by hand (`PUT /api/virtual-objects/{id}/server`, a server of the same customer, or none), which the matching then never changes; this is audited (`virt.link`). Automatic matches are not audited: they decide nothing but what a page shows, unlike Proxmox's, which a snapshot acts on.

**Shown on both pages.** The source's page lists its hosts, clusters, virtual machines and datastores with their state, processor, memory, storage, snapshots and alarms, and a link to each matched server. A server's page shows the virtual machine it runs in, its host and source, and its snapshots.

**"Install the agent" in one click.** A virtual machine without a match has an "Install the agent" button (`POST /api/virtual-objects/{id}/install-agent`): it adds a server waiting for its agent (ADR-0146) in the source's site, named after the guest host name or else the virtual machine's name, tied to the virtual machine, and shows the install command. The agent takes it over when it enrolls with that name.

### Alerts that come by default

Adding a vCenter or ESXi host adds, as a template does for SNMP (ADR-0149):

| Alert | Rule |
| --- | --- |
| The API stops answering | `vmware_up < 1` for 2 minutes, critical |
| Host disconnected | `vmware_host_connected < 1` for 5 minutes, critical |
| Datastore full | `vmware_datastore_free_ratio < 0.1` warning, `< 0.05` critical, for 10 minutes |
| Old snapshot | `vmware_vm_snapshot_age_seconds > 604800` (7 days), warning |
| Red alarm | `vmware_*_alarms_critical > 0` on hosts and datastores, warning: vCenter's own alarm, shown so it is not missed |
| Hardware problem | `vmware_host_hardware_status >= 1` warning, `>= 2` critical |

"Watch Hyper-V" adds the old checkpoint rule (`hyperv_vm_snapshot_age_seconds`, 7 days, warning) and replication health (`hyperv_vm_replication_health`, warning at 1, critical at 2). Every rule is an ordinary threshold, changed on the device's or server's page like any other.

### Who may, and the API

As for devices and checks: operators of the site add and change a vCenter, its credentials and Hyper-V checks, and correct matches; viewers read the inventory. "Install the agent" is for administrators of the site, who make install commands. Everything is under `/api/v1` with its OpenAPI document: `POST /api/devices` takes `vmware` credentials, `POST /api/devices/vmware/test` tests a connection, `GET`, `PUT` and `DELETE /api/devices/{id}/vmware`, `GET /api/servers/{id}/virtualization`, `PUT /api/virtual-objects/{id}/server`, `POST /api/virtual-objects/{id}/install-agent` and `POST /api/servers/{id}/hyperv`. Changes are audited (`device.create` with the credentials but no secret, `device.vmware_update`, `device.vmware_delete`, `virt.link`, `virt.install_agent`, `server.hyperv_watch`).

### Moving from another tool

The importer (ADR-0146) maps a Zabbix host with the "VMware" or "VMware FQDN" template, and its `{$VMWARE.URL}` and `{$VMWARE.USERNAME}` macros, to a vCenter device; the password is a secret macro Zabbix does not export, so the item is approximated, "enter the password on the device's page". Hosts that Zabbix's VMware discovery created ("VMware Hypervisor", "VMware Guest") are covered by the vCenter's inventory and are not created again; their triggers on datastore space, host state and snapshots map to the thresholds above, exactly when they compare the same measure, approximated otherwise with the reason. PRTG's VMware sensors (Virtual Machine, Host Server, Host Performance, Datastore, Host Hardware Status, all over SOAP) map to the vCenter device of their parent device, and its Hyper-V sensors (Virtual Machine, Host Server, Virtual Storage Device, Virtual Network Adapter) to "Watch Hyper-V" on the host; what they measure that MonitorFlow does not (per-adapter traffic, storage latency) is listed as approximated with the reason.

## Alternatives rejected

- **The vSphere Automation REST API.** It exists on vCenter only, and lacks snapshots' dates, alarms and hardware health.
- **One device per virtual machine, host or datastore.** A cluster of 500 virtual machines would add 500 devices with their pages and checks; one source with instances keeps one check and one place, and a virtual machine that matters gets its agent in one click.
- **A new protocol message for the inventory.** Samples already travel from agents and probes, through relays' buffers, validated and bounded; an `_info` sample carries what the page needs, the way Prometheus exporters describe objects, and needs no change to relays.
- **Performance counters (`PerformanceManager`).** Real-time counters need one query per entity and counter; the quick statistics in each object's summary give processor, memory and storage use every collection at the cost of one request.
- **Ignoring certificate errors for self-signed hosts.** It would send the password to whoever answers; pinning the fingerprint once is as quick.
- **WinRM or WMI from a probe for Hyper-V.** It needs an inbound port and a Windows account on every host; the agent is already there.

## Consequences

- A vCenter of 1,000 virtual machines is one request of a few megabytes every 5 minutes, and about 15,000 series.
- Matching needs nothing from people when agents report their hardware (ADR-0093); virtual machines without an agent are listed, never lost.
- Hyper-V monitoring gives the agent's account full control of Hyper-V on that host, which the person grants knowingly; the agent's code only reads.
- Hyper-V cannot run in a container: its parsing and collection are tested with output shaped like the cmdlets', not on a real host, until one is available.
- An old snapshot rule fires per virtual machine, so one forgotten snapshot is one incident naming that machine.
