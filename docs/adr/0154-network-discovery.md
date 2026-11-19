# ADR-0154: Network discovery

Date: 2026-11-18
Status: accepted

## Context

Since ADR-0148 and ADR-0149 the central server and relays check devices, and "Add a device" detects what one address answers over SNMP. Someone taking over a network still has to know every address first. Zabbix has network discovery rules and actions, PRTG auto-discovery groups: give a range, and the tool lists what answers and adds it. People moving from them expect the same, and a person setting up MonitorFlow on a new site wants to see what is there before deciding what to watch.

Scanning a network has its own risks. A scan sends traffic to addresses nobody named one by one, so it must stay inside the range it was given, go slowly enough not to trip an intrusion detection system or flood a small link, and never add anything by itself: a printer someone does not want watched should not start raising incidents. A relay that scans its site is untrusted for what it reports, as for everything else (architecture §4).

## Decision

**A scan belongs to a site.** `network_scans` holds a name, the addresses to scan, the TCP ports to try, the probe that runs it (`central` or a relay of the site, the site's default probe when not told, as for checks), a schedule and whether to notify. Addresses are IP addresses only, never DNS names, given as a list of addresses, CIDR blocks and `a.b.c.d-e` ranges: what a name resolves to is not what the person read, so names could take the scan outside the range. Together they are at most 1,024 addresses (a /22). Network and broadcast addresses of IPv4 blocks shorter than /31 are left out, and so are loopback, multicast, link-local and unspecified addresses, which the API refuses. Ports default to 22, 23, 80, 135, 139, 443, 445, 3389, 5985, 8080 and 8443, at most 32 of them, TCP only.

**A scan is a series of one-off tests on its probe.** A new probe kind, `discover`, of `crates/probe`, takes up to 16 addresses, the ports and SNMP credentials as parameters, and reports one sample per address that answered something: one echo of ping, a TCP connect to each port (open when it connects; refused and silent are both closed), the SNMP detection of ADR-0149 (sysDescr, sysName, sysObjectID and which groups answer) with each set of credentials until one answers, the MAC address from the probe's ARP table when the address is on its own link, and the name from reverse DNS (the system resolver, so `/etc/hosts` counts). It starts at most 50 probes a second (an echo, a connection or an SNMP request) and waits at most a second for each. The central server runs it in process, a relay through the `ProbeTest` and `ProbeTestResult` envelopes of ADR-0149, so nothing changes on the wire: a relay older than this answers that it does not run `discover` checks, and the scan says to update it. A run sends its tests one after the other, so a /24 takes about two minutes, and a probe runs one scan at a time.

**Runs.** Each scan has runs (`network_scan_runs`): queued by "Scan now" or by the schedule (every hour, day or week, counted from the start of the last run), at most one queued or running per scan, the 10 most recent kept. Every instance looks for queued runs every 5 seconds and takes the ones whose probe it can reach, the central server or a relay whose stream it holds, with `FOR UPDATE SKIP LOCKED`, so a relay's scan runs where its stream is (ADR-0089). A running run notes its progress every chunk; one that has not moved for 2 minutes was on an instance that stopped, and fails with that reason.

**What a run found is kept per address.** `network_scan_hosts` holds, per run, each address that answered: ping and its round trip, open ports, name, MAC address, and for SNMP which set of credentials answered, sysDescr, sysName, sysObjectID, the groups that answer and the template that fits, as "Add a device" suggests it. What a relay reports is checked as untrusted input: an address it was not asked about, a port it was not asked to try, a name longer than 253 bytes or with control characters is dropped and counted. Text from the network (names, descriptions) is shown as text.

**SNMP uses the site's credentials.** A scan tries the credentials given to it, if any, sealed in `network_scans.snmp_secret` like a device's (ADR-0125) and never returned, then each distinct set that devices of the site already have, at most four sets. A device added from a scan gets the set that answered, copied into its own `snmp_credentials` row.

**Proposals are computed when read.** As recommendations are (ADR-0134), the proposals of the latest run follow the inventory without a table to keep in step:

- **Already monitored**: a device of the site at that address, or a server of the site with the MAC address the probe saw, or whose host name is the reverse DNS name (or its first label) or the SNMP sysName, without case. Listed under "Already monitored", never proposed again.
- **Install the agent**: SSH (22) or RDP (3389) open, unless SNMP says it is a switch, router, firewall, printer or UPS. It links to the site's install command (ADR-0073), for Linux when SSH is open and for Windows when RDP or WinRM is. Nothing is added: the agent's server appears when it enrolls, and the next scan recognises it.
- **SNMP device**: answered SNMP; added with the template detection suggests, which the person can change, its credentials and its ping.
- **Device**: anything else that answered; added with a ping if it answered ping and a port check for each open port, each with the default alert.

A person adds them one by one or all at once (`POST /api/network-scans/{id}/add`), through the code that "Add a device" uses, with the same validation, roles and audit; checks run from the scan's probe. An address can be dismissed for the scan, for good, and restored, as recommendations are. Nothing is ever added without that request.

**What changed.** Each finished run compares the addresses that answered with those of the previous finished run of the scan: the new ones are marked, the gone ones listed on the run. When the scan's `notify` is on (off by default), a run with something new or gone queues a `network_scan.changed` notification for the site, which channels and silences treat like any event of the site.

**Who may.** Viewers of the site read scans and their results; operators of the site create, change, run and delete them, add what they propose and dismiss addresses, as for devices. Every change is audited (`network_scan.create`, `.update`, `.delete`, `.run`, `.add`, `.dismiss`, `.restore`), and runs started by the schedule are not, as they change nothing. The routes are under `/api/network-scans` and `/api/v1`, in the OpenAPI document.

**Moving from another tool.** The importer maps Zabbix network discovery rules (their IP ranges, delay, TCP and SNMP checks) and PRTG auto-discovery groups (their IP ranges and schedule) to scheduled scans of the site their hosts go to, with notifications off. A delay that is not an hour, a day or a week becomes the nearest of them, and a range of more than 1,024 addresses keeps its first 1,024; both say so. Zabbix auto-registration of agents is the install key of the site (ADR-0073) and is listed as not moved, with that reason.

## Alternatives rejected

- **New envelope variants for a long scan on the relay.** A scan would then be a job on the relay, with its own progress, cancellation and resumption after a reconnect. Chunks of one-off tests reuse what exists, keep the relay stateless, and let the server pace and stop a scan.
- **Adding what answers by itself**, as Zabbix actions can. A scan of a busy network finds phones, laptops and printers nobody wants alerts about; a person confirming once is cheap, and "Add all" keeps it one click.
- **Ping first, then only addresses that answered.** Many firewalls and Windows hosts drop ping; trying the ports and SNMP on every address costs little at this rate.
- **Scanning UDP ports and fingerprinting operating systems** like nmap. Slower, noisier, and more likely to alarm an intrusion detection system for little more than SSH, RDP and SNMP already tell.
- **Storing proposals.** They would go stale as soon as a device is added by hand or an agent enrolls.

## Consequences

- A /24 takes about two minutes and sends a few thousand packets, at most 50 a second; a /22 about eight minutes.
- A scan from the central server in Docker sees the networks the container is on; MAC addresses, and so recognising agents by them, only on the probe's own link.
- A scheduled scan of a relay that is offline waits queued until it reconnects.
- The scan's probe sees SNMP credentials of the site's devices in memory during the scan, as it does when it checks them.
