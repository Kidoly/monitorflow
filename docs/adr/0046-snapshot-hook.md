# ADR-0046: Proxmox snapshots in the remediation loop

Date: 2026-10-28
Status: accepted

## Context

ADR-0043 left a seam in the incident loop: `runner::hook::ChangeHook::before_change` runs once before every change-tier command, its answer is recorded as the `snapshot` event, and an error escalates without running the command. ADR-0044 built the snapshot side, `Snapshotter` and `ProxmoxSnapshotter`, and left two things open: whether a failed snapshot blocks the command, and how the loop's call is bounded. This ADR connects the two.

## Decision

**Adapter.** `snapshot::SnapshotHook` implements `ChangeHook` over any `Snapshotter`, with the `method` it records (`proxmox`). It asks `support(server)` first:

- `Unavailable(reason)` (`[proxmox]` off or without clusters, server not mapped, cluster not configured): `method: "none"`, no name, detail `"<reason>; rollback is limited"`. The command runs. A `take` that answers `Unsupported` because the mapping was removed in between is treated the same.
- `Available(vm)`: `take(server, {incident, step})`. Success records `method: "proxmox"`, the snapshot's name, and a detail naming the VM, its cluster and node and the command's tier. Any error is returned, and the loop escalates.

An error from `support` itself (the database) also escalates: the loop cannot tell whether rollback was available.

**A failed snapshot on a supported server escalates, without running the command.** This confirms ADR-0043. The approver, or the `low_impact_auto` policy, accepted the change on a server where the approval view (ADR-0044's `Support`) showed rollback as available. Running it anyway would silently drop the safety net the decision relied on. The usual causes (the VM locked by a backup, storage without snapshots, a token without `VM.Snapshot`, Proxmox unreachable) need a human anyway, and the escalation reason carries Proxmox's error. Running the command with a "rollback is limited" note was rejected for that reason. Parking the incident for another approval was rejected too: it would add a state to the approval queue for a case the escalation already hands over. The hook falls back to "rollback is limited" only where the approver saw that before approving.

**Step.** `Change` gains `step`, the command's number in the incident: the commands queued before it plus one, derived from the events. A resumed incident (a crash during the snapshot, ADR-0043) asks for the same step, so the deterministic name `mf-<12 hex>-<step>` returns the snapshot already recorded, or adopts one whose task finished after a timeout (ADR-0044), instead of taking a second.

**Time.** The loop bounds `before_change` by the incident's deadline, as it bounds the AI call. When the deadline passes first, the hook is dropped and the next pass escalates at the duration limit. Inside, ADR-0044's limits still apply to each request (`request_timeout`, 30 s) and to each task (`task_timeout`, 600 s). The lease is renewed while the hook runs, so a slow snapshot does not hand the incident to another process.

**Retention.** ADR-0044's rules apply unchanged: after each snapshot, MonitorFlow's own snapshots of the VM beyond the newest `keep_snapshots` (3) are deleted, across incidents, and other snapshots are never touched. Retention runs inside `take`, so it delays the command by the deletions it makes. Its failures are logged and do not fail the snapshot.

**Wiring.** When `[ai]` is enabled, the server builds `ProxmoxSnapshotter::from_config` and passes `SnapshotHook` to `Remediation::with_hook`. With `[proxmox]` disabled it has no clusters, and every change records "snapshots are not enabled; rollback is limited", as `NoSnapshot` did. With `[proxmox]` enabled, an unreadable token secret or CA file stops the server at start, like other configuration errors.

## Consequences

- On a mapped server every change-tier command is preceded by a recorded Proxmox snapshot or does not run. The `snapshot` event names it for a later rollback.
- A Proxmox outage escalates every change on mapped servers, while read-only commands still run. Unmapping a server, or removing its cluster from the configuration, lets changes run without snapshots, and the approval view shows that rollback is limited.
- A snapshot whose task the deadline cuts short may finish on Proxmox unrecorded. The incident has escalated, so no retry adopts it, and it is left for an operator, as ADR-0044 already accepts for task timeouts.
- Two incidents whose UUIDs share their first 48 bits would name the same snapshot on the same VM. The second gets `NameTaken` and escalates. That is unlikely with random UUIDs, and it is the safe side.
- Rolling back from the interface is not part of this ADR.
