-- Traffic flows (ADR-0184). A device sends NetFlow, sFlow or IPFIX to one
-- probe: the central server or a relay of its site. Only sums reach the
-- server, one window (a minute) at a time: the largest keys of each
-- dimension, the largest conversations, and the totals.

CREATE TABLE flow_exporters (
    server_id      uuid        PRIMARY KEY REFERENCES servers (id) ON DELETE CASCADE,
    probe          text        NOT NULL CHECK (probe IN ('central', 'relay')),
    probe_relay_id uuid        REFERENCES relays (id) ON DELETE CASCADE,
    -- Where its flows come from when not the device's own address, like
    -- the loopback a router exports from. NULL: the device's address.
    source_address text        CHECK (char_length(source_address) BETWEEN 1 AND 253
                                      AND source_address !~ '[[:cntrl:][:space:]]'),
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now(),
    CHECK ((probe = 'relay') = (probe_relay_id IS NOT NULL))
);
COMMENT ON TABLE flow_exporters IS
    'Devices whose traffic flows a probe receives (ADR-0184).';

-- The largest keys of each dimension per window. `dimension` `total` has
-- one row of key '' with the window's totals, so the rest of a dimension
-- is that total minus its keys. Minutes are kept 48 hours, then summed
-- into hours (`seconds` 3600), kept 31 days.
CREATE TABLE flow_tops (
    server_id uuid        NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    at        timestamptz NOT NULL,
    seconds   integer     NOT NULL CHECK (seconds > 0),
    dimension text        NOT NULL CHECK (dimension IN ('total', 'src', 'dst', 'app', 'protocol', 'in', 'out')),
    key       text        NOT NULL CHECK (char_length(key) <= 64),
    bytes     bigint      NOT NULL CHECK (bytes >= 0),
    packets   bigint      NOT NULL CHECK (packets >= 0),
    PRIMARY KEY (server_id, dimension, at, seconds, key)
);
CREATE INDEX flow_tops_at_idx ON flow_tops (at);

-- The largest conversations per window, for views filtered on an address.
CREATE TABLE flow_conversations (
    server_id uuid        NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    at        timestamptz NOT NULL,
    seconds   integer     NOT NULL CHECK (seconds > 0),
    src       text        NOT NULL CHECK (char_length(src) <= 64),
    dst       text        NOT NULL CHECK (char_length(dst) <= 64),
    app       text        NOT NULL CHECK (char_length(app) <= 64),
    input     bigint      NOT NULL CHECK (input >= 0),
    output    bigint      NOT NULL CHECK (output >= 0),
    bytes     bigint      NOT NULL CHECK (bytes >= 0),
    packets   bigint      NOT NULL CHECK (packets >= 0),
    PRIMARY KEY (server_id, at, seconds, src, dst, app, input, output)
);
CREATE INDEX flow_conversations_src_idx ON flow_conversations (server_id, src, at);
CREATE INDEX flow_conversations_dst_idx ON flow_conversations (server_id, dst, at);
CREATE INDEX flow_conversations_at_idx ON flow_conversations (at);
