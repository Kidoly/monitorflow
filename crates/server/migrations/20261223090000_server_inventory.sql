-- The hardware and software inventory of each server (ADR-0187): the
-- latest one, item by item, with the history of what changed and the
-- warranty people type in. Linux packages stay in server_packages
-- (ADR-0031); their changes are kept in the same history.

-- What the server holds of an agent's inventory.
CREATE TABLE server_inventories (
    server_id    uuid        PRIMARY KEY REFERENCES servers (id) ON DELETE CASCADE,
    -- SHA-256 of the canonical inventory (common::inventory::hash).
    hash         bytea       NOT NULL CHECK (octet_length(hash) = 32),
    -- When the agent collected it, by the agent's clock.
    collected_at timestamptz NOT NULL,
    -- When it was first received, and when its content last changed.
    first_at     timestamptz NOT NULL DEFAULT now(),
    changed_at   timestamptz NOT NULL DEFAULT now(),
    -- The last report saying it is still current.
    checked_at   timestamptz NOT NULL DEFAULT now()
);
COMMENT ON TABLE server_inventories IS
    'The hash and dates of each server''s inventory; see ADR-0187.';

CREATE TABLE server_inventory_items (
    server_id uuid  NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    kind      text  NOT NULL CHECK (kind IN ('system', 'processor', 'memory', 'disk', 'nic',
                                             'software', 'service', 'feature', 'licence')),
    key       text  NOT NULL CHECK (key <> '' AND octet_length(key) <= 256),
    -- Field name to text value; see ADR-0187 for the names of each kind.
    fields    jsonb NOT NULL CHECK (jsonb_typeof(fields) = 'object'),
    PRIMARY KEY (server_id, kind, key)
);
COMMENT ON TABLE server_inventory_items IS
    'Items of each server''s latest inventory; see ADR-0187.';
-- Searching the fleet by kind; the text match itself scans the kind.
CREATE INDEX server_inventory_items_kind_idx ON server_inventory_items (kind);

-- What changed, newest last. Pruned after a year, server by server, when
-- its next change is written.
CREATE TABLE server_inventory_changes (
    id        bigserial   PRIMARY KEY,
    server_id uuid        NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    at        timestamptz NOT NULL DEFAULT now(),
    -- The item kinds, and `package` for server_packages.
    kind      text        NOT NULL,
    key       text        NOT NULL,
    change    text        NOT NULL CHECK (change IN ('added', 'removed', 'changed')),
    -- The item's fields before and after; NULL for added and removed.
    before    jsonb,
    after     jsonb,
    CHECK ((change = 'added') = (before IS NULL)),
    CHECK ((change = 'removed') = (after IS NULL))
);
COMMENT ON TABLE server_inventory_changes IS
    'History of inventory and package changes per server, a year; see ADR-0187.';
CREATE INDEX server_inventory_changes_server_idx ON server_inventory_changes (server_id, id);

-- The warranty, as people type it: makers give it only through paid or
-- keyed services (ADR-0187).
CREATE TABLE server_warranties (
    server_id  uuid        PRIMARY KEY REFERENCES servers (id) ON DELETE CASCADE,
    ends_on    date,
    note       text        CHECK (note <> '' AND char_length(note) <= 500),
    updated_at timestamptz NOT NULL DEFAULT now(),
    updated_by uuid        REFERENCES users (id) ON DELETE SET NULL,
    CHECK (ends_on IS NOT NULL OR note IS NOT NULL)
);
COMMENT ON TABLE server_warranties IS
    'Warranty end and note per server, typed by people; see ADR-0187.';
