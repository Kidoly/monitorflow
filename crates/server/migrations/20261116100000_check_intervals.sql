-- How often the built-in checks of a site run (ADR-0097). Add-only.

-- NULL: the built-in interval of the server code. Bounds are the API's:
-- 10 s to 24 h.
ALTER TABLE sites
    ADD COLUMN check_interval_ms integer
        CHECK (check_interval_ms BETWEEN 10000 AND 86400000);
COMMENT ON COLUMN sites.check_interval_ms IS 'Interval of the built-in checks of the site, NULL for the built-in one; see ADR-0097.';

-- Agents get their configuration again when it changes, like for a row of
-- sensor_configs (ADR-0016).
CREATE FUNCTION sites_check_interval_bump() RETURNS trigger AS $$
BEGIN
    IF NEW.check_interval_ms IS DISTINCT FROM OLD.check_interval_ms THEN
        NEW.sensor_revision := OLD.sensor_revision + 1;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER sites_check_interval_bump
    BEFORE UPDATE OF check_interval_ms ON sites
    FOR EACH ROW EXECUTE FUNCTION sites_check_interval_bump();
