-- On call (ADR-0081): who is on duty gets a personal message, through the
-- contact each user keeps, besides the channels of what they look after.

-- How to reach one person. Each user changes their own. The email goes out
-- through the mail server of an email channel; the optional chat or webhook
-- destination is stored like a channel's, its secret (a chat webhook URL or
-- a signing secret) never read back.
CREATE TABLE user_contacts (
    user_id       uuid        PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    email         text        CHECK (email <> '' AND char_length(email) <= 254),
    kind          text        CHECK (kind IN ('webhook', 'slack', 'teams', 'discord')),
    settings      jsonb,
    secret        text,
    -- Chosen by someone other than a global administrator: delivered only
    -- to public addresses, like a channel (ADR-0078).
    public_only   boolean     NOT NULL DEFAULT true,
    updated_at    timestamptz NOT NULL DEFAULT now(),
    last_sent_at  timestamptz,
    last_error    text,
    last_error_at timestamptz,
    CHECK ((kind IS NULL) = (settings IS NULL)),
    CHECK (kind IS NOT NULL OR secret IS NULL)
);
COMMENT ON TABLE user_contacts IS
    'Where personal notifications reach a user: an email address and an optional chat or webhook. See ADR-0081.';

-- A rotation for everything, a customer or a site, like a channel's
-- coverage. Hand-offs happen at `handoff_time` in `time_zone`, every day or
-- every week on the weekday of `anchor_date`. The member at
-- `anchor_position` took over at the hand-off of `anchor_date`; the others
-- follow in order. Edits move the anchor so that who is on call now stays.
CREATE TABLE on_call_schedules (
    id              uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    name            text        NOT NULL CHECK (name <> '' AND char_length(name) <= 80),
    customer_id     uuid        REFERENCES customers (id) ON DELETE CASCADE,
    site_id         uuid        REFERENCES sites (id) ON DELETE CASCADE,
    -- An IANA name, like Europe/Paris; checked by the API.
    time_zone       text        NOT NULL CHECK (time_zone <> ''),
    rotation        text        NOT NULL CHECK (rotation IN ('daily', 'weekly')),
    handoff_time    time        NOT NULL,
    anchor_date     date        NOT NULL,
    anchor_position integer     NOT NULL DEFAULT 0 CHECK (anchor_position >= 0),
    created_by      uuid        REFERENCES users (id) ON DELETE SET NULL,
    created_at      timestamptz NOT NULL DEFAULT now(),
    CHECK (customer_id IS NULL OR site_id IS NULL)
);
COMMENT ON TABLE on_call_schedules IS
    'Who is on call for everything, a customer or a site, in turn. See ADR-0081.';

-- The turns, in order. A removed user leaves an empty turn rather than
-- shifting everyone else's: the schedule shows it as a gap.
CREATE TABLE on_call_members (
    schedule_id uuid    NOT NULL REFERENCES on_call_schedules (id) ON DELETE CASCADE,
    position    integer NOT NULL CHECK (position >= 0),
    user_id     uuid    REFERENCES users (id) ON DELETE SET NULL,
    PRIMARY KEY (schedule_id, position)
);
CREATE INDEX on_call_members_user_idx ON on_call_members (user_id);

-- Someone covers a period instead of the rotation: a holiday, a swap. The
-- newest wins where several overlap.
CREATE TABLE on_call_overrides (
    id          uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    schedule_id uuid        NOT NULL REFERENCES on_call_schedules (id) ON DELETE CASCADE,
    user_id     uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    starts_at   timestamptz NOT NULL,
    ends_at     timestamptz NOT NULL,
    created_by  uuid        REFERENCES users (id) ON DELETE SET NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    CHECK (ends_at > starts_at),
    CHECK (ends_at <= starts_at + interval '90 days')
);
CREATE INDEX on_call_overrides_schedule_idx ON on_call_overrides (schedule_id, ends_at);

-- A row for one person only: an owed message (ADR-0079) that their chat
-- destination, still limiting, did not take, copied for the next pass.
ALTER TABLE notification_outbox
    ADD COLUMN user_id uuid REFERENCES users (id) ON DELETE CASCADE,
    ADD CONSTRAINT notification_outbox_one_target
        CHECK (channel_id IS NULL OR user_id IS NULL);
DROP INDEX notification_outbox_one_backup_outage_key;
CREATE UNIQUE INDEX notification_outbox_one_backup_outage_key ON notification_outbox
    ((coalesce(details #>> '{backup,last_success_at}', '')))
    WHERE event = 'backup.failed' AND channel_id IS NULL AND user_id IS NULL;
