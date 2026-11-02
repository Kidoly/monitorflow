-- Silences (ADR-0079): notifications about a server, relay, site, customer
-- or everything stop for a while. Incidents go on as usual; the dispatcher
-- holds their outbox rows back and decides what to say when the silence is
-- over.

CREATE TABLE silences (
    id          uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    -- The scope: at most one of these; none is everything.
    server_id   uuid        REFERENCES servers (id) ON DELETE CASCADE,
    relay_id    uuid        REFERENCES relays (id) ON DELETE CASCADE,
    site_id     uuid        REFERENCES sites (id) ON DELETE CASCADE,
    customer_id uuid        REFERENCES customers (id) ON DELETE CASCADE,
    starts_at   timestamptz NOT NULL,
    -- Every silence ends on its own: there is no permanent one.
    ends_at     timestamptz NOT NULL,
    reason      text        NOT NULL CHECK (reason <> '' AND char_length(reason) <= 200),
    created_by  uuid        REFERENCES users (id) ON DELETE SET NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    -- Ended early, or called off before it started; `ends_at` stays what
    -- was planned.
    ended_at    timestamptz,
    ended_by    uuid        REFERENCES users (id) ON DELETE SET NULL,
    CHECK (num_nonnulls(server_id, relay_id, site_id, customer_id) <= 1),
    CHECK (ends_at > starts_at),
    CHECK (ends_at <= starts_at + interval '30 days')
);
COMMENT ON TABLE silences IS
    'Periods without notifications for a scope; incidents are not affected. See ADR-0079.';
CREATE INDEX silences_end_idx ON silences ((coalesce(ended_at, ends_at)));

-- Set while a silence keeps a row from being delivered. `sent_at` stays
-- NULL, so a held row is neither pending nor delivered, and never pruned.
ALTER TABLE notification_outbox ADD COLUMN held_at timestamptz;
CREATE INDEX notification_outbox_held_idx ON notification_outbox (id)
    WHERE held_at IS NOT NULL AND sent_at IS NULL;

-- The timeline of an incident says what a silence did to it. Both keep the
-- incident's severity, so `severity` stays NULL only once resolved.
ALTER TABLE incident_events DROP CONSTRAINT incident_events_kind_check;
ALTER TABLE incident_events ADD CONSTRAINT incident_events_kind_check
    CHECK (kind IN ('opened', 'escalated', 'deescalated', 'resolved',
                    'silenced', 'silence_ended'));

-- When the longest silence now covering a place ends, or NULL. Any of the
-- arguments may be NULL.
CREATE FUNCTION silenced_until(server uuid, relay uuid, site uuid, customer uuid)
RETURNS timestamptz
LANGUAGE sql STABLE AS $$
    SELECT max(ends_at) FROM silences
    WHERE starts_at <= now() AND coalesce(ended_at, ends_at) > now()
      AND (num_nonnulls(server_id, relay_id, site_id, customer_id) = 0
           OR server_id = server OR relay_id = relay
           OR site_id = site OR customer_id = customer)
$$;
