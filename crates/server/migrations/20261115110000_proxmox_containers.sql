-- Proxmox VE containers (LXC) next to VMs, and what Proxmox answered the
-- last time MonitorFlow asked whether a mapped guest can be snapshotted
-- (ADR-0093). Add-only.

-- `kind` picks the API path: /nodes/{node}/qemu/{vmid} or .../lxc/{vmid}.
-- `checked_at` is NULL until the first check and again when a snapshot
-- failed, so the next pass asks at once. `cannot_snapshot` says why
-- Proxmox refuses snapshots of the guest, `left_out` what a snapshot of
-- it does not hold; both NULL when nothing is known against it.
ALTER TABLE server_proxmox_vms
    ADD COLUMN kind text NOT NULL DEFAULT 'qemu' CHECK (kind IN ('qemu', 'lxc')),
    ADD COLUMN checked_at timestamptz,
    ADD COLUMN cannot_snapshot text CHECK (length(cannot_snapshot) <= 512),
    ADD COLUMN left_out text CHECK (length(left_out) <= 512);
COMMENT ON COLUMN server_proxmox_vms.kind IS 'qemu for a VM, lxc for a container; see ADR-0093.';
