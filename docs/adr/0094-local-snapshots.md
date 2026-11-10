# ADR-0094: Local LVM and ZFS snapshots before a change

Date: 2026-11-03
Status: accepted; extends ADR-0044, ADR-0046 and ADR-0093; `take` and `delete` need the signed command (ADR-0108)

## Context

Before a change-tier command of AI remediation, the server snapshots the server's Proxmox VM when it is mapped to one (ADR-0044, ADR-0046). Every other server, bare metal or a VM on another platform, gets "rollback is limited". Many of them have their root filesystem on LVM, often a thin pool, or on ZFS, which can snapshot themselves in a second. The agent runs as the unprivileged `monitorflow` user and reaches root only through fixed `sudo -n` lines to typed helpers (ADR-0032, ADR-0036, ADR-0053).

Questions: what is snapshotted and how the server learns it in advance, how a snapshot is taken without giving the server a way to run tools as root, what space it may use, what rolling back means, how long snapshots stay, and whether update campaigns take them too.

## Decision

### What is snapshotted

The system volumes: the filesystems mounted at `/`, `/usr`, `/var`, `/opt` and `/boot`, whichever exist. That is where the system, its packages, their configuration and their state live, which is what a package update or a remediation command changes. Data volumes (`/home`, `/srv`, databases elsewhere) are left alone: rolling them back would lose writes the change never touched, and they are often the largest.

Each one is classified from `/proc/self/mountinfo`: a ZFS mount names its dataset; any other mount's device number is matched against `lvs`'s `lv_kernel_major:lv_kernel_minor`. An LV of type `V` is thin LVM, `-` or `o` classic LVM; anything else, a partition, Btrfs, or LVM or ZFS whose tools are missing or fail (inside a container, for instance), is plain. `/` must be on LVM or ZFS for a snapshot to be taken. The other system volumes are covered when they can be, and the page names those left out, typically a `/boot` partition.

### The helper

`monitorflow-agent snapshots inspect|take|delete`, run as root through three new exact sudoers lines, in the style of ADR-0036:

- `inspect` takes no input and prints the volumes and MonitorFlow's snapshots as JSON. It runs `lvm lvs` and `lvm vgs` in their JSON report format with byte units, and `zfs list -H -p`, at fixed paths, in the C locale.
- `take` and `delete` read `{"name": "monitorflow-…"}` on stdin, unknown fields refused, at most 4 KiB. The name is `monitorflow-` then lowercase letters and digits in `-`-separated groups, at most 64 bytes, so it reads neither as an option nor as a path, and fits LVM and ZFS grammars.

The server names the snapshot and nothing else. Which volumes, the snapshot size, the tools and their arguments are decided by the helper from what it inspects itself, so a compromised agent user can make snapshots of the system volumes and delete MonitorFlow's, and nothing more.

The same rules decide, in `common::snapshot::plan`, both in the helper before a take and on the server to say in advance what a change gets, so the two never disagree.

The agent's own user runs `inspect` with each package report (`packages.report_interval`), and the report's new `storage` field carries the result, or the helper's error. The commands `take_snapshot` and `delete_snapshot` (payloads `TakeSnapshot` and `DeleteSnapshot`, new `snapshots.proto`) run the helper, then send a new report at once. Agents without a package manager send no reports, so they get no local snapshots; Windows agents refuse both commands as unsupported.

### Ownership

LVM snapshots carry the tag `monitorflow` and are named `<name>_<origin LV>`; ZFS snapshots are `<dataset>@<name>` with the user property `monitorflow:managed=yes`. Only those are listed, so `delete` and retention never touch a snapshot someone else made, even one named `monitorflow-…`. Ownership lives on the host rather than in a table, as the host is where the snapshot is, and the agent can check it alone.

### Space

- **Thin LVM:** a thin snapshot takes no space up front but draws from the pool as the origin changes, and a full thin pool stops every volume in it. The helper refuses below 10 % free in the pool's data or metadata.
- **Classic LVM:** a snapshot needs space reserved in the volume group, and becomes invalid when the copy-on-write area fills. It gets 20 % of its origin, at least 1 GiB, at most the origin; snapshots of one group share its free space. When the group lacks it, the helper refuses: "volume group rl has 0 B free, and a snapshot of / needs 8.0 GiB". It never shrinks a volume or takes a smaller snapshot than the rule says: an invalid snapshot is worse than none, because it looks like a safety net.
- **ZFS:** a snapshot costs nothing at first and pins blocks as they change. The helper refuses below 10 % available in the dataset. `zfs snapshot -r` covers the dataset's children, so Ubuntu's layout (`rpool/ROOT/ubuntu_x/var/lib`…) is covered by the root's snapshot; system volumes on other datasets join the same `zfs snapshot` call, which is atomic.

There is no setting: these numbers are the safe side, and the page says them when it refuses.

### Taking it

**One order for every change.** `remediation::snapshot::choice::choose` decides what a change gets, and the remediation hook (ADR-0046), the approval queue, the server page and campaigns all follow it:

1. **A Proxmox guest** (ADR-0044, ADR-0093): while `[proxmox]` is on, a server mapped to a VM or container, by hand or by discovery, is Proxmox's. Proxmox snapshots it; when it cannot (storage that cannot take snapshots, a cluster that no longer serves the server), rollback is limited for Proxmox's reason. Such a guest **never also gets a local snapshot**: two rollbacks of one machine would disagree, and the fix is on the hypervisor, which the page names. A server that discovery matched automatically is a mapped guest like any other; one with several candidates is not mapped yet and gets local snapshots until someone picks its guest, after which Proxmox takes over. "Not this one" unmaps it, and local snapshots apply again.
2. **A local snapshot**: a server that is no mapped guest, or any server while `[proxmox]` is off, gets one when its system volumes allow it.
3. **Otherwise rollback is limited**, for the local reason, like "/ is on plain ext4".

Proxmox's answer comes first and the local rules are not even read for a mapped guest. There is no other provider today: ADR-0046's "hook" is this adapter, not a separate script.

**Limited rollback runs.** As ADR-0093 decided, a change whose rollback is limited runs with "rollback is limited" in its `snapshot` event: the approver saw it before approving. Only a snapshot that fails where the approver was shown one escalates. That holds for local snapshots: a refusal known from the report, like a full thin pool, shows as limited beforehand and the change runs; a refusal that appears only when taking (the pool filled since the last report) escalates without running the command.

For a local snapshot, the hook queues `take_snapshot` named `monitorflow-incident-<first 12 hex digits of the incident>-<step>` and waits for its result, at most 10 minutes plus a minute. The agent sends a new report after every take, so the next change sees the volumes as they are.

As with Proxmox, a failed snapshot on a server shown as supported escalates the incident without running the command (ADR-0046's reason: the approver relied on it). The name is deterministic, so a resumed step asks for the same snapshot and the helper answers that it already exists rather than taking a second. The time a snapshot was taken is on the host (`lv_time`, ZFS `creation`) and on the page, not in the name, which would break that.

A take of several LVs is not atomic. When one fails, the helper removes what that call created, so the host is as before.

The approval queue and `GET /api/servers/{id}/rollback` (read by anyone who sees the server) answer the same object as ADR-0093's `snapshots`, with `method` added: `proxmox`, `lvm` or `zfs` when available. Its `detail` is Proxmox's cause for a guest, else a sentence on the local snapshot ("LVM snapshot of / and /var (the thin pool has 40.0 GiB free)") or why there is none. The server page keeps ADR-0093's one Rollback line and its states: "Snapshot first" with what is snapshotted, "Rollback limited" with why, and for an administrator of the customer the Proxmox mapping (Choose, Change, Not this one, Map to a VM or container). A server that is no guest shows its local line there, with the button to map it. The approval detail says which snapshot is taken: "A Proxmox snapshot of the container…", or "A local snapshot is taken before the command runs: LVM snapshot of /…". The local snapshots themselves, when they are deleted and how to roll back, are on the Updates tab.

### Rolling back

Rolling back is manual, offered as instructions with the exact commands on the server's page, never automatic and never one click from MonitorFlow:

- **LVM:** `lvconvert --merge vg/<snapshot>` for each volume of the snapshot, then a reboot: the origin is in use, so LVM merges it at the next activation. A merge of `/` that goes wrong leaves a host that may not boot, which needs someone at the console.
- **ZFS:** a mounted root cannot be rolled back live. From a rescue system or the boot loader's environment, `zfs rollback <dataset>@<name>` for each dataset of the snapshot. `zfs rollback` also refuses when newer snapshots exist, which `-r` would destroy; the page says so instead of adding `-r`.

Both lose every write after the snapshot on those volumes, logs included, and need a reboot or downtime. That is a decision for a person who can see the host, not for MonitorFlow, and safety comes first: nothing rolls back without a human. A one-click LVM merge that schedules the reboot could come later, as a high-impact command with its own approval.

`/boot` on a partition is not covered. After rolling back a kernel update, the boot menu may still offer the new kernel first; the page says to pick the previous one.

### Retention

The server deletes, with `delete_snapshot` commands, every hour:

- classic LVM snapshots after a day: they slow every write to their origin and fill up;
- invalid snapshots at once: they cannot be rolled back to;
- others after 7 days, a week as for mirror snapshots (ADR-0074);
- any beyond the 3 newest of a server, as for Proxmox (ADR-0044).

There is no setting, for ADR-0074's reason: the safe value is the default, and a wrong one only costs space or history. Retention reads the snapshots from the latest report, so a snapshot the agent took after a lost result is deleted all the same. A delete is asked again after 6 hours when the snapshot is still listed. Revoked servers are left alone. Takes and deletes are audited as `server.snapshot.take` and `server.snapshot.delete`, with actor `system/snapshots`, besides being commands. Several server instances run the pass under one transaction advisory lock (ADR-0089).

### Campaigns

Update campaigns (ADR-0033) did not snapshot before this ADR, Proxmox included. A package campaign now takes a local snapshot of a server before its update when the order above gives one: the local rules allow it and the server is not a mapped guest while `[proxmox]` is on, named `monitorflow-campaign-<first 12 hex digits of the campaign>`, as one more command in its sequence (after `configure_sources` when pinned, before `apply_updates`). A failed snapshot fails that server for the campaign without updating it, and counts toward the failure threshold. Proxmox snapshots in campaigns are left out: the Proxmox API call blocks for minutes, which the campaign runner's short transactions cannot hold, and it needs its own design. Agent update campaigns take no snapshot: the agent rolls itself back (ADR-0053).

`campaign_targets.snapshot_command_id` records that command; a server configured again after a restore (ADR-0038) takes it again, and the helper answers that it already exists while it does.

### Windows and Btrfs

Windows is out of scope: VSS snapshots are per volume, a system volume cannot be rolled back without a recovery environment, and its tools differ entirely. Windows servers keep "rollback is limited".

Btrfs would fit the same design: `btrfs subvolume snapshot -r` of the root subvolume, rolled back by making the snapshot the default subvolume and rebooting. It is not done here, because layouts differ widely (Snapper and openSUSE's own rollback, Fedora's `root` subvolume, flat or nested), and choosing wrongly would make a host unbootable. Btrfs roots are shown as "/ is on Btrfs, which MonitorFlow does not snapshot yet".

## Consequences

- A bare-metal server, or a VM outside Proxmox, with `/` on LVM or ZFS gets a snapshot before every change-tier remediation command and every campaign update with nothing to configure, and its page says so beforehand.
- Hosts installed before this ADR have sudo rules without the `snapshots` lines, and self-update replaces only the binary, so they report "the sudo rules on this server predate snapshots; run the install command again" until it is run. Refreshing the rules from self-update is a possible follow-up.
- `sudo` logs each `inspect`, once per package report.
- A classic LVM host whose volume group has no free space, the default on many RHEL installs, keeps rollback limited, and the page says how much space a snapshot would need.
- A thin pool or ZFS pool near its 10 % margin refuses snapshots before changes, which escalates incidents on servers shown as supported; the page shows the free space.
- `server_storage` holds the latest report per server, and `campaign_targets.snapshot_command_id` the snapshot of each campaign server (migration `20261115120000_local_snapshots`). It only references `servers`, `campaign_targets` and `commands`, older than ADR-0093's migrations, so it depends on none of them.
- A Proxmox guest still gets no snapshot in campaigns.
