-- Finding each server's Proxmox VM or container instead of mapping it by
-- hand (ADR-0093, discovery). Add-only.

-- What agents report about their hardware in Hello: the SMBIOS system
-- UUID and their MAC addresses. Replaced at each connection.
ALTER TABLE servers
    ADD COLUMN system_uuid uuid,
    ADD COLUMN mac_addresses text[] NOT NULL DEFAULT '{}';

-- How a mapping was made: NULL by hand, which discovery never changes but
-- for the node a guest migrated to, else the signal discovery matched.
ALTER TABLE server_proxmox_vms
    ADD COLUMN matched_by text CHECK (matched_by IN ('uuid', 'mac', 'hostname'));

-- The VMs and containers of each configured cluster at the last discovery
-- pass, templates left out.
CREATE TABLE proxmox_guests (
    cluster       text        NOT NULL CHECK (cluster ~ '^[A-Za-z0-9._-]{1,64}$'),
    vmid          integer     NOT NULL CHECK (vmid BETWEEN 100 AND 999999999),
    node          text        NOT NULL CHECK (node ~ '^[A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?$'),
    kind          text        NOT NULL CHECK (kind IN ('qemu', 'lxc')),
    name          text        NOT NULL DEFAULT '' CHECK (length(name) <= 128),
    system_uuid   uuid,
    mac_addresses text[]      NOT NULL DEFAULT '{}',
    seen_at       timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (cluster, vmid)
);
COMMENT ON TABLE proxmox_guests IS 'Proxmox guests seen by discovery, to match servers; see ADR-0093.';

-- Guests that match a server equally: none is mapped, and the server's
-- page asks which one. Rewritten by each pass.
CREATE TABLE server_proxmox_candidates (
    server_id  uuid    NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    cluster    text    NOT NULL,
    vmid       integer NOT NULL,
    matched_by text    NOT NULL CHECK (matched_by IN ('uuid', 'mac', 'hostname')),
    PRIMARY KEY (server_id, cluster, vmid)
);

-- Guests a person said a server is not: discovery never maps them to it
-- again. Mapping the pair by hand removes the row.
CREATE TABLE server_proxmox_rejections (
    server_id   uuid        NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    cluster     text        NOT NULL,
    vmid        integer     NOT NULL,
    rejected_by uuid        REFERENCES users (id) ON DELETE SET NULL,
    rejected_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (server_id, cluster, vmid)
);
