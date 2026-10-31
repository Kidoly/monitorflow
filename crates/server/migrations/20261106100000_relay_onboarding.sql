-- Relay onboarding without typing (ADR-0066). A relay enrolled without
-- names proposes them; they reach a certificate only once an administrator
-- confirms them, and until then the relay speaks for no agent.
ALTER TABLE relays
    ADD COLUMN pending_names   text[],
    ADD COLUMN pending_source  text,
    ADD COLUMN confirmed_names text[],
    ADD CHECK (pending_names IS NULL OR confirmed_names IS NULL);

COMMENT ON COLUMN relays.pending_names IS
    'Names the relay reported, untrusted, while it waits for an administrator; NULL once it does not.';
COMMENT ON COLUMN relays.pending_source IS
    'Address the waiting relay enrolled from, shown next to its names.';
COMMENT ON COLUMN relays.confirmed_names IS
    'Names an administrator confirmed, written in its certificates; NULL when they come from its token.';

-- An install key enrolls agents, or one relay under its host name.
ALTER TABLE install_keys
    ADD COLUMN role text NOT NULL DEFAULT 'agent' CHECK (role IN ('agent', 'relay')),
    ADD CHECK (role = 'agent' OR (max_hosts = 1 AND NOT require_approval));
