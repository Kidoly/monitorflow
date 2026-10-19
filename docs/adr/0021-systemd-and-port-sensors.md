# ADR-0021: systemd and port sensors

Date: 2026-10-19
Status: accepted

## Context

Phase 2 adds service checks next to the host sensors of ADR-0015. Operators want to know that a unit (sshd, nginx, a backup timer) is running and stays up, and to alert when it is not, including when it was never installed. They also want to know that a port answers, locally or on a neighbour. These sensors only make sense with explicit parameters, run external programs or touch the network, and their output must work with the threshold engine (ADR-0017), which keys a series by server, metric and instance and ignores other labels.

## Decision

### systemd

**Mechanism.** One `systemctl show --no-pager --property Id,LoadState,ActiveState,SubState,NRestarts,StateChangeTimestampMonotonic -- <units>` per collection, for all the sensor's units, spawned directly with `tokio::process` (no shell, arguments passed as a vector after `--`, `LC_ALL=C`, stdin closed), killed on drop and bounded by the collection deadline. `systemctl` is the stable interface for this: its `show` output is documented as machine-readable and has kept its format across the versions we support (RHEL 8's 239 to current 26x). D-Bus was rejected: it needs a D-Bus client dependency (zbus and its async stack, or libdbus through FFI) for four properties, and `systemctl` speaks to the same manager anyway. Output is parsed as `Key=Value` lines rather than with `--value`, because properties a unit type does not have (`NRestarts` on a timer) or an old systemd does not know are left out, which would shift positional values. Each unit's block starts with `Id=`, and blocks come in the order asked; units are matched by position because an alias reports its target's `Id` (`dbus.service` shows `dbus-broker.service`). Lines the parser does not know are ignored.

**Units.** Parameter `units`, a comma-separated list, required, at most 64. A name without a known type suffix (`service`, `socket`, `timer`, …) gets `.service`. Names are checked against systemd's charset (`[A-Za-z0-9:_.\@-]`, at most 255 bytes) and may not start with `-`, `.` or `@` nor be a template without instance (`getty@.service`), so nothing can be read as an option or a glob. A unit listed twice (`ssh` and `ssh.service`) is refused, as it would produce duplicate series.

**Metrics.** Per unit, with the unit name (as configured, after the suffix is added) as instance:

| Metric | Value |
| --- | --- |
| `systemd_loaded` | 1 when `LoadState` is `loaded` |
| `systemd_active` | 1 when `ActiveState` is `active`, `reloading` or `refreshing` |
| `systemd_failed` | 1 when `ActiveState` is `failed` |
| `systemd_restarts` | `NRestarts`, automatic restarts counted by systemd; services only |
| `systemd_state_age_seconds` | Now minus `StateChangeTimestampMonotonic`, both `CLOCK_MONOTONIC`; omitted when the unit never changed state |

The unit is the instance, not a `unit` label, because thresholds target `(metric, instance)`: with a label, every unit of one sensor would share a series in the engine, and a threshold could not name one unit. `reloading` counts as active so a reload does not look like an outage. `restarts` has no `_total` suffix: systemd resets it, so it is a gauge (ADR-0015). Monotonic timestamps avoid parsing the localised wall-clock form, and `--timestamp=unix` is missing from the older systemd versions we support.

**Failure semantics.**

- A unit that does not exist is data, not an error: `systemctl show` answers `LoadState=not-found`, so it reports `loaded 0`, `active 0` and a threshold `systemd_active < 1` alerts on it.
- A host not booted with systemd (no `/run/systemd/system`, the sd_booted(3) test, as in most containers) or without `systemctl` is a permanent error: the scheduler logs it and stops the sensor until it is configured again.
- A non-zero exit (the manager restarting, D-Bus unavailable), a timeout or output that does not match the units asked is transient, retried on the next tick. The error shows the first line of stderr, truncated.

**Example.** `kind = "systemd"`, `name = "systemd"`, `params = {"units": "ssh,nginx,backup.timer"}`; threshold `monitorflow_systemd_active < 1` with instance `nginx.service`, or no instance for every unit. Locally: `monitorflow-agent sensors probe systemd --param units=ssh,cron`.

### port

**Mechanism.** A TCP connect with `tokio::net::TcpStream`, closed as soon as it is established; nothing is sent, so any TCP service can be checked without a protocol client (a service may log a connection closed early). Parameters: `port` (required, 1 to 65535), `host` (default `127.0.0.1`; an IPv4 or IPv6 address, brackets optional, or a host name of letters, digits, `-`, `_` and dots, at most 253 bytes) and `protocol` (`tcp`, the default and only value). A host name is resolved once per collection with the system resolver on the blocking pool; while a lookup is still running from an earlier collection, the sensor reports down without starting another, so a dead DNS server holds one thread, not one per tick. Addresses are tried in resolver order (`localhost` may give `::1` before `127.0.0.1`) until one connects.

**Deadline.** Lookup and connects stop 200 ms before the collection deadline (at most half the remaining time), so a port that does not answer reports `up 0` instead of being cancelled by the scheduler with no sample, which could not alert. The connect timeout is therefore the configured sensor timeout (ADR-0012, 10 s by default) minus that margin; there is no separate parameter.

**Metrics.** With label `target` (`host:port`, `[v6]:port`), and the configured instance:

| Metric | Value |
| --- | --- |
| `port_up` | 1 when a connection was established |
| `port_connect_seconds` | Time the successful connect took, DNS excluded; omitted when down |

One port per configured sensor, so the series is already unique per `(name, instance)` and thresholds work on `port_up < 1`; give each check its own instance (`ssh`, `postgres`) or name. This amends ADR-0015's expectation that a port check emits one unsuffixed value.

**Failure semantics.** Refused, unreachable, timed out and unresolvable targets are all `up 0`, logged at debug level, never a collection error: from this host, the service is not reachable, which is what the check is for. A host name that never resolves (a typo) therefore shows as down and alerts.

**UDP** is out of scope: there is no connection to establish. A UDP probe only learns "closed" from an ICMP port unreachable, which firewalls drop, and "open" only from a reply, which needs a payload specific to each protocol (DNS, NTP, SNMP). Without one, a silent port is ambiguous. `protocol = "udp"` is refused with a pointer here; protocol-aware checks can come as their own sensors.

**Network code in `crates/sensors`.** Architecture §2 kept the crate free of network code so it stays portable and has no transport stack. That intent is about the upstream link (gRPC, TLS, HTTP clients), which stays out. A TCP connect through tokio is a measurement, portable to Windows, and adds no dependency, so it lives here; the architecture document and CLAUDE.md now say "no transport code". Plaintext listeners remain forbidden; the sensor only connects.

**Example.** `kind = "port"`, `name = "port"`, `instance = "postgres"`, `params = {"host": "db1.internal", "port": 5432}`; threshold `monitorflow_port_up < 1` with a duration of 2 minutes. Locally: `monitorflow-agent sensors probe port --param port=22`.

### Common

**Defaults.** Neither sensor is in the built-in defaults of ADR-0016: they need parameters that only the operator knows. `registry::build` refuses them without parameters, which the API reports when a row is written. `sensors probe` without kinds skips kinds that need parameters; `--param name=value` passes parameters to the kinds listed.

## Consequences

- One process per collection per systemd sensor, whatever the number of units; at the 60 s default this is negligible.
- A unit renamed on the host shows as not-found under its old name until the configuration changes, which is the alert we want.
- Tests use a fake runner, output captured from systemd 262, and a stand-in script for `systemctl`; a test against the real systemd runs with `MONITORFLOW_TEST_SYSTEMD=1`.
- Units of the user manager (`systemctl --user`) are not supported.
- Port tests use local listeners: open, closed, a full accept queue for a hanging connect, and a spent deadline. The hanging case is skipped where the kernel does not drop SYNs on a full queue.
- TLS handshakes, HTTP checks and UDP protocols are follow-ups, each as its own sensor.
