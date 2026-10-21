-- Commands sent to agents over their stream, and their results (ADR-0031).
--
-- A command is queued, then sent on the agent's stream, running once the
-- agent says it started, and ends succeeded, failed, expired or
-- cancelled. At most one command per server is sent or running at a time.
CREATE TABLE commands (
    id           uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    server_id    uuid        NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    kind         text        NOT NULL CHECK (kind ~ '^[a-z][a-z0-9_]{0,63}$'),
    -- The serialized protobuf message of the kind, like ApplyUpdates.
    payload      bytea       NOT NULL CHECK (octet_length(payload) <= 1048576),
    state        text        NOT NULL DEFAULT 'queued'
                             CHECK (state IN ('queued', 'sent', 'running', 'succeeded',
                                              'failed', 'expired', 'cancelled')),
    issued_at    timestamptz NOT NULL DEFAULT now(),
    -- Agents refuse to start a command after this time.
    expires_at   timestamptz NOT NULL,
    sent_at      timestamptz,
    started_at   timestamptz,
    finished_at  timestamptz,
    exit_code    integer,
    -- Truncated by the server to 64 KiB each.
    stdout       text,
    stderr       text,
    -- Why it failed or did not run, in one line.
    error        text,
    -- NULL for commands the server issues itself.
    requested_by uuid        REFERENCES users (id),
    -- Campaigns come later; no foreign key until their table exists.
    campaign_id  uuid,
    CHECK (expires_at > issued_at),
    CHECK ((state IN ('succeeded', 'failed', 'expired', 'cancelled')) = (finished_at IS NOT NULL))
);
COMMENT ON TABLE commands IS 'Commands for agents and their results; see ADR-0031.';
CREATE INDEX commands_server_issued_idx ON commands (server_id, issued_at);
-- What the dispatcher looks at: pending commands per server.
CREATE INDEX commands_pending_idx ON commands (server_id, issued_at)
    WHERE state IN ('queued', 'sent', 'running');
CREATE UNIQUE INDEX commands_one_in_flight_key ON commands (server_id)
    WHERE state IN ('sent', 'running');
CREATE INDEX commands_campaign_idx ON commands (campaign_id) WHERE campaign_id IS NOT NULL;
