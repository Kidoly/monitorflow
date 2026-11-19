# ADR-0157: Custom SNMP checks

Date: 2026-11-19
Status: accepted; extends ADR-0149 (SNMP devices) and ADR-0146 (what the importer moves)

## Context

ADR-0149 watches what the common MIBs say: interfaces, host resources, UPS, temperature sensors and printer supplies, as six groups. It left "arbitrary OIDs" for later. Much of what people watch on a device is in its vendor's own MIB: the sessions of a firewall, the state of a NAS's RAID volumes, the battery temperature of a UPS in its maker's MIB, a sensor that reports tenths of a degree. None of it can be watched today, and the importer leaves every such item behind as "cannot be moved" (`snmp_object`).

Tools people move from watch these objects in two ways. One makes a person write a key, a type and a preprocessing chain per item, one OID at a time. The other has a sensor with up to ten channels, each an OID with its own name, unit, scaling and limits, a lookup that turns an enumerated value into a named state, a table sensor that follows the rows of a table, and a MIB library and an OID browser so nobody types OIDs from memory. People coming to MonitorFlow know and like the second far more, and its sensor-with-channels model fits ours: one check, several values, each with its own alerts.

Reading any OID a person types has its own risks. The objects and their texts come from a device, the MIB files from anywhere: both are untrusted input. A MIB is a program-like text in ASN.1 macros that some tools compile with recursion and network fetches of imported modules.

## Decision

### One check, up to ten channels

**A new probe kind, `snmp_custom`**, of `monitorflow-snmp`, run by the central server or a relay like the `snmp` checks (ADR-0148), never by agents, with the device's credentials added when the server sends the check to its probe (ADR-0149). A check reads up to **10 channels** at each collection, in one request for scalars. Each channel has:

| Setting | Parameter | Meaning |
| --- | --- | --- |
| OID | `oidN` | A numeric OID, like `1.3.6.1.4.1.12356.101.4.1.8.0`; names are resolved by the interface and the API, never by the probe |
| Name | `nameN` | What people read: "Sessions" |
| Key | `keyN` | The metric suffix, `[a-z][a-z0-9_]{0,31}`, from the name when not given: the channel "Sessions" of the check `snmp_fw` writes `monitorflow_snmp_fw_sessions` |
| Kind | `typeN` | `gauge` (the value as read, the default), `counter` (a rate per second) or `text` |
| Unit | `unitN` | Free text shown beside values, at most 16 characters: `°C`, `sessions`, `B/s` |
| Scaling | `multiplyN`, `divideN` | Positive factors applied to numbers: divide by 10 for tenths of a degree, multiply by 1024 for KiB to bytes |
| Lookup | `lookupN` | The lookup that names its values and gives their states (below) |
| Expected text | `matchN`, `expectN`, `severityN` | For text: `contains`, `not_contains`, `equals`, `not_equals`, `matches` or `not_matches` (a regular expression of at most 256 bytes, without look-around or back-references, linear time); not as expected is `warning` or `critical` (the default) |

`N` runs from 1 to 10. Check rows hold no credentials and no lookup definitions, only references, which the API refuses otherwise (as for `snmp` checks).

**Value kinds.** A `gauge` takes integers, Gauge32, Counter32 and Counter64 read as they are, TimeTicks in hundredths of a second, and an OctetString holding a number (`"23.5"`, as many vendors write floats). A `counter` keeps its last raw value and time, and reports the difference per second from the second collection on; a counter that went back (a reboot, a wrap of a 32-bit counter) gives no value for that collection, like the interfaces group. Scaling applies after the rate. A `text` is any OctetString, IpAddress or OID, read as UTF-8 with what does not decode replaced.

**What a channel writes.** A number writes the metric `<key>`. A channel with a lookup or an expected text also writes `<key>_state`: 0 for OK, 1 for warning, 2 for critical, with a `text` label holding the value's name or the text itself, cut at 64 characters without control characters. A text channel without either writes `<key>_state` at 0, so its text is shown. A channel whose object does not exist on the device writes nothing, and the check's error says which.

**Thresholds.** Each channel's limits are ordinary thresholds of the device on the channel's metric (ADR-0017): warning and critical, above or below, for how long. A `_state` metric gets warning at 1 and critical at 2 by default, so a lookup or an expected text alerts without any number to type. Every richer rule of ADR-0153 (windows, comparison with last week, forecast, no data, combined conditions) applies to a channel like to any metric, as do silences (ADR-0151), dependencies (ADR-0152), business services and reports (ADR-0155), which key on the check's name and instance.

### Lookups

**A lookup names the values of an enumerated object, each with its state**: `1 = OK, OK`, `2 = Warning, warning`, `3 = Failed, critical`, and what any other value is (unknown, warning by default). An entry matches an integer, a range of integers (`10..19`) or an exact text. A lookup has at most 100 entries, names of at most 64 characters.

**Built-in lookups** cover common states and cannot be changed: OK, warning, failed (1, 2, 3); TruthValue (true, false); IF-MIB status (up, down, testing, unknown, dormant, not present, lower layer down); HOST-RESOURCES device status (unknown, running, warning, testing, down); UPS-MIB battery status (unknown, normal, low, depleted); enabled and disabled (1, 2). Their IDs are `builtin:<name>`.

**A person's own lookups** belong to a customer, like business services (ADR-0155), and any check of its sites may use them. Uploading a MIB offers each enumeration it declares as a lookup, its states guessed from the names (`ok`, `normal`, `up`, `running` are OK; `fail`, `down`, `crit`, `error`, `depleted` are critical; the rest warning) for the person to confirm. A lookup used by a check cannot be deleted. The server expands a check's lookups into its parameters (`lookupN_map`) when it sends the check to a probe, so changing a lookup reaches the probes within seconds, as credentials do.

### Tables

A check can instead **follow a table**: `table` names the table's entry OID (`1.3.6.1.4.1.6574.3.1.1` for a NAS's RAID volumes), `name_column` the column whose values name the rows (a description or a name; the row's index when absent), and each channel's `oidN` is a column of the table. The probe walks the name column and each channel's column, and writes one instance per row, named by its name column (cut at 128 characters, its index added when two rows share a name). Rows that appear are watched from the next collection; rows that go away stop reporting, as interfaces do (ADR-0149). A table follows at most 500 rows; past it, the check says it was cut. A channel's threshold covers every row, and a threshold on one instance overrides it for that row, as for interfaces.

### Picking objects without a MIB compiler in your head

**Test.** Before saving, "Test" reads the channels' OIDs, or walks the table, from the check's probe with the device's credentials, and shows for each the type, the raw value and what the channel would write with its scaling and lookup. **Browse** walks a subtree of the device (the whole `1.3.6.1.4.1` enterprise tree by default, or any OID) and lists what answers, at most 2,000 objects, each with its name when a MIB knows it; clicking one adds it as a channel with the kind, unit and lookup the MIB suggests. Both are one-off tests of a new probe kind, `snmp_read`, through `ProbeTest` on the relay's stream or on the central server (ADR-0149): no protocol change, and a relay older than this answers that it does not run the kind. They change nothing and are not audited, and need the role that changes the device's checks, as detection does.

**MIB files.** Uploading MIB files names objects in Browse, in Test and in the check, and offers their enumerations as lookups. The parser is MonitorFlow's own, in `monitorflow-snmp`, for the SMIv1 and SMIv2 subset that names objects: `OBJECT IDENTIFIER` assignments, `OBJECT-TYPE`, `MODULE-IDENTITY`, `OBJECT-IDENTITY`, `NOTIFICATION-TYPE` and `TEXTUAL-CONVENTION` with their `SYNTAX`, enumerations, `UNITS`, `MAX-ACCESS` and the start of `DESCRIPTION`. It reads text only: it never fetches an imported module, runs nothing, and is bounded (a file of at most 1 MiB, 20 files per upload, 20,000 objects per file, nesting of at most 32 levels, no recursion on input). Imports resolve against a built-in base (SNMPv2-SMI, RFC1155-SMI, SNMPv2-TC, RFC1213-MIB and the MIBs of ADR-0149) and the modules the customer uploaded; a file importing a module nobody uploaded is refused with the list of what is missing, so the person uploads it in the same request. MIBs belong to a customer, like lookups. The parsed objects are stored (`snmp_mib_objects`), not the file.

**A small library** of common vendor objects is built in, by OID with a name, kind, unit, scaling and lookup: net-snmp's UCD-SNMP-MIB (load averages, memory), Synology (system status and temperature, RAID and disk tables), APC PowerNet (battery capacity, temperature and runtime, output status), Fortinet (processor, memory and sessions) and Cisco (processor and temperatures). It holds facts about objects, never a vendor's MIB file, so no license question arises.

### Credentials: SNMPv1 too

People moving from other tools still have v1 devices (old UPS cards, some printers). `version` takes `v1` beside `v2c` and `v3`, with a community, for every SNMP check and for detection. ADR-0149 had left it out on the belief that every device of the last twenty years speaks v2c; imports showed otherwise.

### Storage, API and permissions

`snmp_lookups` (customer, name, entries as JSON, what any other value is) and `snmp_mibs` (customer, module name, file name, who uploaded it, when) with `snmp_mib_objects` (module, name, OID, syntax, enumeration, units, access, a 500-character description). Check rows stay in `sensor_configs` with kind `snmp_custom` and their parameters, so the importer, the check catalogue and the generic check routes need nothing new to carry them.

The API under `/api/v1`, in the OpenAPI document of the same PR:

- `GET`, `POST /api/devices/{id}/snmp-checks` and `PUT`, `DELETE /api/devices/{id}/snmp-checks/{name}`: a custom check as a structured body (its channels with their limits), written with its thresholds in one transaction, and read back with each channel's last value, state and text. The generic check routes read and write the same rows as flat parameters.
- `POST /api/devices/{id}/snmp/read`: Test and Browse, `{"oids": [...]}` or `{"walk": "1.3.6.1.4.1", "max": 500}`, from the device's probe.
- `GET`, `POST /api/snmp-lookups` and `GET`, `PUT`, `DELETE /api/snmp-lookups/{id}`; `GET`, `POST /api/snmp-mibs`, `GET`, `DELETE /api/snmp-mibs/{id}`; `GET /api/snmp-objects?q=...&oid=...`, the names of the MIBs and the library.

Viewers of a device read its checks; operators of its site change them and run Test and Browse. Lookups and MIBs are read by everyone who reads a site of their customer and changed by operators of the customer as a whole, as business services are. Every change is audited (`snmp_check.create`, `.update`, `.delete`, `snmp_lookup.create`, `.update`, `.delete`, `snmp_mib.upload`, `.delete`); Test and Browse are not.

### The web interface

A device's page lists its custom checks with each channel's value, unit and state. "Add a custom check" opens one form: the check's name, "Checked from", the interval (1 minute by default), "One value per object" or "A table", then the channels as rows, each with its OID (typed, picked in Browse or from the library), name, kind, unit, scaling, lookup or expected text, and limits in words ("Warn above", "Critical above", "for 5 min"). Test runs on the form as it stands. Lookups and MIBs have their own page under Monitoring settings, with the built-in ones listed first. A single company sees no customer anywhere in it.

### What the importer moves

| Source | Becomes |
| --- | --- |
| PRTG SNMP Custom (`snmpcustom`) | One channel: OID, unit, multiplication and division, delta as `counter`, limits: exact |
| PRTG SNMP Custom Advanced (`snmpcustomadvanced`) | Up to 10 channels with their OIDs, units, factors, kinds and limits: exact |
| PRTG SNMP Custom String (`snmpcustomstring`) | A `text` channel with "must include", "must not include", equals or a regular expression: exact; PRTG's warning or down becomes `warning` or `critical`; a check of the string's length or of a number inside it is approximated as a text check, with the reason |
| PRTG SNMP Custom Lookup and String Lookup (`snmpcustomlookup`, `snmpcustomstringlookup`) | A channel with a lookup made from the PRTG lookup file when the export has it (entries, ranges, states None/Ok as OK, Warning as warning, Error as critical, Unknown as warning), or a built-in one of the same values and names: exact; a lookup the files read lack leaves the channel without one, with the reason (lookup files are uploaded beside the configuration file; the API has none) |
| PRTG SNMP Custom Table (`snmpcustomtable`) | A table check with its identification column and up to 10 column channels: exact |
| PRTG SNMP Library (`snmplibrary`) sensors | The channels of the library sensor's OIDs as an advanced check, or a table when its OIDs are columns: exact for OIDs, approximated for a library channel computed from several OIDs |
| Zabbix SNMP items (`snmp_oid`), `get[OID]` or a plain OID | A custom check of one channel per item, so each item stays one line of the import; "Change per second" preprocessing as `counter`, "Custom multiplier" as scaling, text value types as `text`, value maps as lookups naming the values (their states stay OK: Zabbix judges values in triggers): exact; other preprocessing approximated, with the step it ignores |
| Zabbix `walk[OID,...]` and SNMP item prototypes (`...{#SNMPINDEX}`) | A table check of that column, rows named by their index: approximated (one table check, not an item per row); through the API, the prototype stands for the items discovered from it |
| Prometheus snmp_exporter modules, with `snmp.yml` uploaded | For each target walked with a module: its scalar metrics as one check and each table as another, `type` (`gauge`, `counter`, `DisplayString`...) as the kind, the first text `lookups` of the table as the name column, `scale` as scaling, and `enum_values` as lookups naming the values: exact for those; regular expression extraction approximated; `if_mib` stays the interfaces group |

An item of another kind, or beyond 10 channels of a check, says so and is listed "cannot be moved" with its reason, as today.

## Alternatives rejected

- **A check per OID.** It is the model that makes other tools slow to set up and hard to read: a NAS with six values would be six checks with six alerts to set. A check with channels is one row with one schedule, one request per collection and one place to read.
- **A library to compile MIBs** (libsmi, a Python compiler). They pull C or a runtime into the server and relays, fetch imports over the network, and compile more than naming objects needs; a bounded parser of the naming subset is a few hundred lines with no dependency.
- **Shipping vendors' MIB files.** Their licenses vary and often forbid redistribution; a short list of OIDs with names is enough to start, and people upload the files they have.
- **Lookups applied by the server at ingest.** The probe already has the value; computing the state there keeps samples self-contained, so graphs, thresholds and the preview read stored series as for every other check.
- **Text values as series values.** VictoriaMetrics stores numbers; a state with the text as a label shows the text and alerts on the state, the way website checks carry their failure reasons.
- **Lookups and MIBs for the whole installation.** One customer of an MSP would see, or change, another's names and states.

## Consequences

- Any object a device answers can be watched, with alerts in its own words, and imports of custom SNMP sensors stop being lost.
- A check with a lookup or an expected text writes one series per text it reported: a value that changes often (a counter shown as text) costs a series per value, which the 64-character cut and the state's own series keep small.
- Test and Browse send real requests to the device from its probe: a walk of the whole enterprise tree of a big switch can take seconds, and stops at 2,000 objects or the test's timeout.
- A relay older than this does not start `snmp_custom` checks and says so in its acknowledgement; the device's page shows it, and updating the relay fixes it.
- SNMPv1 has no 64-bit counters: the interfaces group falls back to the 32-bit ones on v1 devices.
