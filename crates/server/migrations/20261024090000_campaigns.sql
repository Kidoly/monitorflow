-- Update campaigns (ADR-0033): `apply_updates` commands for a set of
-- servers of one customer, frozen at creation and split into ordered
-- waves, queued only inside the campaign's maintenance windows, stopped
-- when a wave fails beyond a threshold.
CREATE TABLE campaigns (
    id                     uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    customer_id            uuid        NOT NULL REFERENCES customers (id),
    name                   text        NOT NULL CHECK (name <> ''),
    -- The site named at creation, if any; the frozen targets are in
    -- campaign_targets.
    site_id                uuid        REFERENCES sites (id),
    -- The ApplyUpdates spec of every command (ADR-0031).
    packages               text[]      NOT NULL DEFAULT '{}',
    security_only          boolean     NOT NULL DEFAULT false,
    allow_reboot           boolean     NOT NULL DEFAULT false,
    -- Upper bound of each command's time to live; also capped at the end
    -- of the window it is queued in.
    command_ttl_s          integer     NOT NULL DEFAULT 3600
                                       CHECK (command_ttl_s BETWEEN 60 AND 604800),
    -- A wave fails when more of its servers failed than this many, or
    -- this percentage of the wave.
    failure_threshold      integer     NOT NULL DEFAULT 0 CHECK (failure_threshold >= 0),
    failure_threshold_unit text        NOT NULL DEFAULT 'count'
                                       CHECK (failure_threshold_unit IN ('count', 'percent')),
    state                  text        NOT NULL DEFAULT 'draft'
                                       CHECK (state IN ('draft', 'scheduled', 'running', 'paused',
                                                        'stopped', 'completed', 'failed')),
    -- The wave in progress, from 0; NULL until the first one starts.
    current_wave           integer     CHECK (current_wave >= 0),
    -- Why it stopped or failed, in one line.
    error                  text,
    created_by             uuid        REFERENCES users (id),
    created_at             timestamptz NOT NULL DEFAULT now(),
    started_at             timestamptz,
    finished_at            timestamptz,
    CHECK (failure_threshold_unit = 'count' OR failure_threshold <= 100),
    CHECK ((state IN ('stopped', 'completed', 'failed')) = (finished_at IS NOT NULL)),
    CHECK (state = 'draft' OR started_at IS NOT NULL)
);
COMMENT ON TABLE campaigns IS 'Update campaigns: waves of apply_updates commands; see ADR-0033.';
CREATE INDEX campaigns_customer_idx ON campaigns (customer_id, created_at);
-- What the runner looks at.
CREATE INDEX campaigns_active_idx ON campaigns (created_at) WHERE state IN ('scheduled', 'running');

CREATE TABLE campaign_waves (
    campaign_id uuid        NOT NULL REFERENCES campaigns (id) ON DELETE CASCADE,
    position    integer     NOT NULL CHECK (position >= 0),
    started_at  timestamptz,
    finished_at timestamptz,
    PRIMARY KEY (campaign_id, position),
    CHECK (finished_at IS NULL OR started_at IS NOT NULL)
);
COMMENT ON TABLE campaign_waves IS 'Ordered waves of a campaign; see ADR-0033.';

CREATE TABLE campaign_targets (
    campaign_id uuid    NOT NULL,
    server_id   uuid    NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    wave        integer NOT NULL,
    -- The server's current command of this campaign; NULL until queued,
    -- and again after a pause cancelled it unsent.
    command_id  uuid    REFERENCES commands (id) ON DELETE SET NULL,
    -- Why the server got no command, like a revocation since creation.
    error       text,
    PRIMARY KEY (campaign_id, server_id),
    FOREIGN KEY (campaign_id, wave) REFERENCES campaign_waves (campaign_id, position)
        ON DELETE CASCADE
);
COMMENT ON TABLE campaign_targets IS 'Servers of a campaign, their wave and command; see ADR-0033.';
CREATE INDEX campaign_targets_wave_idx ON campaign_targets (campaign_id, wave);
CREATE UNIQUE INDEX campaign_targets_command_key ON campaign_targets (command_id);

-- A weekly window opens on each of `weekdays` (ISO, 1 is Monday) at
-- `start_minute` past local midnight in `timezone` and closes at
-- `end_minute`; an end before the start closes the next day. A one-off
-- window is the range [starts_at, ends_at). A campaign without windows
-- may queue at any time.
CREATE TABLE campaign_windows (
    campaign_id  uuid        NOT NULL REFERENCES campaigns (id) ON DELETE CASCADE,
    -- Order of the request, for display.
    position     integer     NOT NULL CHECK (position >= 0),
    kind         text        NOT NULL CHECK (kind IN ('weekly', 'once')),
    weekdays     smallint[],
    start_minute smallint,
    end_minute   smallint,
    timezone     text,
    starts_at    timestamptz,
    ends_at      timestamptz,
    CHECK (kind <> 'weekly' OR (
        weekdays IS NOT NULL AND start_minute IS NOT NULL AND end_minute IS NOT NULL
        AND cardinality(weekdays) BETWEEN 1 AND 7 AND weekdays <@ '{1,2,3,4,5,6,7}'
        AND start_minute BETWEEN 0 AND 1439 AND end_minute BETWEEN 1 AND 1440
        AND start_minute <> end_minute AND timezone IS NOT NULL
        AND starts_at IS NULL AND ends_at IS NULL)),
    CHECK (kind <> 'once' OR (
        starts_at IS NOT NULL AND ends_at IS NOT NULL AND starts_at < ends_at AND weekdays IS NULL AND start_minute IS NULL
        AND end_minute IS NULL AND timezone IS NULL)),
    PRIMARY KEY (campaign_id, position)
);
COMMENT ON TABLE campaign_windows IS 'Maintenance windows of a campaign; see ADR-0033.';

-- When the windows of `p_campaign` open at `p_at` close: the latest end among
-- them, NULL when none is open, 'infinity' when the campaign has none.
-- Local times use PostgreSQL's time zone database.
CREATE FUNCTION campaign_window_end(p_campaign uuid, p_at timestamptz) RETURNS timestamptz
LANGUAGE sql STABLE AS $$
    SELECT CASE
        WHEN NOT EXISTS (SELECT 1 FROM campaign_windows WHERE campaign_id = p_campaign)
            THEN 'infinity'::timestamptz
        ELSE (
            SELECT max(CASE
                WHEN w.kind = 'once' THEN
                    CASE WHEN p_at >= w.starts_at AND p_at < w.ends_at THEN w.ends_at END
                -- Open since today's start.
                WHEN extract(isodow FROM l.local)::smallint = ANY (w.weekdays)
                     AND l.since >= make_interval(mins => w.start_minute)
                     AND (w.end_minute < w.start_minute
                          OR l.since < make_interval(mins => w.end_minute))
                    THEN (l.local::date + make_interval(mins => w.end_minute)
                          + CASE WHEN w.end_minute < w.start_minute
                                 THEN interval '1 day' ELSE interval '0' END)
                         AT TIME ZONE w.timezone
                -- Open since yesterday's start, past midnight.
                WHEN w.end_minute < w.start_minute
                     AND extract(isodow FROM l.local - interval '1 day')::smallint = ANY (w.weekdays)
                     AND l.since < make_interval(mins => w.end_minute)
                    THEN (l.local::date + make_interval(mins => w.end_minute)) AT TIME ZONE w.timezone
            END)
            FROM campaign_windows w
            CROSS JOIN LATERAL (
                SELECT x.local, x.local - x.local::date AS since
                FROM (SELECT p_at AT TIME ZONE coalesce(w.timezone, 'UTC') AS local) x
            ) l
            WHERE w.campaign_id = p_campaign
        )
    END
$$;
