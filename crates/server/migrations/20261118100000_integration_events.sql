-- Outgoing events for integrations (ADR-0102): signed webhooks for
-- machines, delivered at least once, retried for a day and replayable for
-- a month.
--
-- Triggers on the tables where things happen write `integration_events`,
-- reusing the notification helpers so the place of an event is computed
-- once. A trigger on that table writes one delivery per endpoint that
-- wants it, in the same transaction.

-- Where events go. The signing secret is made by the server, shown once,
-- and never returned by the API.
CREATE TABLE integration_endpoints (
    id              uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    name            text        NOT NULL CHECK (name <> '' AND char_length(name) <= 80),
    url             text        NOT NULL CHECK (url ~ '^https?://'),
    secret          text        NOT NULL CHECK (secret <> ''),
    -- The events it wants; see integration_events.event.
    events          text[]      NOT NULL CHECK (cardinality(events) > 0),
    -- What it covers, like a notification channel (ADR-0078): both NULL
    -- for everything.
    customer_id     uuid        REFERENCES customers (id) ON DELETE CASCADE,
    site_id         uuid        REFERENCES sites (id) ON DELETE CASCADE,
    -- Set by a global administrator only: deliver to private addresses.
    allow_private   boolean     NOT NULL DEFAULT false,
    -- on: gets events; paused: by an administrator; disabled: after a day
    -- of failures.
    state           text        NOT NULL DEFAULT 'on' CHECK (state IN ('on', 'paused', 'disabled')),
    created_at      timestamptz NOT NULL DEFAULT now(),
    -- The first failure since the last success; NULL while it works.
    failing_since   timestamptz,
    last_success_at timestamptz,
    CHECK (customer_id IS NULL OR site_id IS NULL)
);
COMMENT ON TABLE integration_endpoints IS
    'Webhook endpoints of integrations; secret never leaves the server. See ADR-0102.';

-- What happened, for integrations. Kept 30 days for replays.
CREATE TABLE integration_events (
    seq          bigserial   PRIMARY KEY,
    -- What receivers de-duplicate on: the same for every endpoint, retry
    -- and replay.
    id           uuid        NOT NULL UNIQUE DEFAULT gen_random_uuid(),
    event        text        NOT NULL CHECK (event IN (
                     'incident.opened', 'incident.escalated', 'incident.acknowledged',
                     'incident.resolved', 'server.added', 'server.removed', 'server.offline',
                     'server.online', 'update.started', 'update.succeeded', 'update.failed',
                     'campaign.started', 'campaign.succeeded', 'campaign.failed',
                     'approval.waiting', 'approval.decided')),
    -- The customer it concerns; NULL when none.
    customer_id  uuid,
    -- The site named in the payload, like notification_outbox.site_id.
    site_id      uuid        GENERATED ALWAYS AS ((details #>> '{site,id}')::uuid) STORED,
    -- The subject, like `incident:<id>`: deliveries about one subject to
    -- one endpoint go in order.
    subject      text        NOT NULL,
    -- The payload's fields, without version, id, event and url.
    details      jsonb       NOT NULL CHECK (jsonb_typeof(details) = 'object'),
    created_at   timestamptz NOT NULL DEFAULT now()
);
COMMENT ON TABLE integration_events IS
    'Events for integration endpoints, kept 30 days for replays. See ADR-0102.';
CREATE INDEX integration_events_created_idx ON integration_events (created_at);

-- One event to one endpoint. Pending ones are claimed by moving `next_at`
-- ahead, so a crashed instance's claim runs out and another retries.
CREATE TABLE integration_deliveries (
    id              bigserial   PRIMARY KEY,
    endpoint_id     uuid        NOT NULL REFERENCES integration_endpoints (id) ON DELETE CASCADE,
    event_seq       bigint      NOT NULL REFERENCES integration_events (seq) ON DELETE CASCADE,
    -- Copied from the event, for the ordering check.
    subject         text        NOT NULL,
    state           text        NOT NULL DEFAULT 'pending'
                                CHECK (state IN ('pending', 'delivered', 'failed')),
    replay          boolean     NOT NULL DEFAULT false,
    attempts        integer     NOT NULL DEFAULT 0,
    next_at         timestamptz NOT NULL DEFAULT now(),
    created_at      timestamptz NOT NULL DEFAULT now(),
    last_attempt_at timestamptz,
    -- The last answer's HTTP status; NULL without an answer.
    last_status     integer,
    -- The last failure in plain words; NULL once delivered.
    last_error      text,
    finished_at     timestamptz,
    CHECK ((state = 'pending') = (finished_at IS NULL))
);
COMMENT ON TABLE integration_deliveries IS
    'Deliveries of integration events to endpoints, and their outcome. See ADR-0102.';
CREATE INDEX integration_deliveries_pending_idx ON integration_deliveries (next_at)
    WHERE state = 'pending';
CREATE INDEX integration_deliveries_subject_idx ON integration_deliveries (endpoint_id, subject, id)
    WHERE state = 'pending';
CREATE INDEX integration_deliveries_endpoint_idx ON integration_deliveries (endpoint_id, id);
CREATE INDEX integration_deliveries_event_idx ON integration_deliveries (event_seq);

-- Writes an event; the place comes from the server or relay.
CREATE FUNCTION integration_emit(event text, subject text, server uuid, relay uuid,
                                 details jsonb) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    place record;
BEGIN
    SELECT * INTO place FROM notification_place(server, relay);
    INSERT INTO integration_events (event, customer_id, subject, details)
    VALUES (event, place.customer_id, subject, jsonb_build_object(
        'customer', place.customer,
        'site', place.site,
        'server', (SELECT notification_ref(id, hostname) FROM servers WHERE id = server),
        'relay', (SELECT notification_ref(id, name) FROM relays WHERE id = relay)) || details);
END;
$$;

-- One delivery per endpoint that is on, wants the event and covers its
-- place: everything takes every event, a customer its events, a site its
-- events (ADR-0078).
CREATE FUNCTION integration_fan_out() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO integration_deliveries (endpoint_id, event_seq, subject)
    SELECT e.id, NEW.seq, NEW.subject FROM integration_endpoints e
    WHERE e.state = 'on' AND NEW.event = ANY (e.events)
      AND ((e.customer_id IS NULL AND e.site_id IS NULL)
           OR e.customer_id = NEW.customer_id OR e.site_id = NEW.site_id);
    RETURN NULL;
END;
$$;

CREATE TRIGGER integration_fan_out
    AFTER INSERT ON integration_events
    FOR EACH ROW EXECUTE FUNCTION integration_fan_out();

-- Incidents opened, escalated, acknowledged or resolved; an offline
-- incident of a server is also the server going offline or coming back.
CREATE FUNCTION integration_incident_event() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    i incidents;
BEGIN
    IF NEW.kind NOT IN ('opened', 'escalated', 'acknowledged', 'resolved') THEN
        RETURN NULL;
    END IF;
    SELECT * INTO i FROM incidents WHERE id = NEW.incident_id;
    PERFORM integration_emit('incident.' || NEW.kind, 'incident:' || i.id, i.server_id, i.relay_id,
        jsonb_build_object(
            'occurred_at', NEW.occurred_at,
            'incident', jsonb_build_object(
                'id', i.id, 'metric', i.metric, 'instance', i.instance,
                'severity', coalesce(NEW.severity, i.severity),
                'value', NEW.value, 'peak_value', i.peak_value,
                'reason', NEW.details ->> 'reason',
                'acknowledged_by', CASE WHEN NEW.kind = 'acknowledged' THEN
                    jsonb_build_object('id', NEW.details ->> 'user_id',
                                       'name', NEW.details ->> 'username') END,
                'opened_at', i.opened_at, 'resolved_at', i.resolved_at)));
    IF i.metric = 'offline' AND i.server_id IS NOT NULL
       AND (NEW.kind = 'opened'
            OR (NEW.kind = 'resolved' AND NEW.details ->> 'reason' = 'back_online')) THEN
        PERFORM integration_emit(
            CASE NEW.kind WHEN 'opened' THEN 'server.offline' ELSE 'server.online' END,
            'server:' || i.server_id, i.server_id, NULL,
            jsonb_build_object('occurred_at', NEW.occurred_at,
                               'incident', jsonb_build_object('id', i.id)));
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER integration_incident_event
    AFTER INSERT ON incident_events
    FOR EACH ROW EXECUTE FUNCTION integration_incident_event();

-- Servers enrolled, removed, or back after a removal (ADR-0062).
CREATE FUNCTION integration_server() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    PERFORM integration_emit(
        CASE WHEN NEW.revoked_at IS NULL THEN 'server.added' ELSE 'server.removed' END,
        'server:' || NEW.id, NEW.id, NULL,
        jsonb_build_object(
            'occurred_at', coalesce(NEW.revoked_at, now()),
            'restored', TG_OP = 'UPDATE' AND NEW.revoked_at IS NULL));
    RETURN NULL;
END;
$$;

CREATE TRIGGER integration_server_added
    AFTER INSERT ON servers
    FOR EACH ROW WHEN (NEW.revoked_at IS NULL)
    EXECUTE FUNCTION integration_server();

CREATE TRIGGER integration_server_changed
    AFTER UPDATE OF revoked_at ON servers
    FOR EACH ROW WHEN ((OLD.revoked_at IS NULL) <> (NEW.revoked_at IS NULL))
    EXECUTE FUNCTION integration_server();

-- Updates of a server or relay outside campaigns: started, then succeeded
-- or failed (any end that is not a success, `state` says which).
CREATE FUNCTION integration_update() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    server  uuid;
    relay   uuid;
    version text;
BEGIN
    IF TG_TABLE_NAME = 'commands' THEN
        IF NEW.kind NOT IN ('apply_updates', 'self_update') OR NEW.campaign_id IS NOT NULL THEN
            RETURN NULL;
        END IF;
        server := NEW.server_id;
    ELSE
        relay := NEW.relay_id;
        version := NEW.version;
    END IF;
    PERFORM integration_emit(
        CASE NEW.state WHEN 'running' THEN 'update.started'
                       WHEN 'succeeded' THEN 'update.succeeded'
                       ELSE 'update.failed' END,
        'update:' || NEW.id, server, relay,
        jsonb_build_object(
            'occurred_at', coalesce(NEW.finished_at, NEW.started_at, now()),
            'update', jsonb_build_object(
                'kind', NEW.kind, 'command_id', NEW.id, 'version', version,
                'state', NEW.state, 'error', NEW.error)));
    RETURN NULL;
END;
$$;

CREATE TRIGGER integration_update
    AFTER UPDATE OF state ON commands
    FOR EACH ROW WHEN (OLD.state IS DISTINCT FROM NEW.state
                       AND NEW.state IN ('running', 'succeeded', 'failed', 'expired', 'cancelled'))
    EXECUTE FUNCTION integration_update();

CREATE TRIGGER integration_update
    AFTER UPDATE OF state ON relay_commands
    FOR EACH ROW WHEN (OLD.state IS DISTINCT FROM NEW.state
                       AND NEW.state IN ('running', 'succeeded', 'failed', 'expired', 'cancelled'))
    EXECUTE FUNCTION integration_update();

-- Campaigns started, completed, or ended any other way.
CREATE FUNCTION integration_campaign() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.state = 'running' AND OLD.state NOT IN ('draft', 'scheduled') THEN
        RETURN NULL;
    END IF;
    INSERT INTO integration_events (event, customer_id, subject, details)
    VALUES (CASE NEW.state WHEN 'running' THEN 'campaign.started'
                           WHEN 'completed' THEN 'campaign.succeeded'
                           ELSE 'campaign.failed' END,
            NEW.customer_id, 'campaign:' || NEW.id, jsonb_build_object(
        'occurred_at', coalesce(NEW.finished_at, NEW.started_at, now()),
        'customer', (SELECT notification_ref(id, name) FROM customers WHERE id = NEW.customer_id),
        'site', (SELECT notification_ref(id, name) FROM sites WHERE id = NEW.site_id),
        'server', NULL, 'relay', NULL,
        'campaign', jsonb_build_object(
            'id', NEW.id, 'name', NEW.name, 'state', NEW.state, 'error', NEW.error,
            'started_at', NEW.started_at, 'finished_at', NEW.finished_at)));
    RETURN NULL;
END;
$$;

CREATE TRIGGER integration_campaign
    AFTER UPDATE OF state ON campaigns
    FOR EACH ROW WHEN (OLD.state IS DISTINCT FROM NEW.state
                       AND NEW.state IN ('running', 'completed', 'failed', 'stopped'))
    EXECUTE FUNCTION integration_campaign();

-- Commands the AI proposed: waiting for approval, then decided.
CREATE FUNCTION integration_approval() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    r remediation_incidents;
BEGIN
    SELECT * INTO r FROM remediation_incidents WHERE id = NEW.incident_id;
    PERFORM integration_emit(
        CASE WHEN NEW.state = 'pending' THEN 'approval.waiting' ELSE 'approval.decided' END,
        'approval:' || NEW.id, r.server_id, NULL,
        jsonb_build_object(
            'occurred_at', coalesce(NEW.decided_at, NEW.created_at),
            'approval', jsonb_build_object(
                'id', NEW.id, 'remediation_incident_id', r.id,
                'incident_id', r.alert_incident_id, 'metric', r.metric,
                'command', NEW.command, 'tier', NEW.tier, 'state', NEW.state,
                'decided_at', NEW.decided_at,
                'decided_by', (SELECT notification_ref(id, username) FROM users
                               WHERE id = NEW.decided_by))));
    RETURN NULL;
END;
$$;

CREATE TRIGGER integration_approval_waiting
    AFTER INSERT ON remediation_pending_commands
    FOR EACH ROW EXECUTE FUNCTION integration_approval();

CREATE TRIGGER integration_approval_decided
    AFTER UPDATE OF state ON remediation_pending_commands
    FOR EACH ROW WHEN (OLD.state = 'pending' AND NEW.state <> 'pending')
    EXECUTE FUNCTION integration_approval();

-- The notice to administrators when an endpoint is turned off after a day
-- of failures.
ALTER TABLE notification_outbox DROP CONSTRAINT notification_outbox_event_check;
ALTER TABLE notification_outbox ADD CONSTRAINT notification_outbox_event_check
    CHECK (event IN ('incident.opened', 'incident.escalated', 'incident.resolved',
                     'approval.waiting', 'update.failed', 'backup.failed',
                     'update.available', 'ai_commands.changed', 'integration.disabled'));
