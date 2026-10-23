# ADR-0044: Proxmox VE snapshots before change-tier commands

Date: 2026-10-27
Status: accepted

## Context

Architecture §8 ("Approval, audit, rollback") says an arbitrary shell command cannot be undone reliably, so before any change-tier command (low or high impact, ADR-0041) the server takes a snapshot when the platform supports it, Proxmox VE VM snapshots first, and the interface shows that rollback is limited when it does not. The remediation loop (ADR-0043) is written separately and will call the snapshot side through a hook; this ADR covers that side only.

Questions: the interface the loop calls, how the server reaches Proxmox and authenticates, how a server is tied to a VM, how snapshots are named and pruned without ever touching someone else's, and how the API tells whether rollback is available.

## Decision

**Trait.** `crates/server/src/remediation/snapshot`: `Snapshotter` with `support(server)`, `take(server, SnapshotRequest {incident, step})`, `list(server)` and `rollback(server, name)`. `support` answers `Available(vm)` or `Unavailable(reason)`, with reason `disabled`, `not_mapped` or `unknown_cluster`. `NoSnapshots` answers `disabled` for every server. LVM, Btrfs or ZFS can come later behind the same trait. `take` waits for the snapshot to exist before returning, so the loop sends the command only after a successful snapshot. Whether a failed snapshot blocks the command or leaves it to the approver is the loop's decision.

**Proxmox client.** HTTPS REST API with `reqwest` on rustls, as in the rest of the workspace. `POST /api2/json/nodes/{node}/qemu/{vmid}/snapshot` (`snapname`, `description`, disk only, without RAM) starts a task. `GET /nodes/{node}/tasks/{upid}/status` is polled every second until `stopped`: exit status `OK` or `WARNINGS…` succeeds, anything else is a task failure carrying the exit status, and `task_timeout` (600 s) is a timeout. The same applies to `DELETE …/snapshot/{name}` and `POST …/snapshot/{name}/rollback`. Every path segment is percent-encoded. Errors are typed: authentication (401/403), API (status and Proxmox's `message`, bounded to 512 bytes, control characters removed), task failure, timeout, transport and invalid answer.

**Authentication.** An API token only (`Authorization: PVEAPIToken=user@realm!tokenid=secret`), never a password or ticket. The token needs `VM.Snapshot` and `VM.Snapshot.Rollback` on the VMs (`VM.Audit` to list them). The secret is read at start from `token_secret_file` (only the secret, surrounding whitespace ignored). It never goes in the configuration, the environment, the database or logs, and the header value is marked sensitive. Redirects are not followed, so the token is never sent to another host.

**TLS.** Certificates are always verified, and there is no option to turn that off. By default the built-in web roots are trusted. With `ca_file` (PEM), only those CA certificates are trusted for that cluster, which covers Proxmox's self-signed cluster CA (`/etc/pve/pve-root-ca.pem`). `https_only` is set on the client, and `url` must be `https`.

**Configuration.** A `[proxmox]` section, off by default: `enabled`, `keep_snapshots` (3), `request_timeout` (30 s), `task_timeout` (600 s), and `[[proxmox.clusters]]` with `name` (1 to 64 of `[A-Za-z0-9._-]`), `url`, `token_id`, `token_secret_file` and an optional `ca_file`. The scalar keys have `MONITORFLOW_PROXMOX_*` overrides; clusters come only from the file. Cluster entries are always validated, and the rest only when enabled. Enabled requires at least one cluster.

**Mapping.** Table `server_proxmox_vms (server_id PK, cluster, node, vmid, updated_by, updated_at)`, migration `20261027100000_proxmox_snapshots` (add-only). There is one VM per server and one server per `(cluster, vmid)`, `node` is a host name and `vmid` runs from 100 to 999999999. Administrators of the server's customer set it, as for revocation (ADR-0029): `GET`, `PUT` and `DELETE /api/servers/{id}/proxmox`. `PUT` accepts only cluster names present in the configuration. A VM mapped twice is 409. Changes go to `audit_log` (`server.proxmox.set` with the previous mapping, `server.proxmox.clear`). The node is the one the VM lives on: after a migration to another node, the administrator updates it. Discovering VMs by MAC address or guest agent was left out, because a wrong automatic match would snapshot or roll back the wrong machine.

**Availability.** The `GET` view carries `snapshots: {available, reason}`. It is available when the integration is enabled, the server is mapped and its cluster is configured. It does not depend on reachability, which only a real attempt tells. The approval queue will read the same `Support` to show that rollback is limited.

**Names.** `mf-<first 12 hex digits of the incident UUID>-<step>`, at most 25 characters, within Proxmox's 40 and its `[A-Za-z][A-Za-z0-9_-]*` grammar. They are deterministic, so a retry of a step returns the snapshot already recorded instead of taking a second. A snapshot with the name but not recorded is adopted only if its description is exactly the one this step writes. That covers a task that finished after its timeout. Any other snapshot with the name is refused (`NameTaken`).

**Ownership and retention.** Table `proxmox_snapshots` records each snapshot MonitorFlow took: cluster, vmid, name, server and time, with `deleted_at` once pruned. After each successful snapshot, recorded snapshots of the VM beyond the newest `keep_snapshots` are deleted, oldest first. One already missing from Proxmox is only marked deleted. Ownership comes from this table, not from the name: a snapshot MonitorFlow did not record is never deleted or rolled back to, even if it is named `mf-…`. A retention failure is logged and does not fail the snapshot just taken. Creation, deletion and rollback are written to `audit_log` as `proxmox.snapshot.create`, `.delete` and `.rollback`, with actor `system/remediation`.

## Consequences

- The loop gets a ready, tested snapshot call. Wiring it into the pre-change hook, and showing availability in the web interface, are later tasks.
- Proxmox is never contacted by tests. They run against a fake API over TLS with a test CA, which also checks that untrusted certificates are refused.
- Mappings are manual: one API call per server. Bulk import or discovery can come later.
- A snapshot whose task outlives `task_timeout` stays on the VM unrecorded until the same step is retried, which adopts it. Otherwise it is left for an operator, which is the safe side.
- Snapshots on storage without snapshot support (raw on LVM, for instance) fail as tasks, and the loop treats that like any snapshot failure.
- LXC containers (`/nodes/{node}/lxc/{vmid}/snapshot`) are not covered. A `kind` column can be added to the mapping later.
