-- Thresholds, alert state and incidents of the threshold engine (ADR-0017).
--
-- A threshold belongs either to a site, as a default for every server of
-- the site, or to one server, as an override. The effective thresholds of
-- a server are the built-in defaults, then its site's rows, then its own
-- rows: each layer replaces the threshold with the same (metric, instance)
-- of the layers before it, and a disabled row mutes it (ADR-0016's model).
--
-- A NULL instance matches every instance of the metric; a non-NULL one only
-- that instance, and wins over the NULL row for it.
CREATE TABLE thresholds (
    id          uuid             PRIMARY KEY DEFAULT gen_random_uuid(),
    site_id     uuid             REFERENCES sites (id) ON DELETE CASCADE,
    server_id   uuid             REFERENCES servers (id) ON DELETE CASCADE,
    -- Full metric name as written to VictoriaMetrics (ADR-0013).
    metric      text             NOT NULL CHECK (metric ~ '^monitorflow_[a-z][a-z0-9_]{0,63}$'),
    instance    text             CHECK (octet_length(instance) <= 256 AND instance !~ '[[:cntrl:]]'),
    comparison  text             NOT NULL CHECK (comparison IN ('>', '>=', '<', '<=')),
    -- NaN is greater than Infinity in PostgreSQL, so `< 'Infinity'` refuses
    -- both.
    warning     double precision CHECK (warning > '-Infinity' AND warning < 'Infinity'),
    critical    double precision CHECK (critical > '-Infinity' AND critical < 'Infinity'),
    -- Seconds the condition must hold, measured on sample timestamps.
    duration_s  integer          NOT NULL DEFAULT 0 CHECK (duration_s BETWEEN 0 AND 86400),
    -- How far back past a threshold the value must go to recover from it.
    hysteresis  double precision NOT NULL DEFAULT 0
                                 CHECK (hysteresis >= 0 AND hysteresis < 'Infinity'),
    enabled     boolean          NOT NULL DEFAULT true,
    created_at  timestamptz      NOT NULL DEFAULT now(),
    updated_at  timestamptz      NOT NULL DEFAULT now(),
    CHECK ((site_id IS NULL) <> (server_id IS NULL)),
    -- A disabled row only mutes, so it needs no value.
    CHECK (NOT enabled OR warning IS NOT NULL OR critical IS NOT NULL),
    -- Critical is at least as far on the alarm side as warning.
    CHECK (warning IS NULL OR critical IS NULL
           OR (comparison IN ('>', '>=') AND critical >= warning)
           OR (comparison IN ('<', '<=') AND critical <= warning)),
    CONSTRAINT thresholds_key UNIQUE NULLS NOT DISTINCT (site_id, server_id, metric, instance)
);
COMMENT ON TABLE thresholds IS 'Thresholds per site (defaults) or per server (overrides); see ADR-0017.';
CREATE INDEX thresholds_server_id_idx ON thresholds (server_id) WHERE server_id IS NOT NULL;

-- Same revision scheme as sensor_configs (ADR-0016): a server's threshold
-- revision is sites.threshold_revision + servers.threshold_revision.
ALTER TABLE sites ADD COLUMN threshold_revision bigint NOT NULL DEFAULT 0;
ALTER TABLE servers ADD COLUMN threshold_revision bigint NOT NULL DEFAULT 0;

CREATE FUNCTION thresholds_bump_revision() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP <> 'INSERT' THEN
        UPDATE sites SET threshold_revision = threshold_revision + 1 WHERE id = OLD.site_id;
        UPDATE servers SET threshold_revision = threshold_revision + 1 WHERE id = OLD.server_id;
    END IF;
    IF TG_OP <> 'DELETE' THEN
        UPDATE sites SET threshold_revision = threshold_revision + 1 WHERE id = NEW.site_id;
        UPDATE servers SET threshold_revision = threshold_revision + 1 WHERE id = NEW.server_id;
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER thresholds_bump_revision
    AFTER INSERT OR UPDATE OR DELETE ON thresholds
    FOR EACH ROW EXECUTE FUNCTION thresholds_bump_revision();

-- An incident: a series that crossed a threshold for its duration. At most
-- one is open per series, which the partial unique index enforces even if
-- two engines ever race.
CREATE TABLE incidents (
    id            uuid             PRIMARY KEY DEFAULT gen_random_uuid(),
    server_id     uuid             NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    metric        text             NOT NULL,
    -- '' for series without an instance, like the label (ADR-0013).
    instance      text             NOT NULL,
    severity      text             NOT NULL CHECK (severity IN ('warning', 'critical')),
    state         text             NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'resolved')),
    opened_at     timestamptz      NOT NULL,
    resolved_at   timestamptz,
    last_value    double precision NOT NULL,
    last_value_at timestamptz      NOT NULL,
    -- The worst value seen while open: the highest for > and >=, the
    -- lowest for < and <=.
    peak_value    double precision NOT NULL,
    -- The threshold as it was when the incident opened.
    threshold     jsonb            NOT NULL CHECK (jsonb_typeof(threshold) = 'object'),
    CHECK ((state = 'resolved') = (resolved_at IS NOT NULL)),
    CHECK (resolved_at IS NULL OR resolved_at >= opened_at)
);
COMMENT ON TABLE incidents IS 'Threshold incidents; at most one open per (server, metric, instance).';
CREATE UNIQUE INDEX incidents_one_open_key ON incidents (server_id, metric, instance)
    WHERE state = 'open';
CREATE INDEX incidents_server_opened_idx ON incidents (server_id, opened_at);

-- What happened to an incident, one row per change. Rows are never
-- updated; they go away only with their incident, when its server row is
-- deleted.
CREATE TABLE incident_events (
    id          bigserial        PRIMARY KEY,
    incident_id uuid             NOT NULL REFERENCES incidents (id) ON DELETE CASCADE,
    -- Timestamp of the sample that caused the change, or the server clock
    -- when no sample did (a removed threshold).
    occurred_at timestamptz      NOT NULL,
    recorded_at timestamptz      NOT NULL DEFAULT now(),
    kind        text             NOT NULL
                                 CHECK (kind IN ('opened', 'escalated', 'deescalated', 'resolved')),
    -- Severity after the change; NULL once resolved.
    severity    text             CHECK (severity IN ('warning', 'critical')),
    value       double precision,
    details     jsonb            NOT NULL DEFAULT '{}'::jsonb,
    CHECK ((kind = 'resolved') = (severity IS NULL))
);
COMMENT ON TABLE incident_events IS 'Changes of incidents: opened, escalated, deescalated, resolved. Never updated.';
CREATE INDEX incident_events_incident_idx ON incident_events (incident_id, id);

CREATE FUNCTION incident_events_no_update() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'incident_events rows are never changed: % is not allowed', TG_OP
        USING ERRCODE = 'insufficient_privilege';
END;
$$;

CREATE TRIGGER incident_events_no_update
    BEFORE UPDATE ON incident_events
    FOR EACH ROW EXECUTE FUNCTION incident_events_no_update();

CREATE TRIGGER incident_events_no_truncate
    BEFORE TRUNCATE ON incident_events
    FOR EACH STATEMENT EXECUTE FUNCTION incident_events_no_update();

-- Evaluation state of a series that is not OK: pending timers and the
-- severity of its open incident. A series back to OK has no row. Times
-- are sample timestamps.
CREATE TABLE alert_states (
    server_id      uuid        NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    metric         text        NOT NULL,
    instance       text        NOT NULL,
    -- Severity of the open incident, NULL while only pending.
    severity       text        CHECK (severity IN ('warning', 'critical')),
    -- Since when the warning or critical condition holds, NULL when not.
    warning_since  timestamptz,
    critical_since timestamptz,
    -- The last sample evaluated when the row was written.
    last_sample_at timestamptz NOT NULL,
    updated_at     timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (server_id, metric, instance),
    CHECK (severity IS NOT NULL OR warning_since IS NOT NULL OR critical_since IS NOT NULL)
);
COMMENT ON TABLE alert_states IS 'Threshold engine state of series that are pending or firing; reloaded at start.';
