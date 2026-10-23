-- The remediation loop (architecture §8, ADR-0043): which server process
-- drives each incident, and the proposed commands waiting for approval.
-- Neither table is history: what happened is in remediation_incident_events.

-- One row per incident a server process is driving. The holder renews
-- expires_at while it works; an expired lease may be taken over, which is
-- how incidents left running by a stopped server resume, and how two
-- processes never drive one incident at once.
CREATE TABLE remediation_loop_leases (
    incident_id uuid        PRIMARY KEY REFERENCES remediation_incidents (id),
    -- Random per server process.
    holder      uuid        NOT NULL,
    expires_at  timestamptz NOT NULL
);
COMMENT ON TABLE remediation_loop_leases IS 'Which server process drives a remediation incident; see ADR-0043.';

-- A command the risk rules say needs approval, with what an approver
-- needs to decide. The approval queue sets the decision; the incident's
-- events record it.
CREATE TABLE remediation_pending_commands (
    id           uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    incident_id  uuid        NOT NULL REFERENCES remediation_incidents (id),
    -- The ai_proposal event of the command.
    proposal_seq bigint      NOT NULL,
    command      text        NOT NULL CHECK (command <> '' AND octet_length(command) <= 4096),
    -- The AI's text, untrusted.
    diagnosis    text        NOT NULL,
    reasoning    text        NOT NULL,
    tier         text        NOT NULL CHECK (tier IN ('low_impact', 'high_impact')),
    -- The rule that decided, {"source": ..., "id": ...}, and why.
    rule         jsonb       NOT NULL CHECK (jsonb_typeof(rule) = 'object'),
    reason       text        NOT NULL,
    -- expired: the incident hit its duration limit first; cancelled: the
    -- incident closed otherwise, like its alert resolving.
    state        text        NOT NULL DEFAULT 'pending'
                             CHECK (state IN ('pending', 'approved', 'rejected', 'expired',
                                              'cancelled')),
    created_at   timestamptz NOT NULL DEFAULT now(),
    decided_at   timestamptz,
    decided_by   uuid        REFERENCES users (id),
    CHECK ((state = 'pending') = (decided_at IS NULL)),
    FOREIGN KEY (incident_id, proposal_seq)
        REFERENCES remediation_incident_events (incident_id, seq)
);
COMMENT ON TABLE remediation_pending_commands IS 'Proposed commands waiting for approval; see ADR-0043.';
CREATE UNIQUE INDEX remediation_pending_commands_one_key ON remediation_pending_commands (incident_id)
    WHERE state = 'pending';

-- The loop opens one remediation per alert, ever: it looks them up by alert.
CREATE INDEX remediation_incidents_alert_idx ON remediation_incidents (alert_incident_id)
    WHERE alert_incident_id IS NOT NULL;
-- And looks for incidents to drive or to sweep by state.
CREATE INDEX remediation_incidents_active_idx ON remediation_incidents (state)
    WHERE state IN ('open', 'running', 'awaiting_approval');
