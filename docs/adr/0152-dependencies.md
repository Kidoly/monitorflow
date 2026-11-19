# ADR-0152: Dependencies

Date: 2026-11-18
Status: accepted; generalises the relay rule of ADR-0072 and ADR-0080

## Context

When a switch dies, everything behind it fails at once: its own ping, the agents that reach the central server through it, the devices and website checks a probe watches across it. Each opens its own incident and the person on call gets one alert per machine, with the cause somewhere in the list. Grouping (ADR-0080) turns forty messages into one, but the message still says "41 alerts at paris", not "the switch is down".

MonitorFlow already knows one such cause. A server behind a relay that is offline opens no offline incident of its own: the relay's incident speaks for it (ADR-0072), and once the relay is back its servers get 5 minutes to reconnect before they are judged (ADR-0080). Nothing else can say "this is behind that". Every tool people move from can: Zabbix has trigger dependencies, PRTG a device tree whose parent pauses its children and a "dependency" setting per object, Nagios parent hosts. The importer (ADR-0146) lists them as not moved.

Phase 17 added devices (ADR-0148), SNMP devices (ADR-0149) and website checks (ADR-0150), so the parents people have in mind (a switch, a firewall, a router, an uplink) are now in MonitorFlow too.

## Decision

**What depends on what.** A dependency says that a child is behind a parent. A child is a server or a device, or one check of one (`{name, instance}`, as silences name checks, ADR-0151). A parent is a server or a device, a relay, or one check of a server or device, such as "the uplink ping". A child may have several parents and is unreachable while any of them is down: one dead path is enough to blind the central server, and a child with two redundant paths does not need a dependency at all. A dependency never crosses customers: both ends belong to one customer, which the API and a database trigger check.

**Cycles are refused.** A dependency that would make something depend on itself, directly or through others, is refused with the path in words: "web-1 cannot depend on sw-core: sw-core already depends on web-1 (sw-core, then fw-1, then web-1)". For this, a check is behind its own server, a server behind its relay, and a check behind the relay that runs it. So one server's checks may depend on each other ("https depends on ping"), but a server cannot depend on one of its own checks.

**Implicit parents.** Two dependencies exist without being set, so nothing changes for people who set nothing: a server is behind the relay it is assigned to (ADR-0072), and a check a relay runs (ADR-0148) is behind that relay. They are shown with the others, marked automatic, and cannot be removed.

**When a parent is down.** A parent is down while:

- a server with an agent, or a relay, has its offline incident open (ADR-0072);
- a device has a critical incident open from one of its ping checks; a device without a ping check is down only through a parent check, and its page says so with "Add a ping check";
- a check has a critical incident open.

A warning is never down: a switch losing 20 % of its pings is degraded, and what is behind it still has things to say. A parent that is itself unreachable counts as down for its own children, so a router, a switch behind it and the servers behind the switch form a tree: the router's incident speaks for all of it.

A parent is in doubt while it may be about to go down: an agent or relay without a stream (the 5 minutes ADR-0072 waits), or a check whose critical condition holds but has not lasted the threshold's duration yet (`alert_states`). This is what makes the cause win the race.

**What a child does meanwhile.** A child whose parent is down or in doubt opens no incident of its own and notifies nobody. The threshold engine evaluates its samples as usual, so it knows the condition holds, but does not write the incident; the offline rule skips it as it skips servers behind an offline relay today. The child shows as unreachable, "Unreachable: sw-core is down", on its page, in the fleet and in lists, and on dashboards. An incident it had open before stays open, as ADR-0072 does for a server behind a relay.

The parent's incident lists what it hides: every child behind it, transitively, with its state, on the incident page (`hides` in `GET /api/incidents/{id}`), and the count in its notification: "Critical: ping on sw-core (paris) is down; 14 servers, devices and checks behind it cannot be checked". This replaces `relay.servers` for relays, which stays in webhook payloads for receivers of version 1.

**When the parent recovers.** Children are judged again 2 minutes after the parent's incident resolves. That is two runs of a probe check at its default interval, and the time a switch's links take to settle; a child that still fails then opens its own incident, dated from then, and only then, through the next sample that fails. A check that runs less often is judged at its next run after the 2 minutes. The offline rule keeps the 5 minutes of ADR-0080: the parent's recovery counts as heard from, so agents have as long as anyone to reconnect through it. A parent that leaves doubt without going down (its condition cleared within its duration) frees its children at once: nothing happened to them.

**When a child fails first.** The engine reads dependencies and parent states every 5 seconds, so a child that fails within those seconds before its parent can still open an incident. The dispatcher, which waits at least 10 seconds before telling anyone (ADR-0080), looks again when it claims the event: an opened or escalated event of an incident whose child is behind a parent that is down by then is not sent, and the incident is marked as hidden by the parent's incident (`hidden_by`). Its later events are not sent either, and nobody is asked to acknowledge it. It stays in the history, listed under the parent's incident. If it is still open 2 minutes after the parent recovered, the child still fails: it is no longer hidden and is told then, as opened, with the reason `still_failing`. A child that failed earlier than that, before anything was known of its parent, keeps its incident: it was told, and it resolves on its own. It is also listed under the parent's incident.

**Setting it.** On a server or device page, "Depends on" lists the parents, automatic ones included, with "Add" to choose a server, device, relay or check, and per check the same from its row. On a parent's page, "Behind it" lists the children and "Add what is behind it" picks several at once ("These servers are behind this switch"). Suggestions appear where the parent is obvious, each with one "Add" and never added without it: the Proxmox host a virtual machine or container runs on (ADR-0093), when it is monitored; the switches, routers and firewalls among the devices of the same site, from the template a device was added with (ADR-0149), now recorded on the device. Relays are not suggested: they are automatic.

**API.** `GET /api/v1/dependencies` (filtered by `server_id`, `relay_id` or `parent`), `POST /api/v1/dependencies` with one parent and one or more children, all made or none, `DELETE /api/v1/dependencies/{id}`, and `GET /api/v1/servers/{id}/dependencies` and `GET /api/v1/relays/{id}/dependencies` for what a page shows: parents with their state, children, suggestions, and whether it is unreachable now and why. Operators of both ends' sites add and remove them; viewers read them. Each change is audited (`dependency.create`, `dependency.remove`). All of it is in the OpenAPI document.

**Status.** The fleet status (ADR-0130) gains `unreachable`, after silenced and before offline: a server or device behind a parent that is down is unreachable, whatever else it shows, since what it shows is stale. `GET /api/servers` gives `state: "unreachable"` with `unreachable_because` (the parent), and the summary counts them. A child check behind a down parent leaves its server's state alone and says so on its row.

**Importing.** Zabbix trigger dependencies and PRTG dependencies become dependencies (ADR-0146):

- A Zabbix trigger that depends on another maps to a dependency of the check its trigger became on the check the other became: exactly when each trigger is the only one of its check, approximated otherwise ("the whole check depends on it: in the source only this trigger did"). A trigger on a host's availability (agent unavailable, ICMP ping) stands for that host as a whole, as a child and as a parent. A trigger of a template depends on its host's copy of the other; otherwise on the host its expression reads. A check its agent runs that depends on that agent's availability needs nothing: the agent being down stops it already.
- PRTG's "select object" setting on a device or a sensor maps to the chosen device, or to the chosen sensor's check (a ping sensor stands for its device). "Master object for parent" on a sensor puts the other checks of its device behind it. A group's setting becomes one dependency per device in it, approximated, since groups are not kept. The default, "use parent", needs nothing: what a remote probe watches is checked from its relay after the import, which is behind it automatically. An object PRTG shows as paused by a dependency right now is not paused: its dependency is moved instead.

What cannot be expressed (a dependency on a trigger or object that is not moved) stays "not moved" with its reason.

## Rejected

- **Opening child incidents marked as hidden**, as an incident state. Every count, list, follow-up and integration would have to learn to skip them, and a child that recovers with its parent would leave an incident nobody cares about. Not opening them is what people mean, and the engine already retries an incident it could not open at the next sample.
- **Unreachable only when all parents are down.** Redundant paths do not need a dependency; the common case, one uplink, would need a setting for "any".
- **Warnings as down.** Above.
- **A setting for the 2 minutes.** As for the offline delay (ADR-0072) and grouping windows (ADR-0080), one good default.
- **Inferring the topology** from SNMP neighbours (LLDP, CDP) or traceroutes. Discovery (ADR-0154) and network maps may propose dependencies later; they go through the same confirmation.
- **Groups as parents.** Groups of servers are a filter in MonitorFlow, not an object that can be down.

## Consequences

- A switch, router, firewall or host going down is one incident and one notification, listing what is behind it. What still fails 2 minutes after it is back is reported then.
- A child failing within a few seconds before its parent is recorded but not told, as hidden by the parent's incident.
- A device without a ping check can be a parent only through one of its checks.
- `servers` gains `device_template`; relays' implicit dependencies behave exactly as before; `relay.servers` stays in webhook payloads.
- The importer moves Zabbix and PRTG dependencies instead of listing them.
