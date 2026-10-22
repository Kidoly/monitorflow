-- Relay mirror URLs (ADR-0037): each relay reports on its own stream the
-- base URL its agents reach its mirror listener at and the snapshots it
-- serves. Source configuration commands for a server attached to the
-- relay use that URL when the relay holds every mirror they name.
ALTER TABLE relays
    ADD COLUMN mirror_url         text,
    ADD COLUMN mirror_reported_at timestamptz;
COMMENT ON COLUMN relays.mirror_url IS 'Base URL of the relay''s mirror listener as its agents reach it; NULL when it serves none.';
COMMENT ON COLUMN relays.mirror_reported_at IS 'When the relay last reported its mirrors; older reports are ignored.';

CREATE TABLE relay_mirrors (
    relay_id  uuid   NOT NULL REFERENCES relays (id) ON DELETE CASCADE,
    family    text   NOT NULL CHECK (family IN ('apt', 'dnf')),
    name      text   NOT NULL,
    -- The snapshot `current` names on the relay, if any.
    current   text,
    snapshots text[] NOT NULL,
    PRIMARY KEY (relay_id, family, name)
);
COMMENT ON TABLE relay_mirrors IS 'Mirrors each relay serves, as it last reported them; see ADR-0037.';
