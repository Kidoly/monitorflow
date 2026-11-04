-- The storage part of agents' package reports (ADR-0094): the system
-- volumes of a Linux server, whether they are on LVM or ZFS, and the local
-- snapshots MonitorFlow took of them. The latest report only; snapshots
-- are listed from the host, where their tag or property says they are
-- MonitorFlow's.
CREATE TABLE server_storage (
    server_id   uuid        PRIMARY KEY REFERENCES servers (id) ON DELETE CASCADE,
    report      jsonb       NOT NULL,
    reported_at timestamptz NOT NULL DEFAULT now()
);
COMMENT ON TABLE server_storage IS 'System volumes and local snapshots reported by agents; see ADR-0094.';

-- The local snapshot a campaign takes of a server before its update, when
-- the server has no Proxmox VM and its system volumes allow one.
ALTER TABLE campaign_targets
    ADD COLUMN snapshot_command_id uuid REFERENCES commands (id) ON DELETE SET NULL;
