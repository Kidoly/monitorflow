-- Custom SNMP checks (ADR-0157): SNMPv1 credentials, a customer's own
-- lookups, and the objects its uploaded MIB files name. Check rows stay
-- in sensor_configs with kind 'snmp_custom'.

ALTER TABLE snmp_credentials DROP CONSTRAINT snmp_credentials_version_check;
ALTER TABLE snmp_credentials ADD CONSTRAINT snmp_credentials_version_check
    CHECK (version IN ('v1', 'v2c', 'v3'));

-- Names and states for the values of an enumerated object. `definition`
-- is {"entries": [...], "otherwise": "warning"}, checked by the API with
-- monitorflow_snmp::custom::Lookup; checks name a lookup by its ID.
CREATE TABLE snmp_lookups (
    id          uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    customer_id uuid        NOT NULL REFERENCES customers (id) ON DELETE CASCADE,
    name        text        NOT NULL CHECK (btrim(name) <> '' AND char_length(name) <= 64
                                            AND name !~ '[[:cntrl:]]'),
    definition  jsonb       NOT NULL CHECK (jsonb_typeof(definition) = 'object'),
    created_by  uuid        REFERENCES users (id) ON DELETE SET NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT snmp_lookups_name_key UNIQUE (customer_id, name)
);
COMMENT ON TABLE snmp_lookups IS
    'A customer''s lookups for custom SNMP checks. See ADR-0157.';

-- A MIB module a customer uploaded: what it names is in snmp_mib_objects;
-- the file itself is not kept. `types` holds its textual conventions, for
-- modules uploaded later that import them.
CREATE TABLE snmp_mibs (
    id          uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    customer_id uuid        NOT NULL REFERENCES customers (id) ON DELETE CASCADE,
    module      text        NOT NULL CHECK (module ~ '^[A-Za-z][A-Za-z0-9-]{0,127}$'),
    file_name   text        NOT NULL CHECK (char_length(file_name) BETWEEN 1 AND 255
                                            AND file_name !~ '[[:cntrl:]]'),
    types       jsonb       NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(types) = 'object'),
    uploaded_by uuid        REFERENCES users (id) ON DELETE SET NULL,
    uploaded_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT snmp_mibs_module_key UNIQUE (customer_id, module)
);
COMMENT ON TABLE snmp_mibs IS
    'MIB modules a customer uploaded, by name; the files are not kept. See ADR-0157.';

CREATE TABLE snmp_mib_objects (
    mib_id      uuid NOT NULL REFERENCES snmp_mibs (id) ON DELETE CASCADE,
    name        text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 128),
    oid         text NOT NULL CHECK (oid ~ '^[0-9]+(\.[0-9]+)+$' AND char_length(oid) <= 1024),
    kind        text NOT NULL CHECK (kind IN ('node', 'scalar', 'table', 'entry', 'column', 'notification')),
    syntax      text CHECK (char_length(syntax) <= 128),
    -- [[value, "name"], ...]
    enumeration jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(enumeration) = 'array'),
    units       text CHECK (char_length(units) <= 64),
    access      text CHECK (char_length(access) <= 64),
    description text NOT NULL DEFAULT '' CHECK (char_length(description) <= 501),
    PRIMARY KEY (mib_id, name)
);
CREATE INDEX snmp_mib_objects_oid_idx ON snmp_mib_objects (oid);
CREATE INDEX snmp_mib_objects_name_idx ON snmp_mib_objects (lower(name) text_pattern_ops);
