-- Campaigns that update agents instead of packages (ADR-0053): each
-- server gets a self_update to this version, with the waves, windows and
-- failure thresholds of any campaign.
ALTER TABLE campaigns
    ADD COLUMN agent_version text CHECK (agent_version <> ''),
    ADD CONSTRAINT campaigns_agent_update_only CHECK (
        agent_version IS NULL
        OR (packages = '{}' AND NOT security_only AND NOT allow_reboot AND sources_mode IS NULL)
    );
COMMENT ON COLUMN campaigns.agent_version IS
    'Set for agent update campaigns: the version every server self-updates to; see ADR-0053.';

-- Per-site policy (ADR-0053): when on, the server updates the site's
-- agents to the newest release it holds. Off by default.
ALTER TABLE sites ADD COLUMN agent_auto_update boolean NOT NULL DEFAULT false;

-- Each server is updated automatically at most once per version, so a
-- release that rolls back is not retried in a loop.
CREATE TABLE agent_auto_updates (
    server_id  uuid        NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    version    text        NOT NULL CHECK (version <> ''),
    command_id uuid        REFERENCES commands (id) ON DELETE SET NULL,
    queued_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (server_id, version)
);
COMMENT ON TABLE agent_auto_updates IS
    'self_update commands queued by the per-site policy, one per server and version; see ADR-0053.';
