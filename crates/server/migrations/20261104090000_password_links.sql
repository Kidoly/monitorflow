-- Users managed from the web interface (ADR-0058).

-- An invited user has no password until they open their link.
ALTER TABLE users ALTER COLUMN password_hash DROP NOT NULL;
COMMENT ON COLUMN users.password_hash IS
    'argon2 PHC string; NULL for an invited user who has not chosen a password yet.';

-- The one-time link that lets a user choose a password: an invitation, or
-- a reset issued by an administrator. One live link per user: issuing one
-- replaces the previous one. Only the SHA-256 of the 256-bit secret is
-- stored, as for setup_tokens.
CREATE TABLE password_links (
    user_id    uuid        PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    token_hash bytea       NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    CHECK (expires_at > created_at)
);
COMMENT ON TABLE password_links IS
    'Hash of the one-time link that sets a user''s password; see ADR-0058.';

-- Removing a user keeps the history it left. The rows stay, without the
-- link to the account; the audit log still names it by ID.
ALTER TABLE enrollment_tokens
    DROP CONSTRAINT enrollment_tokens_created_by_fkey,
    ADD CONSTRAINT enrollment_tokens_created_by_fkey
        FOREIGN KEY (created_by) REFERENCES users (id) ON DELETE SET NULL;
ALTER TABLE campaigns
    DROP CONSTRAINT campaigns_created_by_fkey,
    ADD CONSTRAINT campaigns_created_by_fkey
        FOREIGN KEY (created_by) REFERENCES users (id) ON DELETE SET NULL;
ALTER TABLE commands
    DROP CONSTRAINT commands_requested_by_fkey,
    ADD CONSTRAINT commands_requested_by_fkey
        FOREIGN KEY (requested_by) REFERENCES users (id) ON DELETE SET NULL;
ALTER TABLE remediation_pending_commands
    DROP CONSTRAINT remediation_pending_commands_decided_by_fkey,
    ADD CONSTRAINT remediation_pending_commands_decided_by_fkey
        FOREIGN KEY (decided_by) REFERENCES users (id) ON DELETE SET NULL;
ALTER TABLE remediation_rules
    DROP CONSTRAINT remediation_rules_created_by_fkey,
    ADD CONSTRAINT remediation_rules_created_by_fkey
        FOREIGN KEY (created_by) REFERENCES users (id) ON DELETE SET NULL;
ALTER TABLE remediation_policies
    DROP CONSTRAINT remediation_policies_updated_by_fkey,
    ADD CONSTRAINT remediation_policies_updated_by_fkey
        FOREIGN KEY (updated_by) REFERENCES users (id) ON DELETE SET NULL;
ALTER TABLE server_proxmox_vms
    DROP CONSTRAINT server_proxmox_vms_updated_by_fkey,
    ADD CONSTRAINT server_proxmox_vms_updated_by_fkey
        FOREIGN KEY (updated_by) REFERENCES users (id) ON DELETE SET NULL;

-- A used enrollment token stays immutable, except for losing its creator
-- when the creator's account is removed.
CREATE OR REPLACE FUNCTION enrollment_tokens_single_use() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    unlinked enrollment_tokens;
BEGIN
    unlinked := OLD;
    unlinked.created_by := NULL;
    IF OLD.used_at IS NOT NULL AND NEW IS DISTINCT FROM unlinked THEN
        RAISE EXCEPTION 'enrollment token % was already used', OLD.id
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;
