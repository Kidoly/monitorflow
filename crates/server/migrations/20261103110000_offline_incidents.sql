-- Offline incidents (ADR-0057): a server or relay that misses heartbeats
-- opens a critical incident in `incidents`, with the reserved metric
-- 'offline', and resolves it when it reconnects.

-- A relay's incident has no server. Exactly one of the two is set, and
-- only offline incidents belong to a relay.
ALTER TABLE incidents
    ALTER COLUMN server_id DROP NOT NULL,
    ADD COLUMN relay_id uuid REFERENCES relays (id) ON DELETE CASCADE,
    ADD CONSTRAINT incidents_one_peer CHECK (num_nonnulls(server_id, relay_id) = 1),
    ADD CONSTRAINT incidents_relay_offline CHECK (relay_id IS NULL OR metric = 'offline');
COMMENT ON COLUMN incidents.relay_id IS
    'The relay of an offline incident; NULL for incidents of a server. See ADR-0057.';
-- The servers' index cannot hold relays' rows (server_id is NULL there).
CREATE UNIQUE INDEX incidents_one_open_relay_key ON incidents (relay_id)
    WHERE state = 'open';

-- Offline incidents are on by default; turning one of these off mutes
-- them for the site, the server or the relay.
ALTER TABLE sites ADD COLUMN offline_alerts boolean NOT NULL DEFAULT true;
ALTER TABLE servers ADD COLUMN offline_alerts boolean NOT NULL DEFAULT true;
ALTER TABLE relays ADD COLUMN offline_alerts boolean NOT NULL DEFAULT true;
COMMENT ON COLUMN sites.offline_alerts IS
    'Open incidents when servers or relays of the site go offline; see ADR-0057.';
