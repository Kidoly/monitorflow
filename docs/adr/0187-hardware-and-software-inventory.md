# ADR-0187: Hardware and software inventory

Date: 2026-11-24
Status: accepted

## Context

Phase 20 asks to know what each server is made of (model, serial number, processor, memory, disks, network cards, firmware and BIOS, warranty) and what it runs (installed software and versions, services, Windows features, licences), searchable across the fleet, with changes kept and exported, and synced to the PSA as assets.

Some of it exists already: agents send their installed packages (Linux) or Windows updates in `PackageReport` (ADR-0031), third-party apps (ADR-0138), and running services and listening ports for check recommendations (ADR-0134). Those reports are sent whole each time. The fleet has to stay within what `docs/load-test.md` measured: about 8,000 agents on a server of 2 vCPU and 4 GB.

## Decision

**A list of items, one model for everything.** The inventory is a list of items, each with a kind, a key unique within its kind and text fields (`proto/monitorflow/v1/inventory.proto`). Kinds: `system` (maker, model, serial numbers, chassis, motherboard, BIOS, operating system, kernel, total memory), `processor`, `memory` (modules), `disk`, `nic`, `software` (Windows programs), `service`, `feature` (Windows features) and `licence`. The fields of each kind are listed in the OpenAPI document (tag `inventory`). One model means one diff, one table, one history and one search for every kind; a new kind is a new name, not a new message.

**Linux packages stay where they are.** Installed Linux packages (and Windows updates) are already in `server_packages`, a few hundred to a few thousand rows per server. Copying them into the inventory would double the largest table of the database. The inventory leaves them out; the fleet search reads `server_packages` and `server_third_party_packages` next to the inventory's Windows programs, and package changes go into the inventory's history (below).

**Read-only, without root where possible.** On Linux the agent reads `/sys/class/dmi/id`, `/proc/cpuinfo`, `/proc/meminfo`, `/etc/os-release`, `/sys/class/net`, and runs `lsblk --json`, `ip -json address` and `systemctl list-unit-files`, as its own user. The DMI serial numbers (`product_serial`, `board_serial`, `chassis_serial`) are readable by root only: the systemd unit copies them into the agent's runtime directory before starting it, readable by the agent alone, exactly as it already copies the system UUID (ADR-0093). No new privilege, no sudo rule. Memory modules need `dmidecode` as root and are left out on Linux: the total is reported. Inside a container the firmware is the host's, so DMI is not read. On Windows a PowerShell script built into the agent (no input from the server) reads CIM classes (`Win32_ComputerSystem`, `Win32_BIOS`, `Win32_BaseBoard`, `Win32_Processor`, `Win32_PhysicalMemory`, `Win32_DiskDrive`, `Win32_NetworkAdapter`, `Win32_Service`, `Win32_OptionalFeature`, `Win32_ServerFeature`, `SoftwareLicensingProduct`) and the registry's Uninstall keys. It never uses `Win32_Product`, which checks and may repair every MSI package. Its JSON is parsed by plain Rust tested on every platform. Licences are what Windows says it is licensed for, with the last five characters of the key Windows itself shows; on Linux nothing readable without root says it, so there are none.

**Sent as a hash, then as changes.** Both sides compute the same canonical form (items sorted by kind and key, fields by name, empty fields dropped, the same bounds) and its SHA-256 (`common::inventory`). The agent collects every hour on Linux and every 4 hours on Windows, and:

- when a stream opens, and at least once a day, it sends only the hash;
- when the inventory changed, it sends the changes since the last one it sent (items added or changed whole, keys removed), or the whole inventory when that is smaller;
- the server answers a report it cannot use (a hash it does not hold, a delta from another base, a result whose hash does not match) with `InventoryRequest`, naming the hash it holds; the agent then sends the changes since that one if it still has it, else the whole inventory.

After an agent restart, nothing is sent but the hash unless something changed meanwhile. A collection where a command fails is skipped rather than sent, so a passing failure does not read as hardware removed. The server takes at most one report a minute per stream, answers to its own requests aside, and writes through the stream's write budget (`WRITES_PER_STREAM`), like package reports.

**Stored item by item, with the history.** `server_inventories` holds each server's hash and dates (collected, first received, last changed, last confirmed); `server_inventory_items` its items, fields as a JSON object; `server_inventory_changes` what was added, removed or changed, with the fields before and after. A delta is applied to the stored items and its hash checked, so the rows and the hash never drift apart. A server's first inventory and first package list are not changes. Package reports, which still replace `server_packages` whole, now return the old rows from the same `DELETE` and record the differences by name and architecture. Changes are kept a year, pruned server by server when the next change is written.

**Search and export.** `GET /api/v1/inventory` searches one kind across the servers the caller can read: software by name across the three sources, other kinds by key or any field ("which disk models", "which BIOS version"). Versions are compared in Rust with dpkg's rules, which also order RPM versions and Windows' dotted versions, so "OpenSSL below 3.0.15" is `kind=software&q=openssl&version_below=3.0.15`. `format=csv` returns every match up to 100,000 rows. The web interface has an Inventory tab on the server page and an Inventory page for the fleet.

**Warranty typed by people.** Dell, HP and Lenovo give warranties only through APIs that need a registered or paid key; scraping their support sites would break and breach their terms. The warranty is a date and a note typed in the web interface or through `PUT /api/v1/servers/{id}/warranty`, audited as `server.warranty`. A maker's free API, if one appears, can fill the same table later.

**PSA assets: the hook, not the sync.** The server's inventory answer carries `asset`: host name, maker, model and serial number, what ADR-0164 says an asset receives. The asset sync itself (an asset type per system family chosen in Halo, field mapping, matching existing assets by serial number then host name with a confirmation, External Links) needs its own settings, screens and checks on a trial instance, and is left for the PSA task. It will read `server_inventories.changed_at` to push at most once an hour per server what changed, as ADR-0164 says, and `asset_facts` for the fields. `docs/halopsa.md` says so.

## Cost

Measured against the load test's 8,000 agents on 2 vCPU and 4 GB:

- **Agents.** Linux: about 30 small files and three short commands an hour, tens of milliseconds of CPU. Windows: one PowerShell run of a few seconds every 4 hours.
- **Steady state.** One hash report per agent a day (about 60 bytes) and on each new stream: 8,000 a day is 0.1 a second, one indexed `UPDATE` each. Changes are rare and small (a delta of a few hundred bytes).
- **Reconnection storm** after a server restart: one hash report per agent, two indexed statements in one transaction each, behind the stream's write budget. That is less than the package report each agent already sends on connecting.
- **First rollout.** Each agent sends its whole inventory once after upgrading, 10 to 60 kB and about 100 to 400 rows; spread over the upgrade waves of ADR-0055, about 2 million rows for 8,000 servers, some 300 MB with indexes.
- **Package history.** The `DELETE` that existed already returns its rows; one `INSERT` of change rows when packages changed.
- **Search.** Searching software by text scans `server_packages` (millions of rows at 8,000 servers, about a second on 2 vCPU). It runs only when someone searches, never polled, so no text index was added: one would slow down every package report, which rewrites the table's rows of its server.

The load test's agents now carry a 124-item inventory, so the next run measures all of the above.

## Consequences

- Hardware, software and their changes are in one place per server and searchable across the fleet, with the same API as the web interface.
- Agents before this version send nothing; their servers show "No inventory yet". Older relays drop the new messages, as they drop any payload they do not know, until they update.
- Memory modules and licences are Windows only; Linux serial numbers need the unit's copy, so an agent started another way (by hand, in a container) reports none.
- The PSA asset sync is still to be built on the hook above.
