# ADR-0093: Proxmox containers

Date: 2026-11-03
Status: accepted; amends ADR-0044 and ADR-0046

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
