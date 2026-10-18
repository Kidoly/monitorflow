-- Enrollment tokens name the peer they enroll (ADR-0006), and can be
-- created with `monitorflow-server token create`, which has no web user.
--
-- peer_id is NOT NULL without a default: no release ever created tokens
-- before this migration, so the table is empty when it runs.
ALTER TABLE enrollment_tokens
    ADD COLUMN peer_id text NOT NULL CHECK (peer_id ~ '^[A-Za-z0-9._-]{1,64}$'),
    ALTER COLUMN created_by DROP NOT NULL,
    ADD CHECK (used_by_peer_id IS NULL OR used_by_peer_id = peer_id);

COMMENT ON COLUMN enrollment_tokens.peer_id IS 'ID written in the certificate URI SAN; never taken from the CSR.';
COMMENT ON COLUMN enrollment_tokens.created_by IS 'User who created the token; NULL when created from the server CLI.';
