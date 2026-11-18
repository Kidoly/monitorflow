# ADR-0148: Network checks from a probe

Date: 2026-11-18
Status: accepted

## Context

Everything MonitorFlow measured came from an agent on the machine itself. People moving from Zabbix, Prometheus or PRTG also watch what cannot run an agent (switches, printers, appliances, a router at a branch office) and whether an address answers at all, seen from somewhere else. The importer listed every ping, remote port and UDP check as "cannot be moved" (ADR-0146). A check run from one place also says nothing about a network problem between two places: a branch whose relay sees its NAS fine while the central server cannot reach it is a different incident from a NAS that is down.

Relays already sit on each site and keep a stream to the central server, and `crates/sensors` already has a TCP connect probe (ADR-0021). What was missing: a place other than an agent to run checks, something to attach their results to when there is no agent, ICMP, and UDP.

## Decision

**A probe runs network checks.** A probe is the central server or a relay. A check row of `sensor_configs` names who runs it in two columns: `probe` NULL (the server's agent, as before), `central`, or `relay` with `probe_relay_id`. Only a server's own row has a probe; site rows stay about every agent of the site. The column says nothing of the kind, so any kind a probe knows uses it: ping and ports now, SNMP (ADR-0149) and website checks (ADR-0150) next. Agents never receive probe rows. When the API is not told, a new check of a device runs from the relay of its site if there is one, the central server otherwise; the choice is stored, so adding a relay later moves nothing.

**One runner, two places.** `crates/probe` (`monitorflow-probe`) runs a list of checks, each for one server or device, with one sensor scheduler per device (ADR-0012), so a device's checks are keyed by name and instance as an agent's are. Sensors are built by kind through a `ProbeFactory`; `Builtin` builds the network kinds of `monitorflow-sensors` and refuses machine kinds such as `cpu`. Another crate adds a kind by wrapping it. The central server runs its own list on the leader instance (ADR-0089), reloaded every 5 seconds. A relay runs its list as it runs its other work: the server sends a `ProbeConfig` on the relay's own stream after it opens and whenever the list changes, the relay answers `ProbeConfigAck` with the checks it could not start, and sends `ProbeResults`, samples grouped by device, on the same stream. These are three new `oneof` variants of the envelope: older relays drop them, older servers never send them. Each instance sends the relays whose streams it holds; the poll is woken when a relay connects, so it gets its list at once.

**Results are a device's samples.** The server validates probe results as an agent's (ADR-0013) under the identity of the device (its ID as `peer`, its site as `site`) and adds a `probe` label, `central` or the relay's ID, which agents may no longer send. They go to VictoriaMetrics and the threshold engine like any server's series, so thresholds, incidents, silences, notifications, dashboards and graphs need nothing new. A relay is believed only for the devices of the list it was last sent, the way it is believed only for the agents of its site (ADR-0025); results about other devices are dropped and counted. Two probes watching one device keep their incidents apart through the check's instance, which the API sets to the probe (`central` or the relay's ID) when none is given.

**Ping.** A new probe kind, `ping`: `count` echoes (5 by default, at most 20) 200 ms apart, each waiting at most 2 s, within the check's timeout. It reports `up` (some answer), `loss_ratio` and `rtt_seconds` (the average of the answers, absent without one), labelled with the target. It uses an unprivileged ICMP datagram socket, which Linux allows to the groups in `net.ipv4.ping_group_range` (every group by systemd's and Docker's defaults), or a raw socket with `CAP_NET_RAW`. Without either, the check fails with an error that says so. Agents refuse the kind: a server's own reachability is its agent's stream.

**UDP.** `port` takes `protocol=udp` from a probe. There is no handshake, so the check sends something and says what it saw: `query=dns` (a query for the root name servers, the default on port 53) or `query=ntp` (a client request, the default on 123) are up when answered; `query=none` (the default elsewhere) sends one empty datagram and is up unless an ICMP "port unreachable" comes back. It reports `answered` beside `up`, and the interface says in words that "no answer, no refusal" cannot tell an open port that stays silent from a filtered one. Agents still refuse UDP (ADR-0021): it runs from a probe.

**Devices.** An address without an agent is a `servers` row with `kind = 'device'`, its `address` (an IP address or a DNS name), `os_family = 'device'` and no package updates. It has the server's page, incidents, thresholds, silences, dashboards and API, and its own state from its incidents alone: it is never offline (its checks say what is wrong), never waiting for its agent, never offered updates, campaigns, AI commands or remediation, and no agent can take it over (only imported servers waiting for their agent can, ADR-0146). A probe check of a device without a `host` parameter checks the device's address.

**Who may.** As for checks of servers: operators of the site add and change devices and their checks, viewers read them, and the API under `/api/v1` does all of it with the routes and OpenAPI document of the same PR.

## Alternatives rejected

- **Devices as a table of their own.** Every page, query and permission that knows servers would need a second path: incidents, thresholds, silences, dashboards, the fleet strip, notifications, imports. A kind on `servers` reuses them all, and the few places that must skip devices (offline incidents, automatic updates, remediation, takeover by an agent) say so in one condition each.
- **Probe checks as a parameter (`probe=central`).** Parameters are the sensor's to read and validate; who runs a check decides which configuration it goes to, and needs a foreign key to the relay that runs it, so it deletes with the relay.
- **Relays forwarding probe results as a pretend agent stream per device.** It needs a certificate or a Hello per device and a stream each; the relay's own stream already exists, and its single identity is the right one to believe or not.
- **Raw ICMP sockets only.** They need root or a capability on every relay and in the server container; datagram sockets work unprivileged where the distribution allows it, and raw sockets remain the fallback.
- **One instance per device in the series key**, the probe only as a label. The threshold engine keys state by server, metric and instance; two probes watching one device would then share one incident and flap.

## Consequences

- A relay runs the checks of its own site only; a device of another site checked from a relay is not sent to it.
- Results a relay measures while its upstream is down wait in a bounded queue (4096 samples) and newer ones are dropped once it is full: checks are about now, unlike agents' buffered batches (ADR-0027).
- The central server's checks stop for a few seconds when leadership moves to another instance.
- Ping from the server container needs `net.ipv4.ping_group_range` to include the server's group, which Docker sets by default.
- The importer can map pings, remote TCP ports and UDP checks to these checks, and hosts without an agent to devices (ADR-0146).
