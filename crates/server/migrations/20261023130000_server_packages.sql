-- The latest package report of each server (ADR-0031). A report replaces
-- the whole inventory of its server.
ALTER TABLE servers
    ADD COLUMN reboot_required      boolean,
    -- When the agent collected its latest report, by the agent's clock.
    ADD COLUMN packages_reported_at timestamptz;

CREATE TABLE server_packages (
    server_id          uuid NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    name               text NOT NULL CHECK (name <> '' AND octet_length(name) <= 256),
    arch               text NOT NULL CHECK (octet_length(arch) <= 64),
    version            text NOT NULL CHECK (octet_length(version) <= 256),
    -- The candidate version when an upgrade is available.
    upgradable_version text CHECK (upgradable_version <> '' AND octet_length(upgradable_version) <= 256),
    PRIMARY KEY (server_id, name, arch)
);
COMMENT ON TABLE server_packages IS 'Installed packages per server from the latest report; see ADR-0031.';
CREATE INDEX server_packages_upgradable_idx ON server_packages (server_id, name, arch)
    WHERE upgradable_version IS NOT NULL;
