-- Richer alert rules (ADR-0153): a threshold row has a kind, and the
-- engine keys thresholds, incidents and alert states by
-- (metric, instance, kind), so one series can have its threshold, a
-- forecast and a no-data rule at once.
--
-- value     the sample, as before (every existing row)
-- window    a function (avg, min, max, change, rate) over window_s seconds
-- compare   the average over window_s against offset_s earlier (ratio,
--           difference)
-- forecast  seconds left until the value reaches target on the side of
--           comparison, from a line fitted over window_s
-- nodata    seconds since the last sample arrived
ALTER TABLE thresholds
    ADD COLUMN kind       text             NOT NULL DEFAULT 'value'
                          CHECK (kind IN ('value', 'window', 'compare', 'forecast', 'nodata')),
    ADD COLUMN function   text
                          CHECK (function IN ('avg', 'min', 'max', 'change', 'rate',
                                              'ratio', 'difference')),
    ADD COLUMN window_s   integer          CHECK (window_s BETWEEN 60 AND 604800),
    ADD COLUMN offset_s   integer          CHECK (offset_s IN (86400, 604800)),
    ADD COLUMN target     double precision CHECK (target > '-Infinity' AND target < 'Infinity'),
    -- Up to two more conditions joined with "and", on the same server.
    ADD COLUMN conditions jsonb            NOT NULL DEFAULT '[]'::jsonb
                          CHECK (jsonb_typeof(conditions) = 'array'
                                 AND jsonb_array_length(conditions) <= 2),
    -- Each kind has exactly the fields it uses; a missing one makes the
    -- CASE NULL, which coalesce refuses.
    ADD CONSTRAINT thresholds_kind_fields CHECK (coalesce(
        CASE kind
            WHEN 'value' THEN function IS NULL AND window_s IS NULL AND offset_s IS NULL
                              AND target IS NULL
            WHEN 'window' THEN function IN ('avg', 'min', 'max', 'change', 'rate')
                               AND window_s <= 86400 AND offset_s IS NULL AND target IS NULL
            WHEN 'compare' THEN function IN ('ratio', 'difference') AND window_s >= 300
                                AND window_s <= 86400 AND offset_s IS NOT NULL
                                AND target IS NULL
            WHEN 'forecast' THEN function IS NULL AND window_s >= 3600 AND offset_s IS NULL
                                 AND target IS NOT NULL
            WHEN 'nodata' THEN function IS NULL AND window_s IS NULL AND offset_s IS NULL
                               AND target IS NULL AND duration_s = 0
                               AND conditions = '[]'::jsonb
        END, false));

-- Critical is at least as far on the alarm side as warning: less time
-- left for a forecast, a longer silence for no data, as the comparison
-- says otherwise. Replaces the unnamed CHECK of 20261019180000.
ALTER TABLE thresholds DROP CONSTRAINT thresholds_check2;
ALTER TABLE thresholds ADD CONSTRAINT thresholds_levels CHECK (
    warning IS NULL OR critical IS NULL
    OR (kind = 'forecast' AND critical <= warning)
    OR (kind = 'nodata' AND critical >= warning)
    OR (kind NOT IN ('forecast', 'nodata')
        AND ((comparison IN ('>', '>=') AND critical >= warning)
             OR (comparison IN ('<', '<=') AND critical <= warning))));

ALTER TABLE thresholds DROP CONSTRAINT thresholds_key;
ALTER TABLE thresholds ADD CONSTRAINT thresholds_key
    UNIQUE NULLS NOT DISTINCT (site_id, server_id, metric, instance, kind);

ALTER TABLE incidents ADD COLUMN kind text NOT NULL DEFAULT 'value'
    CHECK (kind IN ('value', 'window', 'compare', 'forecast', 'nodata'));
DROP INDEX incidents_one_open_key;
CREATE UNIQUE INDEX incidents_one_open_key ON incidents (server_id, metric, instance, kind)
    WHERE state = 'open';
COMMENT ON TABLE incidents IS
    'Threshold incidents; at most one open per (server, metric, instance, kind).';

ALTER TABLE alert_states ADD COLUMN kind text NOT NULL DEFAULT 'value'
    CHECK (kind IN ('value', 'window', 'compare', 'forecast', 'nodata'));
ALTER TABLE alert_states DROP CONSTRAINT alert_states_pkey;
ALTER TABLE alert_states ADD PRIMARY KEY (server_id, metric, instance, kind);

-- Notifications say what kind of rule fired, and when a forecast expects
-- the target: the time of the value plus the seconds left.
CREATE OR REPLACE FUNCTION notification_incident_event() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    i     incidents;
    place record;
BEGIN
    IF NEW.kind NOT IN ('opened', 'escalated', 'resolved') THEN
        RETURN NULL;
    END IF;
    SELECT * INTO i FROM incidents WHERE id = NEW.incident_id;
    SELECT * INTO place FROM notification_place(i.server_id, i.relay_id);
    INSERT INTO notification_outbox (event, customer_id, details)
    VALUES ('incident.' || NEW.kind, place.customer_id, jsonb_build_object(
        'occurred_at', NEW.occurred_at,
        'customer', place.customer,
        'site', place.site,
        'server', (SELECT notification_ref(id, hostname) FROM servers WHERE id = i.server_id),
        'relay', (SELECT notification_ref(id, name) FROM relays WHERE id = i.relay_id),
        'incident', jsonb_build_object(
            'id', i.id, 'metric', i.metric, 'instance', i.instance,
            -- The incident's severity when resolved, the new one otherwise.
            'severity', coalesce(NEW.severity, i.severity),
            'value', NEW.value, 'peak_value', i.peak_value,
            -- Why it changed, when the writer says: an offline incident
            -- resolves on back_online, revoked or muted.
            'reason', NEW.details ->> 'reason',
            'opened_at', i.opened_at, 'resolved_at', i.resolved_at,
            'kind', i.kind,
            'threshold', i.threshold)));
    RETURN NULL;
END;
$$;
