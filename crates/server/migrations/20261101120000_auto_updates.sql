-- Automatic updates (ADR-0055): on by default for the whole server, with
-- a per-site override, rolled out a canary per site first and stopped
-- when updates fail.

-- The server-wide settings, in the one row of server_settings (ADR-0054);
-- NULL leaves each to its default. `enabled` (on) is the default of
-- sites that inherit it; `paused` (off) stops every automatic update at
-- once; a rollout stops once `failure_threshold` (2) of its updates failed.
ALTER TABLE server_settings
    ADD COLUMN auto_update_enabled boolean,
    ADD COLUMN auto_update_paused boolean,
    ADD COLUMN auto_update_failure_threshold integer
        CHECK (auto_update_failure_threshold BETWEEN 1 AND 1000);

-- Sites inherit the server-wide setting unless set: NULL inherits. Every
-- existing site inherits, so automatic updates are on unless turned off.
ALTER TABLE sites
    ALTER COLUMN agent_auto_update DROP NOT NULL,
    ALTER COLUMN agent_auto_update DROP DEFAULT;
UPDATE sites SET agent_auto_update = NULL;
COMMENT ON COLUMN sites.agent_auto_update IS
    'Automatic agent and relay updates for the site: NULL follows server_settings.auto_update_enabled; see ADR-0055.';

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
