-- Precise and recurring silences (ADR-0151): a silence may cover chosen
-- checks of a server or site, and maintenance windows write silences that
-- come back on a schedule.

-- Maintenance windows: a rule, not a period. Their occurrences are rows of
-- `silences` with `window_id`, written ahead by the central server.
CREATE TABLE maintenance_windows (
    id               uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    -- The place, which gives who sees and manages it.
    site_id          uuid        NOT NULL REFERENCES sites (id) ON DELETE CASCADE,
    -- The whole site, servers added later included; else only the servers
    -- of `maintenance_window_servers`, so a removed server never widens it.
    whole_site       boolean     NOT NULL DEFAULT true,
    -- NULL: everything of the servers covered; else `[{name, instance}]`.
    checks           jsonb       CHECK (checks IS NULL OR (jsonb_typeof(checks) = 'array'
                                        AND jsonb_array_length(checks) BETWEEN 1 AND 20)),
    repeat           text        NOT NULL CHECK (repeat IN ('daily', 'weekly', 'monthly')),
    -- Every N days (daily) or weeks (weekly), counted from `starts_on`.
    every            smallint    NOT NULL DEFAULT 1 CHECK (every BETWEEN 1 AND 365),
    -- A bit per weekday, Monday first (bit 0): the days of a weekly window,
    -- the weekday of a monthly one by week.
    weekdays         smallint    NOT NULL DEFAULT 0 CHECK (weekdays BETWEEN 0 AND 127),
    -- Monthly: a day of the month, or a week (1 to 4, 5 the last) and the
    -- weekday in `weekdays`.
    month_day        smallint    CHECK (month_day BETWEEN 1 AND 31),
    month_week       smallint    CHECK (month_week BETWEEN 1 AND 5),
    -- A bit per month, January first (bit 0).
    months           smallint    NOT NULL DEFAULT 4095 CHECK (months BETWEEN 1 AND 4095),
    -- Local time of the start, in `time_zone`.
    start_time       time        NOT NULL,
    duration_minutes integer     NOT NULL CHECK (duration_minutes BETWEEN 1 AND 10080),
    time_zone        text        NOT NULL CHECK (time_zone <> ''),
    -- Local dates a window may start between; no end when NULL.
    starts_on        date        NOT NULL,
    ends_on          date        CHECK (ends_on IS NULL OR ends_on >= starts_on),
    reason           text        NOT NULL CHECK (reason <> '' AND char_length(reason) <= 200),
    created_by       uuid        REFERENCES users (id) ON DELETE SET NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    -- Removed windows stay, as the record of what was quiet and why.
    removed_at       timestamptz,
    removed_by       uuid        REFERENCES users (id) ON DELETE SET NULL,
    CHECK (repeat <> 'weekly' OR weekdays <> 0),
    CHECK (repeat <> 'monthly'
           OR (month_day IS NOT NULL AND month_week IS NULL)
           OR (month_day IS NULL AND month_week IS NOT NULL AND weekdays <> 0))
);
COMMENT ON TABLE maintenance_windows IS
    'Silences that come back on a schedule, in a time zone. See ADR-0151.';
CREATE INDEX maintenance_windows_site_idx ON maintenance_windows (site_id)
    WHERE removed_at IS NULL;

CREATE TABLE maintenance_window_servers (
    window_id uuid NOT NULL REFERENCES maintenance_windows (id) ON DELETE CASCADE,
    server_id uuid NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    PRIMARY KEY (window_id, server_id)
);

-- Chosen checks of a server or site; NULL is the whole target, as before.
ALTER TABLE silences
    ADD COLUMN checks jsonb CHECK (checks IS NULL OR (jsonb_typeof(checks) = 'array'
                                   AND jsonb_array_length(checks) BETWEEN 1 AND 20)),
    ADD COLUMN window_id uuid REFERENCES maintenance_windows (id) ON DELETE CASCADE,
    ADD CONSTRAINT silences_checks_of_a_server_or_site
        CHECK (checks IS NULL OR server_id IS NOT NULL OR site_id IS NOT NULL);
-- Occurrences of a window are written under a lock on it, once per
-- target and start for each version of its rule (`updated_at`).
CREATE INDEX silences_window_idx ON silences (window_id, starts_at) WHERE window_id IS NOT NULL;

-- Whether a check `(name, instance)` wrote `metric` of `instance`: the
-- metric, without `monitorflow_`, is the name or starts with `name_`, no
-- other check name is longer and prefixes it, and the instance is the
-- check's when it has one. Mirrors `notify::silence::check_covers`.
CREATE FUNCTION check_covers(check_name text, check_instance text,
                             metric_name text, metric_instance text)
RETURNS boolean
LANGUAGE sql STABLE AS $$
    WITH m AS (SELECT CASE WHEN metric_name LIKE 'monitorflow\_%' THEN substr(metric_name, 13)
                           ELSE metric_name END AS short)
    SELECT (check_instance = '' OR check_instance = metric_instance)
       AND (m.short = check_name OR starts_with(m.short, check_name || '_'))
       AND NOT EXISTS (
           SELECT 1 FROM sensor_configs sc
           WHERE char_length(sc.name) > char_length(check_name)
             AND (m.short = sc.name OR starts_with(m.short, sc.name || '_')))
    FROM m
$$;

-- The silences covering a place now: a server, relay, site or customer,
-- any of them NULL. With a `metric`, the incident of that metric and
-- instance, which silences of chosen checks may cover too; without one,
-- whole targets only, as a server with some checks silenced is not
-- silenced.
CREATE FUNCTION covering_silences(server uuid, relay uuid, site uuid, customer uuid,
                                  metric text, instance text)
RETURNS SETOF silences
LANGUAGE sql STABLE AS $$
    SELECT x.* FROM silences x
    WHERE x.starts_at <= now() AND coalesce(x.ended_at, x.ends_at) > now()
      AND (num_nonnulls(x.server_id, x.relay_id, x.site_id, x.customer_id) = 0
           OR x.server_id = server OR x.relay_id = relay
           OR x.site_id = site OR x.customer_id = customer)
      AND (x.checks IS NULL
           OR (metric IS NOT NULL AND server IS NOT NULL AND metric <> 'offline' AND EXISTS (
               SELECT 1 FROM jsonb_array_elements(x.checks) c
               WHERE check_covers(c ->> 'name', coalesce(c ->> 'instance', ''),
                                  metric, instance))))
$$;

-- When the longest silence now covering a place ends, or NULL.
CREATE OR REPLACE FUNCTION silenced_until(server uuid, relay uuid, site uuid, customer uuid)
RETURNS timestamptz
LANGUAGE sql STABLE AS $$
    SELECT max(coalesce(ended_at, ends_at))
    FROM covering_silences(server, relay, site, customer, NULL, NULL)
$$;
