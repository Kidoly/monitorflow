-- DNS names and IP addresses written in a relay's certificate, so its
-- agents can verify it like they verify the server (ADR-0025). Chosen by
-- the administrator with the token, never taken from the CSR.
ALTER TABLE enrollment_tokens
    ADD COLUMN server_names text[] NOT NULL DEFAULT '{}',
    ADD CHECK (role = 'relay' OR cardinality(server_names) = 0);

COMMENT ON COLUMN enrollment_tokens.server_names IS 'Names written in a relay certificate; empty for agents.';
