-- Agents on any Linux distribution (ADR-0063): the 'linux' family, and
-- whether the host can take package updates and sources at all.
ALTER TABLE servers DROP CONSTRAINT servers_os_family_check;
ALTER TABLE servers ADD CONSTRAINT servers_os_family_check
    CHECK (os_family IN ('debian', 'rhel', 'windows', 'linux'));
ALTER TABLE servers ADD COLUMN package_updates boolean NOT NULL DEFAULT true;
COMMENT ON COLUMN servers.package_updates IS
    'False when the agent reports no package manager it can drive: updates and sources are unavailable. See ADR-0063.';
