# ADR-0111: Agent-side limit on where port checks connect

Date: 2026-11-12
Status: accepted; amends ADR-0021 and ADR-0024

## Context

A security audit (part 4, agent endpoint) found that the `port` sensor (ADR-0021) connects to any address or host name and any port the server names. The `script`, `log` and `eventlog` sensors are bounded by limits only the host's administrator sets in `agent.toml` (ADR-0024, ADR-0048); the `port` sensor was not.

A malicious central server, or anyone able to push a sensor configuration, could turn every agent into a TCP connect scanner and DNS resolver from its own network position: internal hosts, `169.254.169.254`, services on localhost. `up` and `connect_seconds` come back upstream and tell open from closed.

## Decision

**Setting.** `sensors.port_targets` in `agent.toml` (environment `MONITORFLOW_SENSORS_PORT_TARGETS`, comma-separated), a list of entries:

- an address range in CIDR notation or one address: `10.0.0.0/8`, `192.168.1.10`, `fd00::/8`, `::1`;
- or a host name, compared without case: `db.example.com`;
- each optionally followed by a port or a range of ports: `10.0.0.0/8:22`, `192.168.0.0/16:8000-8999`; IPv6 in brackets then, `[fd00::/8]:443`. Without ports, any port.

An entry with bits set after its prefix (`10.0.0.1/8`) is refused rather than rounded, since the administrator probably meant something else. The agent parses the list when it starts and refuses to start on an unreadable entry, like any other configuration error.

**Checks.** When a configuration is applied, a `port` check is refused (`Refused`, reported in the acknowledgement and retried when the host's limits change) unless:

- its host is an address inside a listed range for its port;
- or its host is a listed host name for its port;
- or its host is another host name and some range is listed for its port.

In the last case, the addresses the name resolves to are checked at every collection, and those outside the ranges for the port are skipped; when none is left the check reports `up 0` and the agent logs a warning. A name that resolves elsewhere later, or a numeric name like `2130706433` that the resolver turns into `127.0.0.1`, cannot get past the list. A listed host name connects to whatever it resolves to: the administrator trusts the name and its DNS. IPv4-mapped IPv6 addresses (`::ffff:127.0.0.1`) are matched as IPv4.

An empty list disables the sensor, as for `log_dirs`.

**Default: unset, any target, as before.** The checks people configure most are of neighbours (a database, a load balancer, a peer across the site) whose addresses the agent cannot guess, and the installers write no `[sensors]` section. A restrictive default such as loopback only would refuse, on update, every existing check of another machine and break monitoring across fleets without warning. Private ranges as a default would still allow almost all of the scanning the audit describes. So the setting is hardening the administrator opts into, documented next to `log_dirs` in the example configuration and the agent guide.

## Consequences

- An administrator who sets `sensors.port_targets` bounds what a compromised server can probe through this agent to the listed ranges, names and ports.
- When the list holds a range for a port, the server can still make the agent resolve an unlisted name it chooses, and learn only whether some address it gets is in the range and answers. DNS queries for names of the server's choice remain possible; listing host names only, without ranges for their ports, stops them.
- Without the setting nothing changes. A later release may change the default once the web interface can show agents' limits; that would need its own ADR.
- `registry::build` on the server still checks parameters only; the limit lives on the agent, which the server's configuration cannot change.
