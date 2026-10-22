-- Campaigns pinned to mirror snapshots (ADR-0038): before its update,
-- each server is pointed at the pinned snapshots with a
-- `configure_sources` command, and its previous sources are restored
-- afterwards.

-- `replace` or `add` (ADR-0032) for the pinned sources; NULL when the
-- campaign pins nothing.
ALTER TABLE campaigns
    ADD COLUMN sources_mode text CHECK (sources_mode IN ('replace', 'add'));

-- The published snapshot of each pinned mirror. A server uses the pinned
-- mirrors of its OS family.
CREATE TABLE campaign_snapshots (
    campaign_id uuid NOT NULL REFERENCES campaigns (id) ON DELETE CASCADE,
    mirror_id   uuid NOT NULL REFERENCES mirrors (id),
    snapshot_id uuid NOT NULL REFERENCES mirror_snapshots (id),
    PRIMARY KEY (campaign_id, mirror_id)
);
COMMENT ON TABLE campaign_snapshots IS 'Mirror snapshots a campaign pins its servers to; see ADR-0038.';

ALTER TABLE campaign_targets
    -- The `configure_sources` command pointing the server at the pinned
    -- snapshots before its update.
    ADD COLUMN sources_command_id uuid REFERENCES commands (id) ON DELETE SET NULL,
    -- The command restoring the server's previous sources afterwards.
    ADD COLUMN restore_command_id uuid REFERENCES commands (id) ON DELETE SET NULL;

-- Servers that may still need their sources restored, whatever the
-- campaign's state.
CREATE INDEX campaign_targets_restore_idx ON campaign_targets (campaign_id)
    WHERE sources_command_id IS NOT NULL AND restore_command_id IS NULL;
