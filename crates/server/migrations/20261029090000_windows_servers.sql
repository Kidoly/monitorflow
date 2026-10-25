-- Windows agents report os_family 'windows' (ADR-0047).
ALTER TABLE servers DROP CONSTRAINT servers_os_family_check;
ALTER TABLE servers ADD CONSTRAINT servers_os_family_check
    CHECK (os_family IN ('debian', 'rhel', 'windows'));
