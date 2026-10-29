-- Notifications (ADR-0058): email and webhook channels set up in the web
-- interface, and an outbox of events the server delivers to them.
--
-- Triggers fill the outbox from the tables where things happen, so every
-- path that opens an incident, parks a command for approval or fails an
-- update is covered without each one calling a notifier.

-- A destination. `settings` holds what the interface shows back; the
-- SMTP password or webhook signing secret is in `secret`, which the API
-- never returns.
CREATE TABLE notification_channels (
    id          uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    kind        text        NOT NULL CHECK (kind IN ('email', 'webhook')),
    -- email: {"host", "port", "username", "from", "to": [...]};
    -- webhook: {"url"}.
    settings    jsonb       NOT NULL CHECK (jsonb_typeof(settings) = 'object'),
    secret      text        CHECK (secret <> ''),
    -- Only this customer's events; NULL for every customer and the
    -- server-wide ones.
    customer_id uuid        REFERENCES customers (id) ON DELETE CASCADE,
    created_at  timestamptz NOT NULL DEFAULT now(),
    -- The last delivery or test, and its error in plain words.
    last_sent_at  timestamptz,
    last_error    text,
    last_error_at timestamptz
);
COMMENT ON TABLE notification_channels IS
    'Email and webhook destinations of notifications; secret never leaves the server. See ADR-0058.';

-- What to tell, written by the triggers below. The dispatcher claims rows
-- by setting `sent_at`, delivers them to every matching channel, and
-- deletes rows a week after.
CREATE TABLE notification_outbox (
    id          bigserial   PRIMARY KEY,
    event       text        NOT NULL CHECK (event IN ('incident.opened', 'incident.escalated',
                                                      'incident.resolved', 'approval.waiting',
                                                      'update.failed')),
    -- The customer it concerns, for channels scoped to one; NULL when
    -- server-wide, like a halted automatic rollout.
    customer_id uuid,
    -- The objects of the event, as the webhook payload carries them.
    details     jsonb       NOT NULL CHECK (jsonb_typeof(details) = 'object'),
    created_at  timestamptz NOT NULL DEFAULT now(),
    sent_at     timestamptz
);
COMMENT ON TABLE notification_outbox IS
    'Events waiting for, or recently given to, notification channels. See ADR-0058.';
CREATE INDEX notification_outbox_pending_idx ON notification_outbox (id) WHERE sent_at IS NULL;

-- `{"id", "name"}` of a server, relay, site or customer, or NULL.
CREATE FUNCTION notification_ref(id uuid, name text) RETURNS jsonb
LANGUAGE sql IMMUTABLE AS $$
    SELECT CASE WHEN id IS NULL THEN NULL ELSE jsonb_build_object('id', id, 'name', name) END
$$;

-- Where `server` or `relay` lives: the site and customer objects and the
-- customer ID. Either may be NULL.
CREATE FUNCTION notification_place(server uuid, relay uuid,
                                   OUT site jsonb, OUT customer jsonb, OUT customer_id uuid)
LANGUAGE sql STABLE AS $$
    SELECT notification_ref(si.id, si.name), notification_ref(c.id, c.name), c.id
    FROM sites si JOIN customers c ON c.id = si.customer_id
    WHERE si.id = coalesce((SELECT site_id FROM servers WHERE id = server),
                           (SELECT site_id FROM relays WHERE id = relay))
$$;

-- Incidents: opened, escalated to critical, resolved. Deescalations are
-- not worth waking anyone. The relay is read from the row as JSON, so the
-- trigger works whether or not incidents carry one.
CREATE FUNCTION notification_incident_event() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    i       incidents;
    relay   uuid;
    place   record;
BEGIN
    IF NEW.kind NOT IN ('opened', 'escalated', 'resolved') THEN
        RETURN NULL;
    END IF;
    SELECT * INTO i FROM incidents WHERE id = NEW.incident_id;
    relay := (to_jsonb(i) ->> 'relay_id')::uuid;
    SELECT * INTO place FROM notification_place(i.server_id, relay);
    INSERT INTO notification_outbox (event, customer_id, details)
    VALUES ('incident.' || NEW.kind, place.customer_id, jsonb_build_object(
        'occurred_at', NEW.occurred_at,
        'customer', place.customer,
        'site', place.site,
        'server', (SELECT notification_ref(id, hostname) FROM servers WHERE id = i.server_id),
        'relay', (SELECT notification_ref(id, name) FROM relays WHERE id = relay),
        'incident', jsonb_build_object(
            'id', i.id, 'metric', i.metric, 'instance', i.instance,
            -- The incident's severity when resolved, the new one otherwise.
            'severity', coalesce(NEW.severity, i.severity),
            'value', NEW.value, 'peak_value', i.peak_value,
            'opened_at', i.opened_at, 'resolved_at', i.resolved_at)));
    RETURN NULL;
END;
$$;

CREATE TRIGGER notification_incident_event
    AFTER INSERT ON incident_events
    FOR EACH ROW EXECUTE FUNCTION notification_incident_event();

-- A command parked for approval (ADR-0043).
CREATE FUNCTION notification_approval() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    r     remediation_incidents;
    place record;
BEGIN
    SELECT * INTO r FROM remediation_incidents WHERE id = NEW.incident_id;
    SELECT * INTO place FROM notification_place(r.server_id, NULL);
    INSERT INTO notification_outbox (event, customer_id, details)
    VALUES ('approval.waiting', r.customer_id, jsonb_build_object(
        'occurred_at', NEW.created_at,
        'customer', place.customer,
        'site', place.site,
        'server', (SELECT notification_ref(id, hostname) FROM servers WHERE id = r.server_id),
        'approval', jsonb_build_object(
            'id', NEW.id, 'remediation_incident_id', r.id,
            'incident_id', r.alert_incident_id, 'metric', r.metric,
            'command', NEW.command, 'tier', NEW.tier)));
    RETURN NULL;
END;
$$;

CREATE TRIGGER notification_approval
    AFTER INSERT ON remediation_pending_commands
    FOR EACH ROW EXECUTE FUNCTION notification_approval();

-- Updates of a server or relay that failed or expired, outside campaigns:
-- a campaign reports once, when it fails, not per server.
CREATE FUNCTION notification_update_failed() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    server  uuid;
    relay   uuid;
    version text;
    place   record;
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
    SELECT * INTO place FROM notification_place(server, relay);
    INSERT INTO notification_outbox (event, customer_id, details)
    VALUES ('update.failed', place.customer_id, jsonb_build_object(
        'occurred_at', NEW.finished_at,
        'customer', place.customer,
        'site', place.site,
        'server', (SELECT notification_ref(id, hostname) FROM servers WHERE id = server),
        'relay', (SELECT notification_ref(id, name) FROM relays WHERE id = relay),
        'update', jsonb_build_object(
            'kind', NEW.kind, 'command_id', NEW.id, 'version', version,
            'state', NEW.state, 'error', NEW.error)));
    RETURN NULL;
END;
$$;

CREATE TRIGGER notification_update_failed
    AFTER UPDATE OF state ON commands
    FOR EACH ROW WHEN (NEW.state IN ('failed', 'expired') AND OLD.state IS DISTINCT FROM NEW.state)
    EXECUTE FUNCTION notification_update_failed();

CREATE TRIGGER notification_update_failed
    AFTER UPDATE OF state ON relay_commands
    FOR EACH ROW WHEN (NEW.state IN ('failed', 'expired') AND OLD.state IS DISTINCT FROM NEW.state)
    EXECUTE FUNCTION notification_update_failed();

-- Campaigns that failed, and automatic rollouts that halted.
CREATE FUNCTION notification_rollout_failed() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_TABLE_NAME = 'campaigns' THEN
        INSERT INTO notification_outbox (event, customer_id, details)
        VALUES ('update.failed', NEW.customer_id, jsonb_build_object(
            'occurred_at', NEW.finished_at,
            'customer', (SELECT notification_ref(id, name) FROM customers WHERE id = NEW.customer_id),
            'site', (SELECT notification_ref(id, name) FROM sites WHERE id = NEW.site_id),
            'update', jsonb_build_object(
                'kind', 'campaign', 'campaign_id', NEW.id, 'name', NEW.name,
                'state', NEW.state, 'error', NEW.error)));
    ELSE
        INSERT INTO notification_outbox (event, customer_id, details)
        VALUES ('update.failed', NULL, jsonb_build_object(
            'occurred_at', NEW.halted_at,
            'update', jsonb_build_object(
                'kind', 'automatic_rollout', 'peers', NEW.kind, 'version', NEW.version,
                'state', NEW.state, 'error', NEW.reason)));
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER notification_campaign_failed
    AFTER UPDATE OF state ON campaigns
    FOR EACH ROW WHEN (NEW.state = 'failed' AND OLD.state IS DISTINCT FROM NEW.state)
    EXECUTE FUNCTION notification_rollout_failed();

CREATE TRIGGER notification_rollout_halted
    AFTER UPDATE OF state ON auto_rollouts
    FOR EACH ROW WHEN (NEW.state = 'halted' AND OLD.state IS DISTINCT FROM NEW.state)
    EXECUTE FUNCTION notification_rollout_failed();

-- A rollout can be halted by the INSERT that records it.
CREATE TRIGGER notification_rollout_halted_at_start
    AFTER INSERT ON auto_rollouts
    FOR EACH ROW WHEN (NEW.state = 'halted')
    EXECUTE FUNCTION notification_rollout_failed();
