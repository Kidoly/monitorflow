-- Answers to writes sent with `Idempotency-Key` through the public API
-- (ADR-0101), kept 24 hours so a retry gets the first answer instead of
-- doing the change twice. Kept per API key.

CREATE TABLE api_idempotency (
    key_id       uuid        NOT NULL REFERENCES api_keys (id) ON DELETE CASCADE,
    -- The header's value.
    value        text        NOT NULL CHECK (char_length(value) BETWEEN 1 AND 255),
    -- SHA-256 of the method, path, query and body of the first request.
    fingerprint  bytea       NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    -- NULL while the first request runs.
    status       smallint,
    content_type text,
    -- NULL when the answer handed out a secret, which is never stored.
    body         bytea,
    PRIMARY KEY (key_id, value)
);
COMMENT ON TABLE api_idempotency IS
    'Answers of the public API kept for Idempotency-Key retries. See ADR-0101.';
CREATE INDEX api_idempotency_created_at_idx ON api_idempotency (created_at);
