-- Follow-up (ADR-0082): when nobody acknowledges a critical incident, the
-- next step of the follow-up of its place is told, after a wait. The state
-- of each incident's follow-up lives here, so a restart keeps its timing.
-- "Escalated" keeps its meaning: an incident that got worse (ADR-0060).

-- The steps for everything, one customer or one site: at most one per
-- place, like a channel's coverage (ADR-0078). `steps` is an array of
-- {"after_minutes", "on_call": [schedule IDs], "users": [user IDs],
--  "channels": [channel IDs], "administrators": bool}, checked by the API;
-- IDs that no longer exist are skipped when a step is told. An empty array
-- tells nobody after the first message.
CREATE TABLE follow_up_policies (
    id          uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    customer_id uuid        REFERENCES customers (id) ON DELETE CASCADE,
    site_id     uuid        REFERENCES sites (id) ON DELETE CASCADE,
    steps       jsonb       NOT NULL CHECK (jsonb_typeof(steps) = 'array'
                                            AND jsonb_array_length(steps) <= 10),
    updated_by  uuid        REFERENCES users (id) ON DELETE SET NULL,
    updated_at  timestamptz NOT NULL DEFAULT now(),
    CHECK (customer_id IS NULL OR site_id IS NULL)
);
COMMENT ON TABLE follow_up_policies IS
    'Who is told next when nobody acknowledges a critical incident, per place. See ADR-0082.';
CREATE UNIQUE INDEX follow_up_policies_place_key ON follow_up_policies
    ((coalesce(customer_id, '00000000-0000-0000-0000-000000000000')),
     (coalesce(site_id, '00000000-0000-0000-0000-000000000000')));

-- Someone said "I'm on it". One per incident; the first one counts. The
-- name stays when the account is removed.
CREATE TABLE incident_acknowledgements (
    incident_id     uuid        PRIMARY KEY REFERENCES incidents (id) ON DELETE CASCADE,
    user_id         uuid        REFERENCES users (id) ON DELETE SET NULL,
    username        text        NOT NULL,
    acknowledged_at timestamptz NOT NULL DEFAULT now(),
    -- The incident's page or list, or the link in a personal message.
    via             text        NOT NULL CHECK (via IN ('page', 'link'))
);
COMMENT ON TABLE incident_acknowledgements IS
    'Who acknowledged an incident and when. See ADR-0082.';

-- An incident's follow-up: `step` steps told so far, the next one due at
-- `next_at`. Stopped once acknowledged, resolved, no longer critical, or
-- when every step was told.
CREATE TABLE incident_follow_ups (
    incident_id uuid        PRIMARY KEY REFERENCES incidents (id) ON DELETE CASCADE,
    -- When the first message about it was sent.
    started_at  timestamptz NOT NULL,
    step        integer     NOT NULL DEFAULT 0 CHECK (step >= 0),
    next_at     timestamptz,
    stopped_at  timestamptz,
    stop_reason text        CHECK (stop_reason IN ('acknowledged', 'resolved', 'not_critical',
                                                   'finished')),
    CHECK ((next_at IS NULL) = (stopped_at IS NOT NULL)),
    CHECK ((stopped_at IS NULL) = (stop_reason IS NULL))
);
COMMENT ON TABLE incident_follow_ups IS
    'Where each critical incident is in its follow-up, and when the next step is due. See ADR-0082.';
CREATE INDEX incident_follow_ups_due_idx ON incident_follow_ups (next_at)
    WHERE next_at IS NOT NULL;

-- Who was told of an incident in person, or a channel told by a follow-up
-- step, so that its resolution reaches them too.
CREATE TABLE incident_told (
    incident_id uuid        NOT NULL REFERENCES incidents (id) ON DELETE CASCADE,
    user_id     uuid        REFERENCES users (id) ON DELETE CASCADE,
    channel_id  uuid        REFERENCES notification_channels (id) ON DELETE CASCADE,
    told_at     timestamptz NOT NULL DEFAULT now(),
    CHECK (num_nonnulls(user_id, channel_id) = 1)
);
CREATE UNIQUE INDEX incident_told_user_key ON incident_told (incident_id, user_id)
    WHERE user_id IS NOT NULL;
CREATE UNIQUE INDEX incident_told_channel_key ON incident_told (incident_id, channel_id)
    WHERE channel_id IS NOT NULL;

-- The link in a personal message that acknowledges its incidents as that
-- person. Only the SHA-256 of the 256-bit secret is stored, as for
-- password links (ADR-0059). Used links stay until they expire, to say who
-- acknowledged.
CREATE TABLE acknowledge_links (
    token_hash   bytea       PRIMARY KEY CHECK (octet_length(token_hash) = 32),
    user_id      uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    incident_ids uuid[]      NOT NULL CHECK (cardinality(incident_ids) BETWEEN 1 AND 500),
    created_at   timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz NOT NULL,
    used_at      timestamptz,
    CHECK (expires_at > created_at)
);
COMMENT ON TABLE acknowledge_links IS
    'Hashes of the links that acknowledge incidents from a personal message. See ADR-0082.';
CREATE INDEX acknowledge_links_expires_idx ON acknowledge_links (expires_at);

-- The timeline says who acknowledged, and who a follow-up step told. Both
-- keep the incident's severity.
ALTER TABLE incident_events DROP CONSTRAINT incident_events_kind_check;
ALTER TABLE incident_events ADD CONSTRAINT incident_events_kind_check
    CHECK (kind IN ('opened', 'escalated', 'deescalated', 'resolved',
                    'silenced', 'silence_ended', 'acknowledged', 'followed_up'));
