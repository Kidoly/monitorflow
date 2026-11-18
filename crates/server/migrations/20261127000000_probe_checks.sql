-- Network checks from a probe, and devices without an agent (ADR-0148).

-- A device is watched from a probe only: no agent, no certificate, never
-- waiting for one. It keeps the servers row so its page, incidents,
-- thresholds, silences and dashboards work as for any server.
ALTER TABLE servers ADD COLUMN kind text NOT NULL DEFAULT 'agent'
    CHECK (kind IN ('agent', 'device'));
COMMENT ON COLUMN servers.kind IS
    'agent: runs the MonitorFlow agent; device: watched from a probe only. See ADR-0148.';
-- The address probes reach a device at: an IP address or a DNS name.
ALTER TABLE servers ADD COLUMN address text
    CHECK (char_length(address) BETWEEN 1 AND 253 AND address !~ '[[:space:][:cntrl:]/]');
ALTER TABLE servers ADD CONSTRAINT servers_device_shape
    CHECK (kind = 'agent' OR (address IS NOT NULL AND NOT awaiting_agent AND NOT package_updates));
ALTER TABLE servers DROP CONSTRAINT servers_os_family_check;
ALTER TABLE servers ADD CONSTRAINT servers_os_family_check
    CHECK (os_family IN ('debian', 'rhel', 'windows', 'linux', 'device'));
ALTER TABLE servers ADD CONSTRAINT servers_device_family
    CHECK ((kind = 'device') = (os_family = 'device'));
CREATE INDEX servers_devices_idx ON servers (site_id) WHERE kind = 'device';

-- Who runs a check: NULL, the server's agent; 'central', the central
-- server; 'relay', the relay probe_relay_id. Only a server's own row has a
-- probe: a site row is about every agent of the site.
ALTER TABLE sensor_configs ADD COLUMN probe text CHECK (probe IN ('central', 'relay'));
ALTER TABLE sensor_configs ADD COLUMN probe_relay_id uuid REFERENCES relays (id) ON DELETE CASCADE;
ALTER TABLE sensor_configs ADD CONSTRAINT sensor_configs_probe_shape
    CHECK ((probe = 'relay') = (probe_relay_id IS NOT NULL)
           AND (probe IS NULL OR server_id IS NOT NULL));
COMMENT ON COLUMN sensor_configs.probe IS
    'Who runs the check: NULL the agent, central the central server, relay probe_relay_id. See ADR-0148.';
CREATE INDEX sensor_configs_probe_idx ON sensor_configs (probe, probe_relay_id)
    WHERE probe IS NOT NULL;
