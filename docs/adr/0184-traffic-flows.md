# ADR-0184: Traffic flows

Date: 2026-11-24
Status: accepted; extends ADR-0148 (probes and devices) and ADR-0149 (SNMP devices)

## Context

SNMP says how much goes through an interface (ADR-0149), not who sends it or what it is. When a branch link is full, the question is which address and which application fill it. Routers, firewalls and switches answer it themselves: they export NetFlow (v5, v9), IPFIX or sFlow over UDP to a collector. Zabbix needs an external collector for this, PRTG has NetFlow, sFlow and IPFIX sensors, and people moving from PRTG expect it.

Three constraints shape the design:

- **Volume.** A busy router sends thousands of flow records a second. The central server is sized at about 8,000 agents on 2 CPUs and 4 GB (`docs/load-test.md`); raw flows from a few sites would cost more than the whole fleet.
- **Reach.** An exporter usually sits on a branch network that the central server does not reach, which is why relays run probe checks (ADR-0148).
- **Transport.** Flow protocols are plain UDP, sent by devices that cannot do TLS, let alone mTLS.

## Decision

### Probes receive flows

A device is set as a **flow exporter**: a `flow_exporters` row naming its probe (the central server, or a relay of its site, chosen like a probe check's: the site's first relay by default) and, when needed, a `source_address` its flows come from instead of its own address (a router exporting from a loopback). The API is `GET`, `PUT` and `DELETE /api/devices/{id}/flow-exporter`, audited as `device.flow_exporter_update` and `device.flow_exporter_delete`.

The exporters of a relay travel in its `ProbeConfig`, in a new field `flow_exporters` (3): the same message, the same revision and the same "believed only for its devices" rule as its checks. Older relays ignore the field. The central server runs its own exporters on the leader, like its own checks.

`crates/flow` (`monitorflow-flow`) holds the collector. It **opens its UDP ports only while its probe has exporters**, on 2055, 6343 and 4739 by default (`[flows]` in the relay's and the server's configuration: `listen`, `netflow_port`, `sflow_port`, `ipfix_port`, 0 to turn one off). Every port takes every format: the version at the start of a datagram says which one it is. A datagram is taken only from an exporter's address (names are resolved, again every 10 minutes); for sFlow, the agent address inside the datagram is also accepted, as an sFlow agent may send from another interface.

### Decoding

NetFlow v5 (header sampling interval applied), NetFlow v9 and IPFIX with templates per exporter address, observation domain and template ID (IPFIX variable-length and enterprise fields read past, withdrawals honored, options records giving `samplingInterval`, `samplingPacketInterval` or `samplerRandomInterval` applied to the exporter's records that carry no rate of their own), and sFlow v5 flow samples (compact and expanded, raw Ethernet, IPv4 or IPv6 headers with up to two VLAN tags, sampled IPv4 and IPv6 records) counted as `sampling_rate` packets of the frame's length. sFlow counter samples are skipped: SNMP reads interfaces.

The decoders never index a slice: every read goes through a bounded reader that returns nothing past the end, lengths and counts are checked against what is left, a template of empty fields decodes nothing rather than looping, and templates are bounded (16,384 in all; those of an address that stops being an exporter are forgotten). Unit tests feed each decoder 300,000 mutated and random datagrams from a fixed seed; no dependency is added for it.

### Aggregation on the probe

Flows are added up **per exporter and one-minute window, on the probe**. At the end of a window, each exporter that sent something gives one `FlowReport`: its bytes and packets, the **20 largest keys of each dimension** (source address, destination address, application, protocol, input and output interface as the exporter's ifIndex) and its **100 largest conversations** (source, destination, application, interfaces). The rest of a dimension is the total minus its keys, so totals stay exact. An application is the protocol and the service port: the one of the pair that a built-in list of about 60 services knows, else the lower one (`tcp/443`, named HTTPS). Memory is bounded at 50,000 keys per dimension and exporter in a window; keys past that count in the total only.

Reports go up as a new envelope variant, `FlowReports` (26), on the relay's own stream. A relay keeps an hour of them while its upstream is down and drops newer ones after that: flows are about now.

### Storage

PostgreSQL, not VictoriaMetrics: an address per series would make cardinality grow with every client a router sees. `flow_tops` holds one row per window, dimension and key (and a `total` row per window); `flow_conversations` one per conversation. Minutes are kept **48 hours**; every 10 minutes the leader sums the minutes older than that into hours, keeping the 20 largest keys of each dimension and the 100 largest conversations of each hour, and drops hours older than **31 days**.

Size per exporter, at most: 6 dimensions × 20 keys + 1 total = 121 rows and 100 conversations a minute, about 320,000 rows (some 40 MB with indexes) for 48 hours, and 221 rows an hour, about 165,000 rows (20 MB) for 31 days. Fifty exporters stay under 3 GB, and most exporters fill far fewer keys. The server writes about 220 rows a minute per exporter in one transaction, which is light next to the ingest of 8,000 agents.

### Reading

`GET /api/devices/{id}/flows` takes a period (at most 31 days), a dimension for the chart, an optional address and a number of keys. It answers the totals, the largest keys of each dimension with their share, and a chart of the 5 largest keys of the chosen dimension and the rest, in bits per second, at a step that gives at most 120 points (at least an hour where minutes are already summed). With an address, every view counts only the traffic it sent or received, from the conversations. Interfaces are named from the device's SNMP interfaces check, which now reports each interface's `if_index`.

The device page gets a **Flows** tab: who receives them and when the last minute came, the period (1 hour to 30 days), a stacked chart, a table per dimension, and a click on an address to filter every view on it.

### Alerts

Each report also writes, for the device, `monitorflow_flow_address_bits_per_second` per address among its largest sources and destinations (instance: the address, sent and received added up) and `monitorflow_flow_bits_per_second` for the whole device, with the probe's label, through the usual ingest. An address that drops out of the largest gets a 0 once, so its incident can resolve. A threshold on the first metric with a duration ("above 100 Mbit/s for 5 minutes", any instance) is the alert, judged by the existing engine (ADR-0017, ADR-0153); the Flows tab adds one in a click. Only the largest addresses of each minute are series, so their number stays bounded.

### Plain UDP

Flow listeners are a new exception to "every link is mTLS", beside the mirrors (ADR-0030, ADR-0034) and port 80 (ADR-0068): devices cannot do otherwise. They listen only while a probe has exporters, accept only exporters' addresses, carry traffic metadata and no credentials, and change nothing but the counts of that device's flows. A forged datagram from a spoofed exporter address can at worst skew those counts.

### Importer

PRTG's NetFlow, jFlow, sFlow and IPFIX sensors, custom ones included, are recognized and listed with what to do instead (turn on Flows on the device's page), as the reception is a setting of the device and its probe, not a check.

## Alternatives rejected

- **Raw flows to the server, or to a flow database.** It costs orders of magnitude more at the central server and on the link from each site, for detail the tab does not show.
- **Flows in VictoriaMetrics with an address label.** Unbounded cardinality: every client address a router sees becomes series. Only the largest addresses are written there, for alerts.
- **An exporter as a check row of `sensor_configs`.** A check produces samples on a schedule; an exporter is a setting of the probe's listener, and listing it among checks would invite "no data" rules and intervals that mean nothing for it.
- **One port per format.** Exporters are often set to one port whatever they send; reading the version is cheap and exact.
- **A fuzzing dependency (proptest, cargo-fuzz).** A fixed-seed mutation loop over valid datagrams finds the same class of bugs in the unit tests, in CI, without a nightly toolchain or a new crate.

## Consequences

- A relay that receives flows needs its UDP ports open from the devices' networks; the relay and server guides say which.
- Two probes cannot receive one device's flows: the exporter names one, as a device sends to one collector.
- Traffic of addresses outside the largest of each minute shows only in "Other"; a slow scan of many addresses is not seen per address.
- With several central servers (ADR-0089), the leader receives the central server's own exporters: point them at an address that follows it, or use a relay. The compose file publishes the three UDP ports (`NETFLOW_PORT`, `SFLOW_PORT`, `IPFIX_PORT` move them).
- Windows are by arrival time at the probe, not by the flows' own start and end, so a long flow exported at its end lands in one minute.
