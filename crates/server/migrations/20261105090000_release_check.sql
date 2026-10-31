-- The daily release check (ADR-0061): every version the source has offered,
-- and `update.available` in the notifications outbox.
--
-- The outbox is pruned after a week, so it cannot remember which versions
-- were told; a row here is what makes the notice once per version.
CREATE TABLE seen_releases (
    version           text        PRIMARY KEY,
    release_notes_url text,
    first_seen_at     timestamptz NOT NULL DEFAULT now(),
    -- The newest of these is what the source offers today.
    last_seen_at      timestamptz NOT NULL DEFAULT now()
);
COMMENT ON TABLE seen_releases IS
    'Release versions the release check has seen. See ADR-0061.';

ALTER TABLE notification_outbox DROP CONSTRAINT notification_outbox_event_check;
ALTER TABLE notification_outbox ADD CONSTRAINT notification_outbox_event_check
    CHECK (event IN ('incident.opened', 'incident.escalated', 'incident.resolved',
                     'approval.waiting', 'update.failed', 'backup.failed',
                     'update.available'));
