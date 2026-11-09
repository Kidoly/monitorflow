-- Plugins (ADR-0103): signed WebAssembly checks and notification channels,
-- installed by global administrators from the web interface.

-- Keys a global administrator trusts to sign plugins, besides the
-- catalogue keys built into the server.
CREATE TABLE plugin_publishers (
    id         uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    name       text        NOT NULL CHECK (name <> '' AND char_length(name) <= 80),
    -- Raw Ed25519 public key.
    public_key bytea       NOT NULL UNIQUE CHECK (length(public_key) = 32),
    created_by uuid        REFERENCES users (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
COMMENT ON TABLE plugin_publishers IS
    'Publisher keys trusted to sign plugins. See ADR-0103.';

-- One row per installed plugin, its current version only. The package is
-- kept whole, as signed, so agents can check it again.
CREATE TABLE plugins (
    id           uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    name         text        NOT NULL UNIQUE,
    kind         text        NOT NULL CHECK (kind IN ('check', 'channel')),
    version      text        NOT NULL,
    title        text        NOT NULL,
    manifest     jsonb       NOT NULL,
    package      bytea       NOT NULL,
    sha256       bytea       NOT NULL CHECK (length(sha256) = 32),
    public_key   bytea       NOT NULL CHECK (length(public_key) = 32),
    official     boolean     NOT NULL,
    -- Turned off: every use stops, settings stay.
    enabled      boolean     NOT NULL DEFAULT true,
    installed_by uuid        REFERENCES users (id) ON DELETE SET NULL,
    installed_at timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now()
);
COMMENT ON TABLE plugins IS
    'Installed plugins and their signed packages. See ADR-0103.';

-- Where a plugin may be used: everything (both NULL), a customer or a
-- site, like a channel's coverage (ADR-0078). None: nowhere yet.
CREATE TABLE plugin_places (
    plugin_id   uuid NOT NULL REFERENCES plugins (id) ON DELETE CASCADE,
    customer_id uuid REFERENCES customers (id) ON DELETE CASCADE,
    site_id     uuid REFERENCES sites (id) ON DELETE CASCADE,
    CHECK (customer_id IS NULL OR site_id IS NULL)
);
CREATE UNIQUE INDEX plugin_places_unique_idx ON plugin_places (
    plugin_id,
    COALESCE(customer_id, '00000000-0000-0000-0000-000000000000'),
    COALESCE(site_id, '00000000-0000-0000-0000-000000000000')
);
CREATE INDEX plugin_places_customer_id_idx ON plugin_places (customer_id);
CREATE INDEX plugin_places_site_id_idx ON plugin_places (site_id);

-- Agents get the check plugins they use with their sensor configuration:
-- any change of a plugin or of where it may be used is a new revision.
CREATE FUNCTION plugins_bump_sensor_revision() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    UPDATE sites SET sensor_revision = sensor_revision + 1;
    RETURN NULL;
END
$$;
CREATE TRIGGER plugins_sensor_revision
    AFTER INSERT OR UPDATE OR DELETE ON plugins
    FOR EACH STATEMENT EXECUTE FUNCTION plugins_bump_sensor_revision();
CREATE TRIGGER plugin_places_sensor_revision
    AFTER INSERT OR UPDATE OR DELETE ON plugin_places
    FOR EACH STATEMENT EXECUTE FUNCTION plugins_bump_sensor_revision();
