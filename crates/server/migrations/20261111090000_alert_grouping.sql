-- Alert grouping (ADR-0080): the dispatcher claims a customer's events
-- together, at most once a minute, and sends related incidents as one
-- message. Each group sent is recorded so that its link lists its
-- incidents in the interface. Incidents stay one per server or relay.

CREATE TABLE notification_groups (
    id           uuid        PRIMARY KEY,
    -- A group never mixes customers.
    customer_id  uuid        NOT NULL REFERENCES customers (id) ON DELETE CASCADE,
    incident_ids uuid[]      NOT NULL CHECK (cardinality(incident_ids) >= 1),
    created_at   timestamptz NOT NULL DEFAULT now()
);
COMMENT ON TABLE notification_groups IS
    'Incidents told together in one notification, for the list its link opens. See ADR-0080.';
CREATE INDEX notification_groups_incidents_idx ON notification_groups USING gin (incident_ids);

-- The dispatcher looks for a customer's last claim within the window at
-- each pass; pruning reads the same column.
CREATE INDEX notification_outbox_sent_idx ON notification_outbox (sent_at);
