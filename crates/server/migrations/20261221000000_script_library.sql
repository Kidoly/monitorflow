-- The script library and automation rules (ADR-0179).

-- A script kept in MonitorFlow, for every customer (customer_id NULL) or
-- one. Its text and parameters are in script_versions; `version` is the
-- current one. Deleted scripts keep their rows for the runs that name them.
CREATE TABLE scripts (
    id           uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    customer_id  uuid        REFERENCES customers (id) ON DELETE CASCADE,
    -- What people and the AI call it: `mf-script <name>`.
    name         text        NOT NULL CHECK (name ~ '^[a-z0-9][a-z0-9_.-]{0,62}$'),
    description  text        NOT NULL DEFAULT ''
                             CHECK (char_length(description) <= 2000),
    interpreter  text        NOT NULL CHECK (interpreter IN ('sh', 'powershell')),
    -- The least role, on every target's site, that may run it.
    run_role     text        NOT NULL CHECK (run_role IN ('operator', 'approver', 'administrator')),
    -- Whether the AI may propose it as a fix.
    ai_allowed   boolean     NOT NULL DEFAULT false,
    timeout_s    integer     NOT NULL DEFAULT 600 CHECK (timeout_s BETWEEN 10 AND 3600),
    version      integer     NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_by   uuid        REFERENCES users (id) ON DELETE SET NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    deleted_at   timestamptz
);
COMMENT ON TABLE scripts IS 'The script library; see ADR-0179.';
CREATE UNIQUE INDEX scripts_name_key ON scripts (customer_id, name) NULLS NOT DISTINCT
    WHERE deleted_at IS NULL;

-- Every version of a script, kept: a run names the one it ran.
CREATE TABLE script_versions (
    script_id   uuid        NOT NULL REFERENCES scripts (id) ON DELETE CASCADE,
    version     integer     NOT NULL CHECK (version >= 1),
    body        text        NOT NULL CHECK (octet_length(body) BETWEEN 1 AND 65536),
    -- [{name, label, type, required, default, choices}], see
    -- scripts::Parameter.
    parameters  jsonb       NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(parameters) = 'array'),
    -- What changed, in the author's words.
    note        text        NOT NULL DEFAULT '' CHECK (char_length(note) <= 500),
    created_by  uuid        REFERENCES users (id) ON DELETE SET NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (script_id, version)
);
COMMENT ON TABLE script_versions IS 'Versions of library scripts; see ADR-0179.';

-- Run a script when something happens. Covers everything, a customer or
-- a site, like integration endpoints.
CREATE TABLE automation_rules (
    id               uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    name             text        NOT NULL CHECK (char_length(name) BETWEEN 1 AND 80),
    customer_id      uuid        REFERENCES customers (id) ON DELETE CASCADE,
    site_id          uuid        REFERENCES sites (id) ON DELETE CASCADE,
    event            text        NOT NULL
                                 CHECK (event IN ('incident_opened', 'check_failed', 'server_joined')),
    -- The incident's metric (check_failed: required; incident_opened:
    -- any when NULL).
    metric           text        CHECK (metric ~ '^[A-Za-z_:][A-Za-z0-9_:.]{0,199}$'),
    script_id        uuid        NOT NULL REFERENCES scripts (id) ON DELETE CASCADE,
    parameters       jsonb       NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(parameters) = 'object'),
    enabled          boolean     NOT NULL DEFAULT true,
    -- At most one run per server in this many minutes.
    cooldown_minutes integer     NOT NULL DEFAULT 60 CHECK (cooldown_minutes BETWEEN 1 AND 10080),
    created_by       uuid        REFERENCES users (id) ON DELETE SET NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    CHECK (customer_id IS NULL OR site_id IS NULL),
    CHECK (event <> 'check_failed' OR metric IS NOT NULL),
    CHECK (event <> 'server_joined' OR metric IS NULL)
);
COMMENT ON TABLE automation_rules IS 'Scripts run when something happens; see ADR-0179.';
CREATE INDEX automation_rules_script_idx ON automation_rules (script_id);

-- A run of one script version on its servers: by a person, a rule or
-- the AI's remediation (which keeps its own record and makes none here).
-- `state`: awaiting_approval, then scheduled, then dispatched; or
-- rejected, expired (no decision in time), cancelled, refused (forbidden
-- by the rules).
CREATE TABLE script_runs (
    id              uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    script_id       uuid        NOT NULL,
    version         integer     NOT NULL,
    customer_id     uuid        NOT NULL REFERENCES customers (id) ON DELETE CASCADE,
    parameters      jsonb       NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(parameters) = 'object'),
    -- What was asked: {"kind": "servers" | "site" | "business_service", ...}.
    target          jsonb       NOT NULL CHECK (jsonb_typeof(target) = 'object'),
    requested_by    uuid        REFERENCES users (id) ON DELETE SET NULL,
    rule_id         uuid        REFERENCES automation_rules (id) ON DELETE SET NULL,
    -- The event that fired the rule.
    trigger         jsonb,
    state           text        NOT NULL CHECK (state IN ('awaiting_approval', 'scheduled',
                                    'dispatched', 'rejected', 'expired', 'cancelled', 'refused')),
    -- The classification of the risk rules (ADR-0041), the highest of its
    -- servers.
    tier            text        NOT NULL
                                CHECK (tier IN ('read_only', 'low_impact', 'high_impact', 'forbidden')),
    rule            jsonb       NOT NULL,
    reason          text        NOT NULL,
    scheduled_at    timestamptz NOT NULL DEFAULT now(),
    created_at      timestamptz NOT NULL DEFAULT now(),
    decided_by      uuid        REFERENCES users (id) ON DELETE SET NULL,
    decided_at      timestamptz,
    decision_reason text        CHECK (char_length(decision_reason) <= 1000),
    dispatched_at   timestamptz,
    FOREIGN KEY (script_id, version) REFERENCES script_versions (script_id, version)
        ON DELETE CASCADE,
    CHECK ((state = 'dispatched') = (dispatched_at IS NOT NULL))
);
COMMENT ON TABLE script_runs IS 'Runs of library scripts; see ADR-0179.';
CREATE INDEX script_runs_script_idx ON script_runs (script_id, created_at);
CREATE INDEX script_runs_due_idx ON script_runs (scheduled_at) WHERE state = 'scheduled';
CREATE INDEX script_runs_waiting_idx ON script_runs (created_at)
    WHERE state = 'awaiting_approval';

-- The servers of a run, frozen when it is made, with their command; the
-- command row keeps each server's output.
CREATE TABLE script_run_targets (
    run_id     uuid NOT NULL REFERENCES script_runs (id) ON DELETE CASCADE,
    server_id  uuid NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    command_id uuid REFERENCES commands (id) ON DELETE SET NULL,
    -- Why the server got no command.
    error      text CHECK (char_length(error) <= 1000),
    PRIMARY KEY (run_id, server_id)
);
COMMENT ON TABLE script_run_targets IS 'Servers of script runs and their commands; see ADR-0179.';
CREATE INDEX script_run_targets_server_idx ON script_run_targets (server_id);
CREATE UNIQUE INDEX script_run_targets_command_key ON script_run_targets (command_id);

-- Each event a rule took, once, with the run it made or why it made none.
CREATE TABLE automation_firings (
    rule_id    uuid        NOT NULL REFERENCES automation_rules (id) ON DELETE CASCADE,
    -- integration_events.id; events are kept 30 days, firings as long.
    event_id   uuid        NOT NULL,
    server_id  uuid        REFERENCES servers (id) ON DELETE CASCADE,
    run_id     uuid        REFERENCES script_runs (id) ON DELETE SET NULL,
    skipped    text        CHECK (char_length(skipped) <= 1000),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (rule_id, event_id)
);
COMMENT ON TABLE automation_firings IS 'Events automation rules took; see ADR-0179.';
CREATE INDEX automation_firings_cooldown_idx ON automation_firings (rule_id, server_id, created_at)
    WHERE run_id IS NOT NULL;
