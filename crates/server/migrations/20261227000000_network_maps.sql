-- Network maps (ADR-0185): drawings of a site or a customer, made of
-- what MonitorFlow already knows (relays, dependencies, devices' LLDP and
-- CDP neighbours, network scans) and of what a person placed by hand.
CREATE TABLE network_maps (
    id          uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    name        text        NOT NULL CHECK (char_length(name) BETWEEN 1 AND 80),
    -- A map belongs to one site or to one customer as a whole.
    site_id     uuid        REFERENCES sites (id) ON DELETE CASCADE,
    customer_id uuid        REFERENCES customers (id) ON DELETE CASCADE,
    -- Positions, zones, links and labels placed by hand, and which
    -- sources draw it, as the API takes them (`MapDocument`).
    document    jsonb       NOT NULL,
    revision    bigint      NOT NULL DEFAULT 1,
    created_by  uuid        REFERENCES users (id) ON DELETE SET NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_by  uuid        REFERENCES users (id) ON DELETE SET NULL,
    updated_at  timestamptz NOT NULL DEFAULT now(),
    CHECK ((site_id IS NULL) <> (customer_id IS NULL))
);
CREATE INDEX network_maps_site ON network_maps (site_id);
CREATE INDEX network_maps_customer ON network_maps (customer_id);
COMMENT ON TABLE network_maps IS 'Network maps of a site or customer; see ADR-0185.';

-- The picture under a map: a floor plan, a rack, a city. PNG, JPEG or
-- WebP only, checked by their first bytes, never SVG.
CREATE TABLE network_map_backgrounds (
    map_id       uuid        PRIMARY KEY REFERENCES network_maps (id) ON DELETE CASCADE,
    content_type text        NOT NULL CHECK (content_type IN ('image/png', 'image/jpeg', 'image/webp')),
    image        bytea       NOT NULL CHECK (octet_length(image) <= 1048576),
    updated_at   timestamptz NOT NULL DEFAULT now()
);
COMMENT ON TABLE network_map_backgrounds IS 'Background image of a network map; see ADR-0185.';

-- What a device last said of its neighbours over LLDP or CDP. Text from
-- the network, shown as text; replaced at each read of the device.
CREATE TABLE network_neighbors (
    server_id      uuid NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    protocol       text NOT NULL CHECK (protocol IN ('lldp', 'cdp')),
    local_port     text NOT NULL CHECK (char_length(local_port) <= 256),
    remote_name    text NOT NULL CHECK (char_length(remote_name) <= 256),
    remote_port    text NOT NULL DEFAULT '' CHECK (char_length(remote_port) <= 256),
    remote_address inet
);
CREATE INDEX network_neighbors_server ON network_neighbors (server_id);
COMMENT ON TABLE network_neighbors IS 'LLDP and CDP neighbours of SNMP devices; see ADR-0185.';

CREATE TABLE network_neighbor_reads (
    server_id uuid        PRIMARY KEY REFERENCES servers (id) ON DELETE CASCADE,
    read_at   timestamptz NOT NULL DEFAULT now(),
    -- Why the last read failed; NULL when it worked.
    error     text        CHECK (char_length(error) <= 512)
);
COMMENT ON TABLE network_neighbor_reads IS 'When each device''s neighbours were last read; see ADR-0185.';
