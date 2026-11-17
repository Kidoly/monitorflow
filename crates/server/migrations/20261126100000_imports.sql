-- Moving from another monitoring tool (ADR-0146).

-- A server made by an import, waiting for its agent: no certificate yet,
-- never seen. Its first agent's Hello clears it.
ALTER TABLE servers ADD COLUMN awaiting_agent boolean NOT NULL DEFAULT false;
COMMENT ON COLUMN servers.awaiting_agent IS
    'Made by an import and waiting for its agent; see ADR-0146.';
CREATE INDEX servers_awaiting_agent_idx ON servers (site_id) WHERE awaiting_agent;

-- Integrations hear of a server when its agent arrives, not before.
DROP TRIGGER integration_server_added ON servers;
CREATE TRIGGER integration_server_added
    AFTER INSERT ON servers
    FOR EACH ROW WHEN (NEW.revoked_at IS NULL AND NOT NEW.awaiting_agent)
    EXECUTE FUNCTION integration_server();
DROP TRIGGER integration_server_changed ON servers;
CREATE TRIGGER integration_server_changed
    AFTER UPDATE OF revoked_at ON servers
    FOR EACH ROW WHEN ((OLD.revoked_at IS NULL) <> (NEW.revoked_at IS NULL)
                       AND NOT OLD.awaiting_agent AND NOT NEW.awaiting_agent)
    EXECUTE FUNCTION integration_server();
CREATE TRIGGER integration_server_arrived
    AFTER UPDATE OF awaiting_agent ON servers
    FOR EACH ROW WHEN (OLD.awaiting_agent AND NOT NEW.awaiting_agent
                       AND NEW.revoked_at IS NULL)
    EXECUTE FUNCTION integration_server();

-- One import: its preview, then what was confirmed and undone.
CREATE TABLE import_runs (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    source       text NOT NULL CHECK (source ~ '^[a-z][a-z0-9_]{0,31}$'),
    site_id      uuid NOT NULL REFERENCES sites (id) ON DELETE CASCADE,
    -- What was read: file names, or the address connected to.
    input        text NOT NULL CHECK (input <> '' AND char_length(input) <= 1024),
    state        text NOT NULL DEFAULT 'preview'
                 CHECK (state IN ('preview', 'confirmed', 'undone')),
    -- The planned items, without secrets.
    items        jsonb NOT NULL CHECK (jsonb_typeof(items) = 'array'),
    -- Channels' secrets by item, sealed (ADR-0125); dropped once confirmed.
    secrets      text CHECK (secrets <> ''),
    -- What the confirmation and the undo did.
    result       jsonb,
    created_by   uuid REFERENCES users (id) ON DELETE SET NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    confirmed_by uuid REFERENCES users (id) ON DELETE SET NULL,
    confirmed_at timestamptz,
    undone_by    uuid REFERENCES users (id) ON DELETE SET NULL,
    undone_at    timestamptz,
    CHECK ((state = 'preview') = (confirmed_at IS NULL)),
    CHECK ((state = 'undone') = (undone_at IS NOT NULL))
);
COMMENT ON TABLE import_runs IS
    'Imports from another monitoring tool: preview, confirmation, undo; see ADR-0146.';
CREATE INDEX import_runs_site_idx ON import_runs (site_id, created_at DESC);

-- What a confirmation created, for the undo.
CREATE TABLE import_created (
    run_id    uuid NOT NULL REFERENCES import_runs (id) ON DELETE CASCADE,
    item_id   text NOT NULL CHECK (item_id <> '' AND octet_length(item_id) <= 600),
    object    text NOT NULL CHECK (object IN ('server', 'check', 'threshold', 'silence', 'channel')),
    object_id uuid NOT NULL,
    PRIMARY KEY (run_id, item_id)
);
COMMENT ON TABLE import_created IS
    'Rows an import created, removed again by its undo; see ADR-0146.';
CREATE INDEX import_created_object_idx ON import_created (object, object_id);
