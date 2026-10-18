-- Sensor configuration pushed to agents (ADR-0016).
--
-- A row belongs either to a site, as a default for every server of the
-- site, or to one server, as an override or an addition. The effective
-- configuration of a server is the server's built-in defaults, then its
-- site's rows, then its own rows: each layer replaces the sensor with the
-- same (name, instance) of the layers before it, and a disabled row
-- removes it.
--
-- Names, intervals and timeouts follow ADR-0012, so a row the database
-- accepts is one the agent accepts too. Params are a flat JSON object of
-- scalars; the agent receives them as strings.
--
-- Thresholds are a later table referencing sensor_configs (id).
CREATE TABLE sensor_configs (
    id          uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    site_id     uuid        REFERENCES sites (id) ON DELETE CASCADE,
    server_id   uuid        REFERENCES servers (id) ON DELETE CASCADE,
    kind        text        NOT NULL CHECK (kind ~ '^[a-z][a-z0-9_]{0,63}$'),
    name        text        NOT NULL CHECK (name ~ '^[a-z][a-z0-9_]{0,63}$'),
    instance    text        NOT NULL DEFAULT ''
                            CHECK (octet_length(instance) <= 256 AND instance !~ '[[:cntrl:]]'),
    interval_ms bigint      NOT NULL CHECK (interval_ms BETWEEN 1000 AND 86400000),
    -- NULL: the agent's default (10 s), capped by the interval.
    timeout_ms  bigint      CHECK (timeout_ms > 0 AND timeout_ms <= interval_ms),
    params      jsonb       NOT NULL DEFAULT '{}'::jsonb
                            CHECK (jsonb_typeof(params) = 'object' AND NOT jsonb_path_exists(
                                params, 'strict $.* ? (@.type() == "object" || @.type() == "array" || @.type() == "null")')),
    enabled     boolean     NOT NULL DEFAULT true,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    CHECK ((site_id IS NULL) <> (server_id IS NULL)),
    CONSTRAINT sensor_configs_key UNIQUE NULLS NOT DISTINCT (site_id, server_id, name, instance)
);
COMMENT ON TABLE sensor_configs IS 'Sensors per site (defaults) or per server (overrides); see ADR-0016.';
CREATE INDEX sensor_configs_server_id_idx ON sensor_configs (server_id) WHERE server_id IS NOT NULL;

-- A server's configuration revision is sites.sensor_revision +
-- servers.sensor_revision. Each counter only grows, so the sum changes on
-- every change of either layer, whatever order transactions commit in.
ALTER TABLE sites ADD COLUMN sensor_revision bigint NOT NULL DEFAULT 0;
ALTER TABLE servers
    ADD COLUMN sensor_revision         bigint NOT NULL DEFAULT 0,
    ADD COLUMN sensor_revision_applied bigint,
    ADD COLUMN sensor_applied_at       timestamptz;
COMMENT ON COLUMN servers.sensor_revision_applied IS 'Revision of the sensor configuration the agent last acknowledged.';

CREATE FUNCTION sensor_configs_bump_revision() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP <> 'INSERT' THEN
        UPDATE sites SET sensor_revision = sensor_revision + 1 WHERE id = OLD.site_id;
        UPDATE servers SET sensor_revision = sensor_revision + 1 WHERE id = OLD.server_id;
    END IF;
    IF TG_OP <> 'DELETE' THEN
        UPDATE sites SET sensor_revision = sensor_revision + 1 WHERE id = NEW.site_id;
        UPDATE servers SET sensor_revision = sensor_revision + 1 WHERE id = NEW.server_id;
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER sensor_configs_bump_revision
    AFTER INSERT OR UPDATE OR DELETE ON sensor_configs
    FOR EACH ROW EXECUTE FUNCTION sensor_configs_bump_revision();
