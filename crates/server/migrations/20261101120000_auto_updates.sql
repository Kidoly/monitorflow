-- Automatic updates (ADR-0055): on by default for the whole server, with
-- a per-site override, rolled out a canary per site first and stopped
-- when updates fail.

-- The server-wide settings, one row. `enabled` is the default of sites
-- that inherit it; `paused` stops every automatic update at once.
CREATE TABLE auto_update_settings (
    id                boolean     PRIMARY KEY DEFAULT true CHECK (id),
    enabled           boolean     NOT NULL DEFAULT true,
    paused            boolean     NOT NULL DEFAULT false,
    -- A rollout stops once this many of its updates failed.
    failure_threshold integer     NOT NULL DEFAULT 2 CHECK (failure_threshold BETWEEN 1 AND 1000),
    updated_at        timestamptz NOT NULL DEFAULT now(),
    updated_by        uuid        REFERENCES users (id) ON DELETE SET NULL
);
COMMENT ON TABLE auto_update_settings IS
    'Server-wide automatic update settings, one row; see ADR-0055.';
INSERT INTO auto_update_settings DEFAULT VALUES;

-- Sites inherit the server-wide setting unless set: NULL inherits. Every
-- existing site inherits, so automatic updates are on unless turned off.
ALTER TABLE sites
    ALTER COLUMN agent_auto_update DROP NOT NULL,
    ALTER COLUMN agent_auto_update DROP DEFAULT;
UPDATE sites SET agent_auto_update = NULL;
COMMENT ON COLUMN sites.agent_auto_update IS
    'Automatic agent and relay updates for the site: NULL follows auto_update_settings; see ADR-0055.';

-- One automatic rollout per kind of peer and version, started when the
-- server first queues an update to it; halted when too many failed.
CREATE TABLE auto_rollouts (
    kind       text        NOT NULL CHECK (kind IN ('agent', 'relay')),
    version    text        NOT NULL CHECK (version <> ''),
    state      text        NOT NULL DEFAULT 'rolling' CHECK (state IN ('rolling', 'halted')),
    started_at timestamptz NOT NULL DEFAULT now(),
    halted_at  timestamptz,
    -- Why it halted, in one line.
    reason     text,
    -- Failures count from here: the start, or the last resume.
    counted_from timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (kind, version),
    CHECK ((state = 'halted') = (halted_at IS NOT NULL))
);
COMMENT ON TABLE auto_rollouts IS
    'Automatic rollouts of a release to agents or relays; see ADR-0055.';
