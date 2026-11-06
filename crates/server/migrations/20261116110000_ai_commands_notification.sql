-- AI commands turned on or off from the web interface (ADR-0098): channels
-- covering the server hear of each request.
ALTER TABLE notification_outbox DROP CONSTRAINT notification_outbox_event_check;
ALTER TABLE notification_outbox ADD CONSTRAINT notification_outbox_event_check
    CHECK (event IN ('incident.opened', 'incident.escalated', 'incident.resolved',
                     'approval.waiting', 'update.failed', 'backup.failed',
                     'update.available', 'ai_commands.changed'));
