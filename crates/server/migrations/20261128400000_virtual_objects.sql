-- Hosts, clusters, virtual machines and datastores of vCenters, ESXi and
-- Hyper-V hosts, as their checks last described them (ADR-0156). Written
-- by the server from samples; add-only.

-- Each source as a whole: a vCenter or ESXi device, or a Hyper-V host.
CREATE TABLE virtual_sources (
    server_id uuid PRIMARY KEY REFERENCES servers (id) ON DELETE CASCADE,
    up        boolean     NOT NULL,
    reason    text        CHECK (char_length(reason) <= 64),
    error     text        CHECK (char_length(error) <= 512),
    details   jsonb       NOT NULL DEFAULT '{}',
    seen_at   timestamptz NOT NULL DEFAULT now()
);
COMMENT ON TABLE virtual_sources IS
    'Whether each vCenter, ESXi or Hyper-V source was read, and why not; see ADR-0156.';

-- The objects of a source, by the instance their samples name. `metrics`
-- holds the numbers of the latest collection (`collected_at`).
CREATE TABLE virtual_objects (
    id           uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    source_id    uuid        NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    kind         text        NOT NULL CHECK (kind IN ('vm', 'host', 'cluster', 'datastore')),
    instance     text        NOT NULL CHECK (char_length(instance) BETWEEN 1 AND 256),
    key          text        NOT NULL DEFAULT '' CHECK (char_length(key) <= 256),
    details      jsonb       NOT NULL DEFAULT '{}',
    metrics      jsonb       NOT NULL DEFAULT '{}',
    collected_at timestamptz NOT NULL DEFAULT now(),
    seen_at      timestamptz NOT NULL DEFAULT now(),
    -- The server or device a virtual machine is, when known.
    server_id    uuid        REFERENCES servers (id) ON DELETE SET NULL,
    matched_by   text        CHECK (matched_by IN ('uuid', 'mac', 'address', 'name', 'person')),
    -- Set by a person: matching never changes it again.
    link_fixed   boolean     NOT NULL DEFAULT false,
    UNIQUE (source_id, kind, instance),
    CHECK (kind = 'vm' OR (server_id IS NULL AND NOT link_fixed)),
    CHECK ((server_id IS NULL) = (matched_by IS NULL))
);
CREATE INDEX virtual_objects_server ON virtual_objects (server_id) WHERE server_id IS NOT NULL;
CREATE INDEX virtual_objects_seen ON virtual_objects (seen_at);
COMMENT ON TABLE virtual_objects IS
    'Latest view of each host, cluster, virtual machine and datastore of a source; see ADR-0156.';
