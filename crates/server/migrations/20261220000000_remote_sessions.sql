-- Remote terminals opened from a server's page (ADR-0190).
--
-- Who may open them, whether they are recorded and when an idle one
-- closes: one row, changed by global administrators. The host itself must
-- also have allowed terminals (`remote.terminal` in agent.toml), which no
-- row here can change.
CREATE TABLE remote_access_settings (
    id                   boolean     PRIMARY KEY DEFAULT true CHECK (id),
    -- The lowest role, on the server's site, that may open a session;
    -- NULL lets nobody.
    min_role             text        CHECK (min_role IN ('operator', 'approver', 'administrator')),
    record               boolean     NOT NULL DEFAULT true,
    idle_timeout_minutes integer     NOT NULL DEFAULT 15
                                     CHECK (idle_timeout_minutes BETWEEN 1 AND 1440),
    -- Recordings older than this are deleted.
    retention_days       integer     NOT NULL DEFAULT 90 CHECK (retention_days BETWEEN 1 AND 3650),
    updated_at           timestamptz NOT NULL DEFAULT now()
);
INSERT INTO remote_access_settings (min_role) VALUES ('administrator');
COMMENT ON TABLE remote_access_settings IS
    'Who may open remote terminals, recording and idle time; one row. See ADR-0190.';

-- One row per session, kept when it closes: who, on which server, when
-- and for how long. The audit log has the same events.
CREATE TABLE remote_sessions (
    id                   uuid        PRIMARY KEY,
    server_id            uuid        NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    kind                 text        NOT NULL CHECK (kind IN ('terminal')),
    requested_by         uuid        REFERENCES users (id) ON DELETE SET NULL,
    -- Kept when the account goes.
    username             text        NOT NULL,
    -- SHA-256 of the one-time token the browser's WebSocket presents.
    token_sha256         bytea       NOT NULL CHECK (octet_length(token_sha256) = 32),
    token_expires_at     timestamptz NOT NULL,
    recorded             boolean     NOT NULL,
    idle_timeout_seconds integer     NOT NULL CHECK (idle_timeout_seconds BETWEEN 60 AND 86400),
    -- The signed `remote_session` command, once the browser connected.
    command_id           uuid        REFERENCES commands (id) ON DELETE SET NULL,
    created_at           timestamptz NOT NULL DEFAULT now(),
    -- The browser connected with the token.
    connected_at         timestamptz,
    -- The agent answered and proved its keys.
    opened_at            timestamptz,
    -- Moved on by the instance holding the browser's WebSocket while the
    -- session lives; a session whose instance stopped is closed by any.
    seen_at              timestamptz NOT NULL DEFAULT now(),
    closed_at            timestamptz,
    close_reason         text,
    -- Keystrokes sent and output received, in bytes.
    bytes_in             bigint      NOT NULL DEFAULT 0,
    bytes_out            bigint      NOT NULL DEFAULT 0,
    CHECK (closed_at IS NULL OR close_reason IS NOT NULL)
);
CREATE INDEX remote_sessions_server_idx ON remote_sessions (server_id, created_at DESC);
CREATE INDEX remote_sessions_open_idx ON remote_sessions (seen_at) WHERE closed_at IS NULL;
COMMENT ON TABLE remote_sessions IS
    'Remote terminals: who opened them on which server, when and for how long. See ADR-0190.';

-- What recorded sessions printed, in order: the terminal's transcript.
-- Keystrokes are not kept apart: what the terminal echoed is in it, and
-- what it did not echo, such as a password, is not.
CREATE TABLE remote_session_recordings (
    session_id uuid    NOT NULL REFERENCES remote_sessions (id) ON DELETE CASCADE,
    seq        integer NOT NULL,
    -- Milliseconds since the session opened.
    at_ms      bigint  NOT NULL CHECK (at_ms >= 0),
    output     bytea   NOT NULL,
    PRIMARY KEY (session_id, seq)
);
COMMENT ON TABLE remote_session_recordings IS
    'Output of recorded remote terminals, for replay. See ADR-0190.';
