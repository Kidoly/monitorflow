-- Web interface sessions. The cookie holds a random 256-bit token; only its
-- SHA-256 is stored, so a database dump does not hand out live sessions.
--
-- A session is valid while expires_at is in the future and last_seen_at is
-- within the idle timeout, which comes from the server configuration.
CREATE TABLE sessions (
    token_hash   bytea       PRIMARY KEY CHECK (octet_length(token_hash) = 32),
    user_id      uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at   timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz NOT NULL,
    CHECK (expires_at > created_at)
);
COMMENT ON TABLE sessions IS 'Web sessions, keyed by the SHA-256 of the cookie token.';
CREATE INDEX sessions_user_id_idx ON sessions (user_id);
CREATE INDEX sessions_expires_at_idx ON sessions (expires_at);
