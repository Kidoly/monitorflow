-- Major distribution upgrades (ADR-0182).

-- The latest `major_upgrade` part of each server's package report: the
-- next release, the checks before upgrading to it and the last upgrade.
CREATE TABLE server_major_upgrades (
    server_id   uuid        PRIMARY KEY REFERENCES servers (id) ON DELETE CASCADE,
    report      jsonb       NOT NULL CHECK (jsonb_typeof(report) = 'object'),
    reported_at timestamptz NOT NULL DEFAULT now()
);
COMMENT ON TABLE server_major_upgrades IS 'Latest major upgrade report per server; see ADR-0182.';

-- A campaign of the third kind: {"from", "to", "accept_limited_rollback",
-- "parallel"}. NULL for package and agent update campaigns.
ALTER TABLE campaigns ADD COLUMN major_upgrade jsonb
    CHECK (major_upgrade IS NULL OR jsonb_typeof(major_upgrade) = 'object');

-- Per server of a major upgrade campaign: its Proxmox snapshot, taken by
-- a task of the runner; the checks failing on it when its upgrade was
-- queued; and when it was found done.
ALTER TABLE campaign_targets
    ADD COLUMN vm_snapshot_state text
        CHECK (vm_snapshot_state IN ('taking', 'taken', 'limited', 'failed')),
    ADD COLUMN vm_snapshot_at    timestamptz,
    ADD COLUMN vm_snapshot_error text,
    ADD COLUMN checks_before     jsonb
        CHECK (checks_before IS NULL OR jsonb_typeof(checks_before) = 'array'),
    ADD COLUMN upgraded_at       timestamptz;
ALTER TABLE campaign_targets
    -- When the runner first saw the server back on the new release.
    ADD COLUMN came_back_at      timestamptz;

-- Past runs keep the snapshot's state and when the upgrade was done.
ALTER TABLE campaign_run_targets
    ADD COLUMN vm_snapshot_state text,
    ADD COLUMN upgraded_at       timestamptz;
