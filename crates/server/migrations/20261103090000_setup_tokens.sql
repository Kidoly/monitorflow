-- The one-time setup link of a server without users (ADR-0057). One row at
-- most: issuing a link replaces the previous one. Only the SHA-256 of the
-- 256-bit secret is stored. Creating any user deletes the row, so the link
-- dies with the first account, whichever way it is created.
CREATE TABLE setup_tokens (
    singleton  boolean     PRIMARY KEY DEFAULT true CHECK (singleton),
    token_hash bytea       NOT NULL CHECK (octet_length(token_hash) = 32),
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    CHECK (expires_at > created_at)
);
COMMENT ON TABLE setup_tokens IS
    'Hash of the one-time link that creates the first administrator; see ADR-0057.';
