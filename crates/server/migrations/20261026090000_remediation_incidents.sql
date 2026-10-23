-- Remediation incidents and their append-only, hash-chained history
-- (architecture §8, ADR-0042).
--
-- `incidents` and `incident_events` already belong to the threshold engine
-- (ADR-0017); a remediation incident is the AI loop's work on one of them.
-- Its row holds the current state; everything that happened is in
-- `remediation_incident_events`, numbered 1, 2, ... per incident, each row
-- carrying SHA-256(previous hash || canonical content). The triggers below
-- reject UPDATE, DELETE and TRUNCATE of events and DELETE of incidents.
-- As for audit_log, production must grant the application role only
-- INSERT and SELECT on the events table: a table owner or superuser can
-- disable triggers, which the hash chain then makes visible.

CREATE TABLE remediation_incidents (
    id                uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    -- The server's customer, checked at insert; one customer per AI request.
    customer_id       uuid        NOT NULL REFERENCES customers (id),
    -- No cascade: history outlives nothing it refers to.
    server_id         uuid        NOT NULL REFERENCES servers (id),
    -- The threshold incident that opened it, if any.
    alert_incident_id uuid        REFERENCES incidents (id),
    -- The series that opened it, kept even if the alert row goes.
    metric            text        NOT NULL CHECK (metric <> ''),
    instance          text        NOT NULL DEFAULT '',
    state             text        NOT NULL DEFAULT 'open'
                                  CHECK (state IN ('open', 'running', 'awaiting_approval',
                                                   'resolved', 'escalated', 'failed')),
    opened_at         timestamptz NOT NULL DEFAULT now(),
    closed_at         timestamptz,
    -- Limits in force when it opened; changing the configuration later
    -- does not change them.
    max_commands      integer     NOT NULL CHECK (max_commands > 0),
    max_duration_s    integer     NOT NULL CHECK (max_duration_s > 0),
    -- Sequence and hash of the last event, maintained by the events
    -- trigger. A missing tail of events shows as a mismatch with these.
    head_seq          bigint      NOT NULL DEFAULT 0 CHECK (head_seq >= 0),
    head_hash         bytea       CHECK (octet_length(head_hash) = 32),
    CHECK ((state IN ('resolved', 'escalated', 'failed')) = (closed_at IS NOT NULL)),
    CHECK (closed_at IS NULL OR closed_at >= opened_at),
    CHECK ((head_seq = 0) = (head_hash IS NULL))
);
COMMENT ON TABLE remediation_incidents IS 'AI remediation of an alert; history in remediation_incident_events. See ADR-0042.';
CREATE INDEX remediation_incidents_server_opened_idx ON remediation_incidents (server_id, opened_at);
CREATE INDEX remediation_incidents_opened_idx ON remediation_incidents (opened_at, id);
-- At most one active remediation per alert.
CREATE UNIQUE INDEX remediation_incidents_one_active_key ON remediation_incidents (alert_incident_id)
    WHERE state IN ('open', 'running', 'awaiting_approval');

CREATE TABLE remediation_incident_events (
    incident_id uuid        NOT NULL REFERENCES remediation_incidents (id),
    seq         bigint      NOT NULL CHECK (seq >= 1),
    kind        text        NOT NULL
                            CHECK (kind IN ('context_sent', 'ai_proposal', 'classification',
                                            'approval', 'rejection', 'edit', 'command_queued',
                                            'command_result', 'snapshot', 'escalation',
                                            'state_change', 'note')),
    actor_kind  text        NOT NULL CHECK (actor_kind IN ('user', 'system', 'ai', 'peer')),
    actor_id    text        NOT NULL CHECK (actor_id <> ''),
    payload     jsonb       NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    created_at  timestamptz NOT NULL DEFAULT now(),
    -- 32 zero bytes for the first event.
    prev_hash   bytea       NOT NULL CHECK (octet_length(prev_hash) = 32),
    -- Set by the trigger, whatever the INSERT says.
    hash        bytea       NOT NULL CHECK (octet_length(hash) = 32),
    PRIMARY KEY (incident_id, seq)
);
COMMENT ON TABLE remediation_incident_events IS 'Append-only, hash-chained history of remediation incidents. See ADR-0042.';

-- The bytes an event's hash covers, after the previous hash: a JSON array
-- whose text form PostgreSQL produces the same way for the same values.
-- The server's verifier hashes this text itself.
CREATE FUNCTION remediation_event_canonical(
    incident_id uuid, seq bigint, kind text, actor_kind text, actor_id text,
    payload jsonb, created_at timestamptz
) RETURNS text
LANGUAGE sql STABLE STRICT AS $$
    SELECT jsonb_build_array(
        1, incident_id, seq, kind, actor_kind, actor_id,
        to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),
        payload
    )::text
$$;
COMMENT ON FUNCTION remediation_event_canonical IS 'Canonical content of a remediation event (format 1), hashed after the previous hash.';

CREATE FUNCTION remediation_event_hash(e remediation_incident_events) RETURNS bytea
LANGUAGE sql STABLE STRICT AS $$
    SELECT sha256(e.prev_hash || convert_to(remediation_event_canonical(
        e.incident_id, e.seq, e.kind, e.actor_kind, e.actor_id, e.payload, e.created_at), 'UTF8'))
$$;

-- Chains a new event: it must follow the incident's head, which this
-- locks and moves.
CREATE FUNCTION remediation_incident_events_chain() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    last_seq  bigint;
    last_hash bytea;
BEGIN
    SELECT head_seq, head_hash INTO last_seq, last_hash
    FROM remediation_incidents WHERE id = NEW.incident_id FOR UPDATE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'remediation incident % does not exist', NEW.incident_id
            USING ERRCODE = 'foreign_key_violation';
    END IF;
    IF NEW.seq <> last_seq + 1
       OR NEW.prev_hash <> coalesce(last_hash, decode(repeat('00', 32), 'hex')) THEN
        RAISE EXCEPTION 'event % of incident % does not follow its head %',
            NEW.seq, NEW.incident_id, last_seq
            USING ERRCODE = 'serialization_failure';
    END IF;
    NEW.hash := remediation_event_hash(NEW);
    UPDATE remediation_incidents SET head_seq = NEW.seq, head_hash = NEW.hash
    WHERE id = NEW.incident_id;
    RETURN NEW;
END;
$$;

CREATE TRIGGER remediation_incident_events_chain
    BEFORE INSERT ON remediation_incident_events
    FOR EACH ROW EXECUTE FUNCTION remediation_incident_events_chain();

CREATE FUNCTION remediation_incident_events_append_only() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'remediation_incident_events is append-only: % is not allowed', TG_OP
        USING ERRCODE = 'insufficient_privilege';
END;
$$;

CREATE TRIGGER remediation_incident_events_no_update_delete
    BEFORE UPDATE OR DELETE ON remediation_incident_events
    FOR EACH ROW EXECUTE FUNCTION remediation_incident_events_append_only();

CREATE TRIGGER remediation_incident_events_no_truncate
    BEFORE TRUNCATE ON remediation_incident_events
    FOR EACH STATEMENT EXECUTE FUNCTION remediation_incident_events_append_only();

-- Incidents are never deleted. Only the state, closed_at and the head
-- change, the head one event at a time; the customer must be the server's.
CREATE FUNCTION remediation_incidents_guard() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NEW.customer_id IS DISTINCT FROM (
            SELECT si.customer_id FROM servers s JOIN sites si ON si.id = s.site_id
            WHERE s.id = NEW.server_id
        ) THEN
            RAISE EXCEPTION 'customer % does not own server %', NEW.customer_id, NEW.server_id
                USING ERRCODE = 'check_violation';
        END IF;
        IF NEW.alert_incident_id IS NOT NULL AND NEW.server_id IS DISTINCT FROM (
            SELECT server_id FROM incidents WHERE id = NEW.alert_incident_id
        ) THEN
            RAISE EXCEPTION 'alert % is not on server %', NEW.alert_incident_id, NEW.server_id
                USING ERRCODE = 'check_violation';
        END IF;
        IF NEW.head_seq <> 0 OR NEW.head_hash IS NOT NULL THEN
            RAISE EXCEPTION 'a new remediation incident has no events'
                USING ERRCODE = 'check_violation';
        END IF;
        RETURN NEW;
    END IF;
    IF TG_OP = 'UPDATE'
       AND (NEW.id, NEW.customer_id, NEW.server_id, NEW.alert_incident_id, NEW.metric,
            NEW.instance, NEW.opened_at, NEW.max_commands, NEW.max_duration_s)
           IS NOT DISTINCT FROM
           (OLD.id, OLD.customer_id, OLD.server_id, OLD.alert_incident_id, OLD.metric,
            OLD.instance, OLD.opened_at, OLD.max_commands, OLD.max_duration_s)
       AND (NEW.head_seq = OLD.head_seq AND NEW.head_hash IS NOT DISTINCT FROM OLD.head_hash
            OR NEW.head_seq = OLD.head_seq + 1) THEN
        RETURN NEW;
    END IF;
    RAISE EXCEPTION 'remediation_incidents: this % is not allowed', TG_OP
        USING ERRCODE = 'insufficient_privilege';
END;
$$;

CREATE TRIGGER remediation_incidents_guard
    BEFORE INSERT OR UPDATE OR DELETE ON remediation_incidents
    FOR EACH ROW EXECUTE FUNCTION remediation_incidents_guard();

CREATE TRIGGER remediation_incidents_no_truncate
    BEFORE TRUNCATE ON remediation_incidents
    FOR EACH STATEMENT EXECUTE FUNCTION remediation_incident_events_append_only();

-- The first event of `incident` whose chain is broken (a gap, a wrong
-- previous hash, a hash that does not match its content, or a head that
-- does not match the last event), or NULL when the chain is intact. The
-- server's verifier does the same in Rust; this one is for operators.
CREATE FUNCTION remediation_verify_chain(incident uuid) RETURNS bigint
LANGUAGE plpgsql STABLE AS $$
DECLARE
    e        remediation_incident_events;
    expected bigint := 1;
    prev     bytea := decode(repeat('00', 32), 'hex');
    head     remediation_incidents;
BEGIN
    SELECT * INTO head FROM remediation_incidents WHERE id = incident;
    IF NOT FOUND THEN
        RETURN NULL;
    END IF;
    FOR e IN SELECT * FROM remediation_incident_events
             WHERE incident_id = incident ORDER BY seq LOOP
        IF e.seq <> expected OR e.prev_hash <> prev OR e.hash <> remediation_event_hash(e) THEN
            RETURN expected;
        END IF;
        prev := e.hash;
        expected := expected + 1;
    END LOOP;
    IF head.head_seq <> expected - 1
       OR head.head_hash IS DISTINCT FROM (CASE WHEN expected = 1 THEN NULL ELSE prev END) THEN
        RETURN expected;
    END IF;
    RETURN NULL;
END;
$$;
COMMENT ON FUNCTION remediation_verify_chain IS 'First broken event sequence of a remediation incident, NULL if its chain is intact.';
