-- API keys (ADR-0101): credentials for scripts and integrations on the
-- public API, `/api/v1`. A key acts as the user who made it, narrowed to
-- its reach, access and areas, checked again on every request. Only the
-- SHA-256 of its secret is stored.

CREATE TABLE api_keys (
    id           uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    token_hash   bytea       NOT NULL UNIQUE,
    -- The first characters of the secret after `mfk_`, to tell keys apart.
    prefix       text        NOT NULL,
    -- What it is for: "HaloPSA".
    name         text        NOT NULL CHECK (name <> '' AND char_length(name) <= 80),
    -- The highest role it acts with.
    role         text        NOT NULL
                 CHECK (role IN ('viewer', 'operator', 'approver', 'administrator')),
    -- Reach: both NULL for everything, else one customer or one site.
    customer_id  uuid        REFERENCES customers (id) ON DELETE CASCADE,
    site_id      uuid        REFERENCES sites (id) ON DELETE CASCADE,
    -- NULL: every area.
    areas        text[]      CHECK (areas <@ ARRAY['servers', 'alerts', 'updates',
                                       'remediation', 'dashboards', 'administration']),
    created_by   uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at   timestamptz NOT NULL DEFAULT now(),
    -- NULL: never expires.
    expires_at   timestamptz,
    last_used_at timestamptz,
    CHECK (customer_id IS NULL OR site_id IS NULL)
);
COMMENT ON TABLE api_keys IS
    'Keys of the public API, acting as their maker within their reach. See ADR-0101.';
CREATE INDEX api_keys_created_by_idx ON api_keys (created_by);
CREATE INDEX api_keys_customer_id_idx ON api_keys (customer_id);
CREATE INDEX api_keys_site_id_idx ON api_keys (site_id);
