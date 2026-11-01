-- Deleting a mirror (ADR-0075). `deleting_at` is set once nothing uses the
-- mirror; the row goes when its files are gone, and `delete_error` says
-- why that has not happened yet.
ALTER TABLE mirrors
    ADD COLUMN deleting_at  timestamptz,
    ADD COLUMN delete_error text;
COMMENT ON COLUMN mirrors.deleting_at IS 'Being deleted since then: no job starts and retention leaves it alone.';

-- A finished campaign keeps showing what it pinned after the mirror is
-- gone, so the names move here and the references may become NULL.
ALTER TABLE campaign_snapshots
    ADD COLUMN mirror_name   text,
    ADD COLUMN snapshot_name text;
UPDATE campaign_snapshots cs
SET mirror_name = m.name, snapshot_name = s.name
FROM mirrors m, mirror_snapshots s
WHERE m.id = cs.mirror_id AND s.id = cs.snapshot_id;
ALTER TABLE campaign_snapshots
    ALTER COLUMN mirror_name SET NOT NULL,
    ALTER COLUMN snapshot_name SET NOT NULL,
    DROP CONSTRAINT campaign_snapshots_pkey,
    DROP CONSTRAINT campaign_snapshots_mirror_id_fkey,
    DROP CONSTRAINT campaign_snapshots_snapshot_id_fkey;
ALTER TABLE campaign_snapshots
    ALTER COLUMN mirror_id DROP NOT NULL,
    ALTER COLUMN snapshot_id DROP NOT NULL,
    ADD PRIMARY KEY (campaign_id, mirror_name),
    ADD FOREIGN KEY (mirror_id) REFERENCES mirrors (id) ON DELETE SET NULL,
    ADD FOREIGN KEY (snapshot_id) REFERENCES mirror_snapshots (id) ON DELETE SET NULL;
