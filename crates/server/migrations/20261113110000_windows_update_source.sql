-- Where a Windows server takes its updates from, from its latest package
-- report (ADR-0086): `UpdateSource` as JSON. Null on Linux and until an
-- agent that reports it sends a report.
ALTER TABLE servers ADD COLUMN windows_update_source jsonb;
