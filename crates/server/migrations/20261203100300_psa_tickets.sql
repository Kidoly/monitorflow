-- Tickets opened for incidents through the PSA connection (ADR-0164,
-- ADR-0166). The sync reads `integration_events` as one more consumer,
-- with its own delivery state, so it inherits at-least-once delivery,
-- retries and the order per incident.

-- One ticket per incident at most. `creating` is written before the
-- request, so a retry after a lost answer looks for the ticket before
-- creating another.
CREATE TABLE psa_tickets (
    incident_id   uuid        PRIMARY KEY REFERENCES incidents (id) ON DELETE CASCADE,
    connection_id uuid        NOT NULL REFERENCES psa_connections (id) ON DELETE CASCADE,
    -- The PSA's ticket id, once known.
    ticket_id     bigint      CHECK (ticket_id > 0),
    -- creating: requested; open: created; closed: its incident resolved;
    -- skipped: no ticket (site not matched); failed: the PSA refused.
    state         text        NOT NULL CHECK (state IN ('creating', 'open', 'closed', 'skipped', 'failed')),
    -- Why it was skipped or failed, or the last failure while retrying.
    error         text,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    closed_at     timestamptz,
    CHECK ((state IN ('open', 'closed')) <= (ticket_id IS NOT NULL))
);
CREATE INDEX psa_tickets_recent_idx ON psa_tickets (updated_at DESC);
COMMENT ON TABLE psa_tickets IS
    'PSA tickets opened for incidents (ADR-0164, ADR-0166).';

-- One incident event for the sync, like integration_deliveries for an
-- endpoint.
CREATE TABLE psa_deliveries (
    id          bigserial   PRIMARY KEY,
    event_seq   bigint      NOT NULL REFERENCES integration_events (seq) ON DELETE CASCADE,
    -- Copied from the event, for the order per incident.
    subject     text        NOT NULL,
    state       text        NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'done', 'failed')),
    attempts    integer     NOT NULL DEFAULT 0,
    next_at     timestamptz NOT NULL DEFAULT now(),
    last_error  text,
    created_at  timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz,
    CHECK ((state = 'pending') = (finished_at IS NULL))
);
CREATE INDEX psa_deliveries_pending_idx ON psa_deliveries (next_at) WHERE state = 'pending';
CREATE INDEX psa_deliveries_subject_idx ON psa_deliveries (subject, id) WHERE state = 'pending';
CREATE INDEX psa_deliveries_event_idx ON psa_deliveries (event_seq);
COMMENT ON TABLE psa_deliveries IS
    'Incident events for the PSA sync, and their outcome (ADR-0164).';

-- Incident events go to the sync while a connection is on.
CREATE FUNCTION psa_fan_out() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.event IN ('incident.opened', 'incident.escalated', 'incident.resolved')
       AND EXISTS (SELECT 1 FROM psa_connections WHERE enabled) THEN
        INSERT INTO psa_deliveries (event_seq, subject) VALUES (NEW.seq, NEW.subject);
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER psa_fan_out
    AFTER INSERT ON integration_events
    FOR EACH ROW EXECUTE FUNCTION psa_fan_out();
