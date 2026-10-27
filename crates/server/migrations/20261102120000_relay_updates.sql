-- Relay self-updates (ADR-0055): the version each relay reports, and the
-- self_update commands sent to relays on their own streams.

-- From the relay's Hello; NULL until it connects with this server.
ALTER TABLE relays ADD COLUMN relay_version text CHECK (relay_version <> '');
COMMENT ON COLUMN relays.relay_version IS
    'Version the relay reported in its last Hello; see ADR-0055.';

-- Like commands (ADR-0031), for relays: only self_update, one in flight
-- per relay.
CREATE TABLE relay_commands (
    id           uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    relay_id     uuid        NOT NULL REFERENCES relays (id) ON DELETE CASCADE,
    kind         text        NOT NULL CHECK (kind = 'self_update'),
    payload      bytea       NOT NULL,
    -- The release asked for, for display.
    version      text        NOT NULL CHECK (version <> ''),
    state        text        NOT NULL DEFAULT 'queued'
                             CHECK (state IN ('queued', 'sent', 'running', 'succeeded',
                                              'failed', 'expired', 'cancelled')),
    -- NULL when queued by automatic updates.
    requested_by uuid        REFERENCES users (id) ON DELETE SET NULL,
    issued_at    timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz NOT NULL,
    sent_at      timestamptz,
    started_at   timestamptz,
    finished_at  timestamptz,
    exit_code    integer,
    stdout       text,
    stderr       text,
    error        text,
    CHECK (expires_at > issued_at),
    CHECK ((state IN ('succeeded', 'failed', 'expired', 'cancelled')) = (finished_at IS NOT NULL))
);
COMMENT ON TABLE relay_commands IS 'self_update commands of relays; see ADR-0055.';
CREATE UNIQUE INDEX relay_commands_in_flight ON relay_commands (relay_id)
    WHERE state IN ('queued', 'sent', 'running');
CREATE INDEX relay_commands_relay_idx ON relay_commands (relay_id, issued_at);

-- Each relay is updated automatically at most once per version.
CREATE TABLE relay_auto_updates (
    relay_id   uuid        NOT NULL REFERENCES relays (id) ON DELETE CASCADE,
    version    text        NOT NULL CHECK (version <> ''),
    command_id uuid        REFERENCES relay_commands (id) ON DELETE SET NULL,
    queued_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (relay_id, version)
);
COMMENT ON TABLE relay_auto_updates IS
    'Relay self_update commands queued by automatic updates, one per relay and version; see ADR-0055.';
