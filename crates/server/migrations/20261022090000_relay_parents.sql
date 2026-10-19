-- Relay chaining (ADR-0028): a relay may connect to another relay, its
-- parent, chosen by the administrator with the relay's enrollment token.
-- The server accepts a chained call only along these links, so a relay
-- speaks only for its own subtree.
ALTER TABLE relays
    ADD COLUMN parent_relay_id uuid REFERENCES relays (id),
    ADD CHECK (parent_relay_id <> id);
CREATE INDEX relays_parent_relay_id_idx ON relays (parent_relay_id);
COMMENT ON COLUMN relays.parent_relay_id IS 'Relay this relay connects through; NULL when it connects to the server.';

ALTER TABLE enrollment_tokens
    ADD COLUMN parent_relay_id uuid REFERENCES relays (id),
    ADD CHECK (role = 'relay' OR parent_relay_id IS NULL);
COMMENT ON COLUMN enrollment_tokens.parent_relay_id IS 'Parent written in the relay row at enrollment; NULL for agents.';
