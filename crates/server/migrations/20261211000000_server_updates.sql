-- Updates of the central server, newest first on the server update page
-- (ADR-0172). The updater's status file holds the last one only: the
-- server keeps each one here, by the ID of its request.
CREATE TABLE server_updates (
    id           text        PRIMARY KEY CHECK (id <> ''),
    from_version text,
    to_version   text        NOT NULL CHECK (to_version <> ''),
    -- The user name of who asked; NULL when the updater started it (an
    -- update from the command line) or the user is not known.
    requested_by text,
    -- `requested` until the updater takes it, then the updater's states;
    -- `unknown` for those copied from the audit log below.
    state        text        NOT NULL CHECK (state <> ''),
    error        text,
    started_at   timestamptz NOT NULL,
    finished_at  timestamptz
);
CREATE INDEX server_updates_started_at_idx ON server_updates (started_at DESC);

-- Updates asked for from the web interface before this table: the audit
-- log has who asked and when, not how each ended.
INSERT INTO server_updates (id, from_version, to_version, requested_by, state, started_at)
SELECT a.details->>'request_id', a.details->>'from', a.details->>'to', u.username, 'unknown', a.occurred_at
FROM audit_log a
LEFT JOIN users u ON u.id::text = a.actor_id
WHERE a.action = 'server.update'
  AND coalesce(a.details->>'request_id', '') <> ''
  AND coalesce(a.details->>'to', '') <> ''
ON CONFLICT (id) DO NOTHING;
