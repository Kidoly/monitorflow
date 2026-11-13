# ADR-0093: Proxmox containers, and finding each server's guest

Date: 2026-11-03
Status: accepted; amends ADR-0044 and ADR-0046; the discovery amendment below replaces ADR-0044's manual-only mapping; amended 2026-11-09 (mapping by hand on a cluster that serves every customer)

## Context

ADR-0044 snapshots a server's Proxmox VM before every change-tier command of the remediation loop, and ADR-0046 connects it to the loop. It left LXC containers out: a server whose agent runs in a container is "not mapped", and its changes run with limited rollback. Containers are common on Proxmox, often for exactly the small services MonitorFlow watches.

Containers differ from VMs where it matters for a snapshot:

- The API path is `/nodes/{node}/lxc/{vmid}/…` instead of `/qemu/`. Snapshot, rollback, deletion and task polling otherwise have the same shape, and a container has no RAM state to save.
- A container's root disk and mount points may sit on storage that cannot take snapshots. `dir` storage, the default `local` of a fresh install, holds containers as plain directories or raw files, and Proxmox refuses their snapshots. A VM on `dir` snapshots as long as its disks are qcow2.
- Mount points may be host paths: a bind mount (`mp0: /srv/shared,mp=/shared`) or a device (`mp0: /dev/sdb1,mp=/data`). Proxmox cannot snapshot those. Depending on the version it refuses the snapshot or takes it without them.
- Containers are privileged or unprivileged.
- Proxmox offers `pct snapshot` on the node besides the API.

ADR-0044 learned that a guest cannot be snapshotted only from a failed attempt, which escalates the incident (ADR-0046). For a container on `dir`, every change would escalate, and the approval page would have shown "rollback available" before each one.

## Decision

**Kind.** `server_proxmox_vms` gains `kind`, `qemu` or `lxc`, `qemu` by default so existing mappings stay VMs (migration `20261115110000_proxmox_containers`, add-only). `VmRef` carries it, every client path is built from it, and audit details record it. `PUT /api/servers/{id}/proxmox` takes an optional `"kind": "lxc"`. VM IDs are unique across both kinds in a cluster, so `proxmox_snapshots`, keyed by cluster and ID, and its retention (ADR-0044's `keep_snapshots`, default 3 per guest) apply unchanged. Rollback goes through the same `rollback` call, with ownership from the same table: MonitorFlow only rolls back to a snapshot it recorded.

**The API, not `pct`.** `pct` runs as root on a node, which would mean giving MonitorFlow a shell on the hypervisor. The API does the same with a token limited to `VM.Snapshot`, `VM.Snapshot.Rollback` and `VM.Audit`, privileges that cover containers too. No new credential and no new setting.

**Asking Proxmox before a change.** Proxmox decides what it can snapshot, so MonitorFlow asks it rather than keep its own list of storage types. `GET …/{kind}/{vmid}/feature?feature=snapshot` answers for the guest as configured now. A refusal is explained from the guest's configuration (`…/config`): the first bind or device mount point, else the first volume on a storage that cannot snapshot it, typed from `/nodes/{node}/storage` (which needs `Datastore.Audit`; without it the sentence falls back to "Proxmox reports that the container's volumes cannot take snapshots"). The answer is stored on the mapping: `cannot_snapshot` (the sentence), `left_out` (host-path mount points a snapshot that Proxmox did take does not hold), and `checked_at`.

A leader-only pass (ADR-0089) asks for each mapping never checked or not checked for an hour, every minute, 50 at a time. Mapping a server clears the previous answer, so a new mapping is checked within a minute. A snapshot that fails with a task or API error marks its mapping for a check at the next pass, so storage changed under a mapping is shown before the next change. A guest Proxmox cannot be asked about keeps its last answer. The pass needs no AI: it runs whenever `[proxmox]` is enabled.

**Rollback limited, said plainly.** `Unsupported` gains `cannot_snapshot`, set from the stored answer. `support` returns it, so the hook records `method: "none"` with "the container's storage cannot take snapshots; rollback is limited" and the command runs, as the approver was shown. This keeps ADR-0046's rule: a snapshot failure escalates only where the approver saw rollback as available. `list` and `rollback` still reach such a guest, as earlier snapshots may exist. The API's `snapshots` object gains `kind`, `detail` (the stored sentence) and `left_out`, in the server view and the approval queue alike.

**Privileged and unprivileged.** Nothing changes between them for snapshots and rollback through the API. They differ only inside the container, in what the agent can read about the hardware.

**Screens.** The server page gains a Rollback line for administrators of its customer while Proxmox is on: "Snapshot first: Proxmox snapshots container 104 on pve2 before each AI change", or "Rollback limited: the container's storage cannot take snapshots" followed by the cause ("Rootfs is on storage local (dir), which cannot take snapshots"), or "Limited: not mapped to a Proxmox VM or container". When a snapshot leaves a bind mount out, it says so and that a rollback does not restore it. The same words appear on the approval page. Mapping by hand (type, node, ID, and cluster when there are several) moves from the API alone to that line.

Rejected:

- **Refusing to map a container that cannot snapshot.** The mapping is still true, and the answer changes when the storage does.
- **Escalating instead of running with limited rollback.** Every change on such a container would wait for a person, for a cause that a person cannot fix inside the incident. The approver sees that rollback is limited and decides.
- **Asking Proxmox on every page load.** Slow when Proxmox is, and the API would need the clients. The answer is at most an hour old, and fresh after a mapping or a failure.
- **Starting the guest after rollback.** Proxmox leaves a rolled-back container or VM stopped unless `start` is passed, a parameter older versions refuse. Rollback is not started from the interface yet, so both kinds keep Proxmox's default.

## Consequences

- A server in a container gets snapshots before AI changes, on the same terms as a VM.
- A container on `dir` storage, or with a bind mount, shows "Rollback limited" with the reason before any change, and its changes run without escalating. Moving its volumes to ZFS, LVM-thin, Ceph or Btrfs makes it "Snapshot first" within an hour, or at once by mapping it again.
- A rollback restores a container's volume mount points with its root disk, as it restores all of a VM's disks. Data on them written after the snapshot is lost, as on a VM.
- Each mapped guest costs two or three API requests an hour.
- A token without `Datastore.Audit` still works; the reason is less precise.

## Amendment: discovery

### Context

ADR-0044 left discovery out: "a wrong automatic match would snapshot or roll back the wrong machine". Mapping each server by hand does not scale past a few, and a mapping goes stale when its guest migrates to another node. The question is which signals to trust, how a doubt is shown, how a person corrects a match, and how a cluster stays within its customer.

### Decision

**What agents report.** `Hello` gains `system_uuid` (field 9) and `mac_addresses` (field 10), additive. A VM's SMBIOS system UUID is the `uuid` of its `smbios1` line, set by Proxmox when the VM is created and regenerated on clone. Linux keeps it root-only (`/sys/class/dmi/id/product_uuid`), and the agent runs as `monitorflow`, so the systemd unit copies it into the agent's runtime directory before starting it (`ExecStartPre=+-…`, ignored where there is none). The copy is `0400` and owned by `monitorflow`, in a `0700` directory, so other local users can read it no more than the original. The agent reads the copy, or the file itself when it runs as root. Windows records it in `HKLM\SYSTEM\HardwareConfig\LastConfig`, which the agent, as LocalSystem, reads. Inside a container the agent sends no UUID, as a privileged one reads its host's. Linux agents send the MAC addresses of their interfaces, loopback and all-zero left out. Windows agents send none: a Windows VM is found by its UUID, and a container never runs Windows. The server stores both on `servers` (`system_uuid`, `mac_addresses`, migration `20261115110100_proxmox_discovery`, add-only), cleaned of what does not parse and of the firmware placeholders many machines share. A malformed value never refuses the agent. Installs made before this change get the unit's copy on their next install; until then their Linux agents match by MAC, which works as well for VMs and containers.

**Signals, most trusted first.**

1. **The SMBIOS UUID**, against VMs. It is the hardware identity Proxmox gives the VM, unique per VM unless a person copied it.
2. **A MAC address**, against VMs and containers. Proxmox generates one per network device, and two guests sharing one would not work on the same network.
3. **The host name**, against the VM's name or the container's `hostname`, compared on their first label without case, **only when the agent reports neither** (an agent older than this change, or behind an older relay, which drops the fields). Names are typed by people and reused across customers. When an agent reports hardware that matches nothing, its name is not tried: the hardware says it is not on that cluster. A match by name is shown as such, with "check it is the right one".

A server is mapped at the first signal that matches anything, and only if exactly one guest matches and no other server matches that guest. Otherwise nothing is mapped, the candidates are recorded (`server_proxmox_candidates`), and the server's page asks: "Several Proxmox VMs or containers match this server, so none is used yet. Which one is it?" with one button per candidate. Rollback stays limited meanwhile, which is the safe side. Templates are left out.

**Who corrects.** A mapping by hand (`PUT`) always wins: discovery never changes it, except to follow its guest to another node. Unmapping (`DELETE`, "Not this one" on the page) records a rejection (`server_proxmox_rejections`) and discovery never maps that server to that guest again; mapping the pair by hand lifts it. `matched_by` on the mapping is `uuid`, `mac` or `hostname`, or null for a mapping by hand. The page says "Matched automatically to VM 104 web-1 on pve2 by its hardware ID" (or "by its network card", or "by its name only; check it is the right one"), with Change and Not this one. The form picks from the guests discovery saw, with "Not in the list" for type, node and ID by hand.

**Tenancy.** Clusters stay in `server.toml` (ADR-0064 kept them there). Each `[[proxmox.clusters]]` entry takes an optional `customer` or `site`, an ID as the web interface shows it. A cluster with an owner serves only that customer's or site's servers: by discovery, by hand (a `PUT` naming another customer's cluster is 400 "unknown Proxmox cluster", as for a missing one) and for snapshots (such a mapping reads `unknown_cluster`). A cluster without one is the operator's and serves every customer, as before. The picker lists an operator's cluster to global administrators only, since its guests belong to every customer; others see the candidates their own server matched, and map their server only to one of those (amended below; they could first map by hand to any guest). No setting is needed for a single-customer or single-operator install.

**The pass.** The leader runs it at start and every 10 minutes, before the snapshot checks: `GET /cluster/resources?type=vm` per cluster, then each guest's configuration (`smbios1`, `netN`, `hostname`), into `proxmox_guests`. Every mapping whose guest is now on another node moves there (`server.proxmox.move`). Then, only if every cluster could be listed, servers are matched: a cluster that cannot be read must not unmap its servers. New and changed matches are audited as `server.proxmox.discover` (with `matched_by` and the previous mapping), lost ones as `server.proxmox.unmatch` (with the reason), all with actor `system/proxmox-discovery`. The token needs `VM.Audit` on the guests to list them, as ADR-0044 already asks.

**Migrations between passes.** A snapshot or rollback that Proxmox refuses with an API error looks the guest up in `/cluster/resources`; if it is on another node, the mapping moves there (audited) and the call is made again once.

Rejected:

- **The guest agent** (`qemu-guest-agent`'s network interfaces). It needs the agent installed in each VM and `VM.Monitor`, and says nothing a MAC does not.
- **Matching by name whenever the hardware fails.** A name matching across customers on an operator's cluster would snapshot the wrong machine and show rollback as available.
- **Mapping the first of several candidates.** A clone and its original share everything but their history; a person tells them apart in one click.
- **Moving clusters into the web interface.** It would store API tokens in the database and is a separate change; the owner keys keep the configuration where it is.

### Consequences

- On a cluster with up-to-date agents, servers in VMs and containers are mapped within 10 minutes of connecting, with no click, and stay mapped across migrations.
- An agent older than this change matches by name only when its name is unique on both sides, and the page says to check it.
- A clone keeps its original's MAC addresses until someone changes them; both show as candidates until then.
- Each pass costs one request per cluster and one per guest.
- A container's agent reports its veth MAC, which Proxmox keeps in the container's configuration: containers match as reliably as VMs.

## Amendment: mapping by hand on a cluster that serves every customer (2026-11-09)

### Context

The picker hid an operator's cluster from administrators of one customer, but `PUT /api/servers/{id}/proxmox` still took any guest ID on it. A customer's administrator could map their server to another customer's VM or container. A mapping is what the remediation hook acts on: before each change-tier AI command of that server it snapshots the mapped guest (`take`), and retention then deletes MonitorFlow's older snapshots of that guest, which are kept per cluster and guest ID, not per server. `rollback` checks the same pair, so it would reach that guest too once something calls it (nothing does yet). The 409 "already mapped to another server" also told which guest IDs are in use.

### Decision

On a cluster without `customer` or `site`, only a global administrator maps a server by hand to any guest. An administrator of a customer or site may `PUT` only a guest that discovery recorded as a candidate for that server (`server_proxmox_candidates`), which is what the "Which one is it?" buttons send. Anything else is 400 with one message, whether the guest exists, is free or is mapped, checked before the insert so that the 409 is never reached. A cluster with an owner is unchanged: its guests are all its customer's or site's, and its administrators map by hand as before.

Rejected:

- **Also allowing a guest the server was once matched to or unmapped from.** After "Not this one", mapping the pair again would need a record of why it was allowed; rejections are also written when a mapping made by hand is removed, so they do not prove a match. A global administrator maps it.
- **Checking ownership of the guest.** Proxmox does not know MonitorFlow's customers; on a shared cluster only discovery's match ties a guest to a server.

### Consequences

- On a shared cluster, an administrator of one customer who unmapped a match, or whose server discovery cannot match, asks a global administrator to map it. "Not in the list" answers them with that sentence.
- Mappings made before this change are not re-checked. `server.proxmox.set` in the audit log lists them, with who made each.
