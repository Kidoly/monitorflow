-- Proxmox VE VMs of servers and the snapshots MonitorFlow took of them
-- before change-tier commands (architecture §8, ADR-0044).

-- At most one VM per server and one server per VM. `cluster` names a
-- `[[proxmox.clusters]]` entry of the server configuration.
CREATE TABLE server_proxmox_vms (
    server_id  uuid        PRIMARY KEY REFERENCES servers (id) ON DELETE CASCADE,
    cluster    text        NOT NULL CHECK (cluster ~ '^[A-Za-z0-9._-]{1,64}$'),
    node       text        NOT NULL CHECK (node ~ '^[A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?$'),
    vmid       integer     NOT NULL CHECK (vmid BETWEEN 100 AND 999999999),
    updated_by uuid        REFERENCES users (id),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT server_proxmox_vms_vm_key UNIQUE (cluster, vmid)
);
COMMENT ON TABLE server_proxmox_vms IS 'Proxmox VE VM of a server, for snapshots before changes; see ADR-0044.';

-- Snapshots MonitorFlow created. Retention deletes only snapshots listed
-- here, so a snapshot someone else made is never touched, whatever its
-- name. `deleted_at` is set once retention removed it from Proxmox.
CREATE TABLE proxmox_snapshots (
    id         bigserial   PRIMARY KEY,
    cluster    text        NOT NULL,
    vmid       integer     NOT NULL,
    name       text        NOT NULL CHECK (name ~ '^mf-[0-9a-f]{12}-[0-9]{1,10}$'),
    server_id  uuid        REFERENCES servers (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    deleted_at timestamptz
);
COMMENT ON TABLE proxmox_snapshots IS 'Proxmox VM snapshots taken by MonitorFlow, subject to retention; see ADR-0044.';
CREATE UNIQUE INDEX proxmox_snapshots_live_key ON proxmox_snapshots (cluster, vmid, name)
    WHERE deleted_at IS NULL;
CREATE INDEX proxmox_snapshots_vm_idx ON proxmox_snapshots (cluster, vmid, created_at)
    WHERE deleted_at IS NULL;
