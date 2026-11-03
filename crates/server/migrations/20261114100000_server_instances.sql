-- Several server instances on one database (ADR-0089). Each running
-- process has a row it refreshes every few seconds; a row that stops moving
-- is a process that died. A single server has one row and behaves as
-- before.
CREATE TABLE server_instances (
    id           uuid        PRIMARY KEY,
    hostname     text        NOT NULL,
    version      text        NOT NULL,
    started_at   timestamptz NOT NULL DEFAULT now(),
    heartbeat_at timestamptz NOT NULL DEFAULT now()
);
COMMENT ON TABLE server_instances IS
    'Running server processes and their heartbeats. See ADR-0089.';

-- Which instance holds the stream of each agent and relay, so every
-- instance shows the same fleet and the offline check sees streams held
-- elsewhere. Rows of a dead instance go with it.
CREATE TABLE peer_streams (
    peer_role     text        NOT NULL CHECK (peer_role IN ('agent', 'relay')),
    peer_id       text        NOT NULL,
    instance_id   uuid        NOT NULL REFERENCES server_instances (id) ON DELETE CASCADE,
    connected_at  timestamptz NOT NULL,
    via_relay     text,
    agent_version text        NOT NULL DEFAULT '',
    hostname      text        NOT NULL DEFAULT '',
    PRIMARY KEY (peer_role, peer_id, instance_id)
);
COMMENT ON TABLE peer_streams IS
    'Live agent and relay streams per server instance. See ADR-0089.';

-- Work only one instance does at a time: whoever holds the row until
-- `expires_at`, renewing it.
CREATE TABLE cluster_leases (
    name       text        PRIMARY KEY,
    holder     uuid        NOT NULL,
    expires_at timestamptz NOT NULL
);
COMMENT ON TABLE cluster_leases IS
    'Leader leases between server instances. See ADR-0089.';

-- Every instance compares the revoked certificates with its own CRL.
CREATE INDEX certificates_revoked_idx ON certificates (revoked_at)
    WHERE revoked_at IS NOT NULL;

-- A queued command wakes the instance holding its peer's stream at once;
-- the dispatchers' polls remain the fallback.
CREATE FUNCTION notify_command_queued() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    PERFORM pg_notify('monitorflow_commands', s.peer_id)
    FROM servers s WHERE s.id = NEW.server_id;
    RETURN NULL;
END
$$;

CREATE TRIGGER commands_notify_queued
    AFTER INSERT ON commands
    FOR EACH ROW EXECUTE FUNCTION notify_command_queued();

CREATE FUNCTION notify_relay_command_queued() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    PERFORM pg_notify('monitorflow_relay_commands', r.peer_id)
    FROM relays r WHERE r.id = NEW.relay_id;
    RETURN NULL;
END
$$;

CREATE TRIGGER relay_commands_notify_queued
    AFTER INSERT ON relay_commands
    FOR EACH ROW EXECUTE FUNCTION notify_relay_command_queued();
