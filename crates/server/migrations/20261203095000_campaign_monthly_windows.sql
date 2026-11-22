-- Monthly and yearly maintenance windows of campaigns (ADR-0167), with
-- the rule of recurring silences (ADR-0151): a day of the month, or a
-- weekday of a week of the month (5 is the last), in chosen months. One
-- month makes a yearly window. The weekday is the one element of
-- `weekdays`.
ALTER TABLE campaign_windows
    ADD COLUMN month_day  smallint   CHECK (month_day BETWEEN 1 AND 31),
    ADD COLUMN month_week smallint   CHECK (month_week BETWEEN 1 AND 5),
    ADD COLUMN months     smallint[] CHECK (cardinality(months) BETWEEN 1 AND 12
                                            AND months <@ '{1,2,3,4,5,6,7,8,9,10,11,12}'),
    DROP CONSTRAINT campaign_windows_kind_check,
    ADD CONSTRAINT campaign_windows_kind_check CHECK (kind IN ('weekly', 'monthly', 'once')),
    ADD CONSTRAINT campaign_windows_not_monthly CHECK (
        kind = 'monthly' OR (month_day IS NULL AND month_week IS NULL AND months IS NULL)),
    ADD CONSTRAINT campaign_windows_monthly CHECK (kind <> 'monthly' OR (
        months IS NOT NULL AND start_minute BETWEEN 0 AND 1439 AND end_minute BETWEEN 1 AND 1440
        AND start_minute <> end_minute AND timezone IS NOT NULL
        AND starts_at IS NULL AND ends_at IS NULL
        AND ((month_day IS NOT NULL AND month_week IS NULL AND weekdays IS NULL)
             OR (month_day IS NULL AND month_week IS NOT NULL AND cardinality(weekdays) = 1
                 AND weekdays <@ '{1,2,3,4,5,6,7}'))));

-- Whether a recurring window of this rule opens on the local date `p_day`.
CREATE FUNCTION campaign_window_day(
    p_kind text, p_weekdays smallint[], p_month_day smallint, p_month_week smallint,
    p_months smallint[], p_day date
) RETURNS boolean
LANGUAGE sql IMMUTABLE AS $$
    SELECT CASE p_kind
        WHEN 'weekly' THEN extract(isodow FROM p_day)::smallint = ANY (p_weekdays)
        WHEN 'monthly' THEN extract(month FROM p_day)::smallint = ANY (p_months) AND CASE
            WHEN p_month_day IS NOT NULL THEN extract(day FROM p_day)::smallint = p_month_day
            ELSE extract(isodow FROM p_day)::smallint = ANY (p_weekdays) AND CASE
                -- The last: a week later is next month.
                WHEN p_month_week = 5 THEN extract(month FROM p_day + 7) <> extract(month FROM p_day)
                ELSE (extract(day FROM p_day)::int - 1) / 7 + 1 = p_month_week
            END
        END
        ELSE false
    END
$$;

-- As before (20261024090000_campaigns), with the day of a recurring
-- window read by campaign_window_day.
CREATE OR REPLACE FUNCTION campaign_window_end(p_campaign uuid, p_at timestamptz) RETURNS timestamptz
LANGUAGE sql STABLE AS $$
    SELECT CASE
        WHEN NOT EXISTS (SELECT 1 FROM campaign_windows WHERE campaign_id = p_campaign)
            THEN 'infinity'::timestamptz
        ELSE (
            SELECT max(CASE
                WHEN w.kind = 'once' THEN
                    CASE WHEN p_at >= w.starts_at AND p_at < w.ends_at THEN w.ends_at END
                -- Open since today's start.
                WHEN campaign_window_day(w.kind, w.weekdays, w.month_day, w.month_week, w.months,
                                         l.local::date)
                     AND l.since >= make_interval(mins => w.start_minute)
                     AND (w.end_minute < w.start_minute
                          OR l.since < make_interval(mins => w.end_minute))
                    THEN (l.local::date + make_interval(mins => w.end_minute)
                          + CASE WHEN w.end_minute < w.start_minute
                                 THEN interval '1 day' ELSE interval '0' END)
                         AT TIME ZONE w.timezone
                -- Open since yesterday's start, past midnight.
                WHEN w.end_minute < w.start_minute
                     AND campaign_window_day(w.kind, w.weekdays, w.month_day, w.month_week,
                                             w.months, l.local::date - 1)
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
