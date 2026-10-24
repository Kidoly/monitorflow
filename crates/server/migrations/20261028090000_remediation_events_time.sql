-- The remediation audit log reads events of many incidents newest first
-- (ADR-0045); the primary key only serves one incident at a time.
CREATE INDEX remediation_incident_events_created_idx
    ON remediation_incident_events (created_at, incident_id, seq);
