-- Past runs of a campaign (ADR-0173). A finished campaign edited or run
-- again keeps what its last run did here: its outcome, the settings it
-- ran with, and its batches and servers, moved out of campaign_waves and
-- campaign_targets so those hold the next run only.
CREATE TABLE campaign_runs (
    campaign_id uuid        NOT NULL REFERENCES campaigns (id) ON DELETE CASCADE,
    -- 1 for the first run, then each one after.
    number      integer     NOT NULL CHECK (number >= 1),
    state       text        NOT NULL CHECK (state IN ('stopped', 'completed', 'failed')),
    error       text,
    started_at  timestamptz,
    finished_at timestamptz NOT NULL,
    -- The campaign's settings for that run, as the API returns them.
    settings    jsonb       NOT NULL,
    archived_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (campaign_id, number)
);
COMMENT ON TABLE campaign_runs IS 'Past runs of a campaign; see ADR-0173.';

CREATE TABLE campaign_run_waves (
    campaign_id uuid        NOT NULL,
    run         integer     NOT NULL,
    position    integer     NOT NULL CHECK (position >= 0),
    started_at  timestamptz,
    finished_at timestamptz,
    PRIMARY KEY (campaign_id, run, position),
    FOREIGN KEY (campaign_id, run) REFERENCES campaign_runs (campaign_id, number)
        ON DELETE CASCADE
);
COMMENT ON TABLE campaign_run_waves IS 'Batches of a past campaign run; see ADR-0173.';

CREATE TABLE campaign_run_targets (
    campaign_id         uuid    NOT NULL,
    run                 integer NOT NULL,
    server_id           uuid    NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    wave                integer NOT NULL,
    command_id          uuid    REFERENCES commands (id) ON DELETE SET NULL,
    error               text,
    sources_command_id  uuid    REFERENCES commands (id) ON DELETE SET NULL,
    restore_command_id  uuid    REFERENCES commands (id) ON DELETE SET NULL,
    snapshot_command_id uuid    REFERENCES commands (id) ON DELETE SET NULL,
    PRIMARY KEY (campaign_id, run, server_id),
    FOREIGN KEY (campaign_id, run, wave) REFERENCES campaign_run_waves (campaign_id, run, position)
        ON DELETE CASCADE
);
COMMENT ON TABLE campaign_run_targets IS 'Servers of a past campaign run and their commands; see ADR-0173.';
