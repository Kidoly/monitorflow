# ADR-0149: SNMP devices

Date: 2026-11-18
Status: accepted

## Context

Switches, routers, firewalls, printers, UPSs and NAS boxes cannot run the agent, but they all speak SNMP. ADR-0148 made them devices, watched by checks a probe runs, with ping and ports. What people moving from Zabbix, PRTG or Prometheus's SNMP exporter watch on them comes from a handful of standard MIBs: whether an interface is up and how much goes through it, the processor, memory and disks of a NAS, the battery of a UPS, the toner of a printer, a temperature. Setting each object by hand is what makes SNMP tools tiresome, so a device should come with sensible checks for its kind, picked by what it answers. Servers with the agent had no traffic figures of their own network interfaces either.

## Decision

**One probe kind, a group per check.** `monitorflow-snmp` builds checks of kind `snmp`, which a probe runs (ADR-0148) and agents never get. Its `what` parameter picks a group: `system` (sysUpTime, and `up` when the device answers at all), `interfaces` (IF-MIB: operational and administrative state, traffic in and out, errors and discards per second, speed and use of it, from the 64-bit counters of ifXTable when the device has them), `host` (HOST-RESOURCES-MIB: processor load, memory, and each fixed or network disk), `ups` (UPS-MIB: charge, runtime left, on battery, battery low), `temperature` (ENTITY-SENSOR-MIB sensors in degrees Celsius, named by entPhysicalName) and `printer` (Printer-MIB supplies: level over capacity). Each group is one sensor, so a device's interfaces are one check with an instance per interface rather than a check per port, and a new port appears by itself. Rates are computed by the sensor from two collections, so thresholds read bytes per second and a share of the link; the first collection, and one after a counter went back, give none. The crate uses `async-snmp`, pure Rust with RustCrypto, so no OpenSSL enters the build.

**v2c and v3.** v2c takes a community. v3 takes a user with authentication (SHA-1, SHA-256, SHA-384, SHA-512, or MD5 for old devices) and privacy (AES-128, AES-256 with the key extension net-snmp uses, or with Cisco's). Passwords are at least eight characters. There is no SNMPv1: every device of the last twenty years speaks v2c.

**Credentials belong to the device.** They are one row of `snmp_credentials` per device: version, port, user and protocols in clear, the community and passwords sealed together (ADR-0125, column `snmp_credentials.secret`). They are never returned by the API, which says only which are set, and never logged; check rows hold no credentials, and the API refuses them there, nor a `host` of their own: credentials go to the device's address only, and the server drops a `host` a row holds when it sends the check. When the server sends a probe its checks, it opens them and adds them to the parameters of the device's `snmp` checks, so a relay holds them in memory only, on the mTLS stream it already trusts with its site's agents.

**Templates per kind of device.** Switch, router, firewall, printer, UPS, NAS and generic each name their groups and default thresholds, in server code so the API and the web interface share them: for instance a switch gets system, interfaces and temperature, with a warning above 90% of a link's speed for 10 minutes and above one error a second; a UPS gets system and UPS, critical on battery and below 10 minutes of runtime; a printer a warning below 10% of a supply. Every device gets a critical alert when SNMP stops answering. Interfaces that are down raise nothing by default, since unused ports of a switch are down: a threshold on one interface's `oper_up` makes it alert.

**"Add a device" detects.** With an address and credentials, the web interface asks the probe to run a `detect` check once (`POST /api/devices/detect`): it reads sysDescr, sysName and sysObjectID, then whether each group answers, and the server suggests a template from that (a UPS-MIB is a UPS, Printer-MIB a printer, storage with interfaces a NAS, known vendors' descriptions a router or a firewall, many interfaces a switch). The person sees what answered, keeps or changes the template, and adds the device with its checks and thresholds in one request. A relay runs the test through two new envelope variants on its own stream, `ProbeTest` and `ProbeTestResult`, answered within the check's timeout; the instance holding the relay's stream runs it.

**Agents measure their own interfaces.** A new built-in agent kind, `net`, reads `/proc/net/dev` on Linux and the interface table of the IP helper API on Windows, and reports per interface the bytes, errors and drops per second in and out, loopbacks left out. It joins cpu, memory, disk and diskio among the checks every server runs, and the server page shows it.

**No traps.** Devices are polled; receiving traps is a task of its own.

## Alternatives rejected

- **One check per OID.** It is what makes other tools slow to set up, and a switch of 48 ports would need hundreds of rows.
- **A MIB compiler and arbitrary OIDs.** The common MIBs cover what people watch first; arbitrary objects can come later as their own kind, with names and units a person gives.
- **Credentials as check parameters.** Parameters are shown, audited by name, copied by the importer and sent to every probe of every check; the device has one set, and the sealed column keeps them out of everything else.
- **snmp2 or net-snmp bindings.** snmp2's v3 needs OpenSSL or brings its own crypto feature less used; net-snmp would need C libraries on relays and in the server image.
- **Detection on the central server only.** A device behind a relay is often unreachable from the central server, which is why the relay checks it.

## Consequences

- A device's SNMP checks run from one probe; two probes polling the same interfaces would share their incidents, since each sample names its interface as its instance.
- Changing credentials reaches the probes at their next read, within seconds.
- A detection test sent to a relay held by another central server instance is refused, and the person tries again; tests are rare and short.
- The importer can map SNMP items to these checks, and interface traffic items to `interfaces` or, for servers with the agent, to `net`.
